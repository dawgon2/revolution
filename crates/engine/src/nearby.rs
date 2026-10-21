//! Sending a project to a nearby computer without a server (STORAGE-PLAN S2b items 4–5,
//! ADR 0008). The protocol lives in `filmcraft_remote::nearby` (feature `nearby`, on with
//! `remote-net`); these are its commands.
//!
//! `nearby.receive` starts listening and replies with the code to show; a job waits for a
//! sender. When an offer arrives `nearby.status` shows who sends what, and `nearby.accept` /
//! `nearby.decline` answer it (nothing is written before Accept; no answer within two minutes
//! declines). The job ends with the received `.revpkg` in `<data>/remote/inbox/nearby/`, which
//! is then imported with `projects.importPackage` (with `"wait": true` the command does both).
//! `nearby.send` packages a project (or takes a `.revpkg`) and sends it to the receiver with
//! that code, found on the network or at a typed address. `nearby.list` lists the computers
//! waiting to receive. `nearby.stop` ends a receive. `nearby.wait` blocks until a receive ends
//! (accepting with `"accept": true`) and imports it: for scripts.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{Result, Session};

/// The receiver's standard TCP port (FORK-PLAN §1a.118), named by the packaged firewall
/// descriptions; the same as `filmcraft_remote::nearby::DEFAULT_PORT` (a test checks).
pub const DEFAULT_PORT: u16 = 47821;

/// What the receive job shares with the commands (and the UI through `nearby.status`).
#[derive(Clone, Debug, Default)]
pub struct NearbyShared {
    /// The running receive job.
    pub job: Option<u64>,
    pub code: String,
    pub port: u16,
    pub warnings: Vec<String>,
    /// An offer waiting for Accept / Decline: (computer, package name, bytes, resumed).
    pub offer: Option<(String, String, u64, bool)>,
    /// The answer to `offer` (`true` = accept).
    pub decision: Option<bool>,
    /// A line for the receive window ("Someone tried a wrong code. Here is a new one.").
    pub note: String,
}

/// The shared receive state of a session.
pub type Nearby = Arc<Mutex<NearbyShared>>;

fn shared(s: &Session) -> std::sync::MutexGuard<'_, NearbyShared> {
    s.nearby.lock().unwrap_or_else(PoisonError::into_inner)
}

fn running(s: &Session) -> Option<crate::Job> {
    let id = shared(s).job?;
    s.jobs.iter().find(|j| j.id == id && j.result.lock().unwrap_or_else(PoisonError::into_inner).is_none()).cloned()
}

fn status_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    let active = running(s).is_some();
    let n = shared(s).clone();
    let offer = n.offer.map(|(computer, package, bytes, resumed)| json!({"computer": computer, "package": package, "bytes": bytes, "resumed": resumed}));
    Ok(json!({"active": active, "job": n.job, "code": n.code, "port": n.port, "warnings": n.warnings, "offer": offer, "note": n.note}))
}

fn answer(s: &mut Session, yes: bool, cmd: &str) -> Result<Value> {
    if running(s).is_none() {
        return Err(bad(cmd, "nothing is being received"));
    }
    let mut n = shared(s);
    if n.offer.is_none() {
        return Err(bad(cmd, "no transfer is waiting for an answer"));
    }
    n.decision = Some(yes);
    Ok(json!({"accepted": yes}))
}

fn stop_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    let job = running(s);
    if let Some(j) = &job {
        j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(json!({"stopped": job.is_some()}))
}

#[cfg(feature = "remote-net")]
mod net {
    use std::path::{Path, PathBuf};
    use std::sync::PoisonError;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use web_time::Instant;

    use filmcraft_remote::nearby::{self, Code, NearbyError, Receiver, ReceiverOptions, SendOptions, Target};
    use serde_json::{Value, json};

    use super::{Nearby, running};
    use crate::commands::{bad, bool_p, str_p, u64_p};
    use crate::remote_transfer::{data_folder, finish, new_job, report, status, wait_for};
    use crate::{Job, Result, Session};

    /// Wrong codes in a row before a receive gives up (each one gets a new code).
    const MAX_RENEWALS: u32 = 5;

    fn options(p: &Value) -> ReceiverOptions {
        let mut o = ReceiverOptions { port: u64_p(p, "port").and_then(|v| u16::try_from(v).ok()).unwrap_or(0), ..Default::default() };
        // tests on one machine turn the network discovery off (`"discovery": false`)
        if bool_p(p, "discovery") == Some(false) {
            o.mdns = false;
            o.probe_port = None;
        }
        o
    }

    /// Starts listening on `opts.port`; when that port is taken (another program, or a second
    /// Revolution receiving), on any free port instead, with a line saying so.
    fn start_receiver(opts: &ReceiverOptions) -> std::result::Result<(Receiver, Option<String>), NearbyError> {
        match Receiver::start(opts.clone()) {
            Ok(rx) => Ok((rx, None)),
            Err(e) if opts.port != 0 => {
                let any = ReceiverOptions { port: 0, ..opts.clone() };
                let rx = Receiver::start(any).map_err(|_| e)?;
                let line = format!(
                    "Port {} is in use, so this computer listens on port {} instead. A firewall rule for port {} won't cover it.",
                    opts.port,
                    rx.port(),
                    opts.port
                );
                Ok((rx, Some(line)))
            }
            Err(e) => Err(e),
        }
    }

    fn publish(n: &Nearby, rx: &Receiver, moved: Option<String>) {
        let mut g = n.lock().unwrap_or_else(PoisonError::into_inner);
        g.code = rx.code().to_string();
        g.port = rx.port();
        g.warnings = moved.into_iter().chain(rx.warnings().iter().cloned()).collect();
        g.offer = None;
        g.decision = None;
    }

    fn set(n: &Nearby, f: impl FnOnce(&mut super::NearbyShared)) {
        f(&mut n.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The receive job: offers, Accept / Decline, the transfer, resumes after a drop.
    fn receive_loop(mut rx: Receiver, opts: ReceiverOptions, n: &Nearby, job: &Job, inbox: &Path) -> std::result::Result<PathBuf, String> {
        let cancel = &job.progress.cancel;
        let prompt = opts.timeouts.accept_prompt;
        let mut renewals = 0u32;
        loop {
            status(job, "Waiting for the other computer…");
            let offer = match rx.wait_offer(cancel, opts.timeouts.code_lifetime) {
                Ok(o) => o,
                Err(NearbyError::WrongCode) if renewals < MAX_RENEWALS => {
                    renewals = renewals.saturating_add(1);
                    // the old one lets go of its (fixed) port first
                    drop(rx);
                    let (fresh, moved) = start_receiver(&opts).map_err(|e| e.to_string())?;
                    rx = fresh;
                    publish(n, &rx, moved);
                    set(n, |g| g.note = "Someone tried a wrong code. Here is a new one.".into());
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
            let resumed = offer.is_resume;
            set(n, |g| {
                g.offer = Some((offer.computer.clone(), offer.package_name.clone(), offer.size, resumed));
                g.decision = None;
            });
            // a resumed transfer was accepted already
            let decision = if resumed {
                Some(true)
            } else {
                status(job, "Waiting for you to accept…");
                let until = Instant::now().checked_add(prompt).unwrap_or_else(Instant::now);
                loop {
                    let d = n.lock().unwrap_or_else(PoisonError::into_inner).decision;
                    if d.is_some() || cancel.load(Ordering::Relaxed) || Instant::now() >= until {
                        break d;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            };
            match decision {
                Some(true) => {
                    status(job, "Receiving…");
                    let prog = &job.progress;
                    let mut on = |done: u64, total: u64| {
                        prog.total.store(total, Ordering::Relaxed);
                        prog.done.store(done, Ordering::Relaxed);
                    };
                    match offer.accept(inbox, &mut on, cancel) {
                        Ok(path) => {
                            set(n, |g| g.offer = None);
                            return Ok(path);
                        }
                        Err(e) if e.is_resumable() => {
                            set(n, |g| {
                                g.offer = None;
                                g.note = format!("{e} Waiting for the other computer to continue.");
                            });
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Some(false) => {
                    let _ = offer.decline();
                    return Err("You declined the transfer.".into());
                }
                None if cancel.load(Ordering::Relaxed) => {
                    let _ = offer.decline();
                    return Err(NearbyError::Cancelled.to_string());
                }
                None => {
                    let _ = offer.decline();
                    return Err("No answer within two minutes, so the transfer was declined.".into());
                }
            }
        }
    }

    pub(super) fn receive(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "nearby.receive";
        if running(s).is_some() {
            return super::status_cmd(s, p);
        }
        let mut opts = options(p);
        if p.get("port").is_none() {
            // 0 in Settings = the standard port the packaged firewall rules name
            opts.port = match s.prefs.remote.nearby_port {
                0 => super::DEFAULT_PORT,
                port => port,
            };
        }
        let (rx, moved) = start_receiver(&opts).map_err(|e| bad(CMD, e.to_string()))?;
        let inbox = data_folder(s).join("remote").join("inbox").join("nearby");
        std::fs::create_dir_all(&inbox).map_err(|e| bad(CMD, format!("{}: {e}", inbox.display())))?;
        let job = new_job(s, "Receive from a nearby computer".into());
        let n = s.nearby.clone();
        *n.lock().unwrap_or_else(PoisonError::into_inner) = super::NearbyShared { job: Some(job.id), ..Default::default() };
        publish(&n, &rx, moved);
        let code = rx.code().to_string();
        let port = rx.port();
        let warnings = n.lock().unwrap_or_else(PoisonError::into_inner).warnings.clone();
        let run = {
            let (job, n) = (job.clone(), n.clone());
            move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| receive_loop(rx, opts, &n, &job, &inbox)))
                    .unwrap_or_else(|_| Err("the receive stopped unexpectedly".into()));
                let r = r.map(|path| {
                    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    report(&path.to_string_lossy(), bytes)
                });
                set(&n, |g| g.offer = None);
                finish(&job, r);
            }
        };
        if !bool_p(p, "wait").unwrap_or(false) {
            std::thread::Builder::new().name("filmcraft-nearby-receive".into()).spawn(run).map_err(|e| bad(CMD, e.to_string()))?;
            return Ok(json!({"job": job.id, "code": code, "port": port, "warnings": warnings}));
        }
        // `wait` (scripts): someone must answer the offer from another thread, or `"accept": true`
        if bool_p(p, "accept") == Some(true) {
            let n = n.clone();
            std::thread::spawn(move || {
                for _ in 0..12_000 {
                    {
                        let mut g = n.lock().unwrap_or_else(PoisonError::into_inner);
                        if g.offer.is_some() {
                            g.decision = Some(true);
                            return;
                        }
                        if g.job.is_none() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
        run();
        let res = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let path = match res {
            Some(Ok(r)) => r.path,
            Some(Err(e)) => return Err(bad(CMD, e)),
            None => return Err(bad(CMD, "the receive did not finish")),
        };
        let mut q = json!({"path": path, "wait": true});
        if let Some(f) = p.get("folder") {
            q["folder"] = f.clone();
        }
        let imported = s.execute("projects.importPackage", q)?;
        let _ = std::fs::remove_file(&path);
        Ok(json!({"imported": imported, "job": job.id, "code": code}))
    }

    /// `nearby.wait`: waits (≤ `timeoutS`, default and at most an hour) for the running receive
    /// to end, accepting its offer when `accept` is set, then imports the package into `folder`.
    /// For scripts: `nearby.receive` prints the code, then `nearby.wait` blocks.
    pub(super) fn wait(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "nearby.wait";
        let Some(job) = running(s) else {
            return Err(bad(CMD, "nothing is being received (run nearby.receive first)"));
        };
        let accept = bool_p(p, "accept").unwrap_or(false);
        let secs = u64_p(p, "timeoutS").unwrap_or(3600).clamp(1, 3600);
        let until = Instant::now().checked_add(Duration::from_secs(secs)).unwrap_or_else(Instant::now);
        let res = loop {
            if let Some(r) = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                break r;
            }
            if Instant::now() >= until {
                return Err(bad(CMD, "still waiting (the receive goes on: run nearby.wait again)"));
            }
            if accept {
                let mut g = s.nearby.lock().unwrap_or_else(PoisonError::into_inner);
                if g.offer.is_some() && g.decision.is_none() {
                    g.decision = Some(true);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let r = res.map_err(|e| bad(CMD, e))?;
        // `"import": false`: just the checked package (its path), e.g. for a transfer test
        if bool_p(p, "import") == Some(false) {
            return Ok(json!({"package": r.path, "bytes": r.bytes, "job": job.id}));
        }
        let path = r.path;
        let mut q = json!({"path": path, "wait": true});
        if let Some(f) = p.get("folder") {
            q["folder"] = f.clone();
        }
        let imported = s.execute("projects.importPackage", q)?;
        let _ = std::fs::remove_file(&path);
        Ok(json!({"imported": imported, "job": job.id}))
    }

    pub(super) fn send(s: &mut Session, p: &Value) -> Result<Value> {
        const CMD: &str = "nearby.send";
        let code = Code::parse(str_p(p, "code").unwrap_or("")).map_err(|e| bad(CMD, e.to_string()))?;
        let path = str_p(p, "path").ok_or_else(|| bad(CMD, "need `path` (the project file or a .revpkg)"))?.to_string();
        let address = str_p(p, "address").map(str::to_string);
        let wait = bool_p(p, "wait").unwrap_or(false);
        let name = Path::new(&path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // a project is packaged first (into the outbox, removed afterwards); a package goes as it is
        let (file, outbox, packaging) = if crate::package::is_package_file(Path::new(&path)) {
            (PathBuf::from(&path), None, None)
        } else {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
            let outbox = data_folder(s).join("remote").join("outbox").join(format!("nearby-{nonce}"));
            std::fs::create_dir_all(&outbox).map_err(|e| bad(CMD, format!("{}: {e}", outbox.display())))?;
            let pk = s.execute(
                "projects.package",
                json!({"path": path, "destination": outbox.to_string_lossy(), "format": "file", "proxies": bool_p(p, "proxies").unwrap_or(false), "trim": bool_p(p, "trim").unwrap_or(false), "wait": wait}),
            );
            let pk = match pk {
                Ok(v) => v,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&outbox);
                    return Err(e);
                }
            };
            let packaging = pk["job"].as_u64().and_then(|id| s.jobs.iter().find(|j| j.id == id).cloned());
            (PathBuf::from(pk["package"].as_str().unwrap_or_default()), Some(outbox), packaging)
        };
        let job = new_job(s, format!("Send {name} to a nearby computer"));
        let discovery = bool_p(p, "discovery") != Some(false);
        let run = {
            let job = job.clone();
            move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<filmcraft_export::Report, String> {
                    if let Some(pj) = &packaging {
                        status(&job, "Packaging…");
                        wait_for(pj, &job.progress)?;
                    }
                    let target = match address {
                        Some(a) => Target::Address(a),
                        None => {
                            status(&job, "Looking for the other computer…");
                            let found = if discovery { nearby::discover(code.channel(), Duration::from_secs(3)) } else { Vec::new() };
                            Target::Candidates(found)
                        }
                    };
                    status(&job, "Sending…");
                    let prog = &job.progress;
                    let mut on = |done: u64, total: u64| {
                        prog.total.store(total, Ordering::Relaxed);
                        prog.done.store(done, Ordering::Relaxed);
                    };
                    let me = crate::remote_project::machine_name();
                    match nearby::send(&code, &target, &file, &me, &mut on, &prog.cancel, &SendOptions::default()) {
                        Ok(out) => {
                            let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                            Ok(report(&out.addr.to_string(), bytes))
                        }
                        Err(NearbyError::Ambiguous(c)) => {
                            let list: Vec<String> = c.iter().map(|c| c.addr.to_string()).collect();
                            Err(format!(
                                "{} computers are waiting with this channel number. Send again with the address of the right one: {}",
                                list.len(),
                                list.join(", ")
                            ))
                        }
                        Err(e) => Err(e.to_string()),
                    }
                }))
                .unwrap_or_else(|_| Err("the send stopped unexpectedly".into()));
                if let Some(o) = &outbox {
                    let _ = std::fs::remove_dir_all(o);
                }
                finish(&job, r);
            }
        };
        if wait {
            run();
            let res = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone();
            return match res {
                Some(Ok(r)) => Ok(json!({"sent": name, "to": r.path, "bytes": r.bytes, "job": job.id})),
                Some(Err(e)) => Err(bad(CMD, e)),
                None => Err(bad(CMD, "the send did not finish")),
            };
        }
        std::thread::Builder::new().name("filmcraft-nearby-send".into()).spawn(run).map_err(|e| bad(CMD, e.to_string()))?;
        Ok(json!({"job": job.id}))
    }

    pub(super) fn list(_: &mut Session, p: &Value) -> Result<Value> {
        let ms = u64_p(p, "timeoutMs").unwrap_or(1500).clamp(100, 5000);
        let found = nearby::list_receivers(Duration::from_millis(ms));
        let v: Vec<Value> = found.iter().take(nearby::MAX_CANDIDATES).map(|c| json!({"channel": c.channel, "address": c.addr.to_string()})).collect();
        Ok(json!({"receivers": v}))
    }
}

#[cfg(not(feature = "remote-net"))]
mod net {
    use serde_json::Value;

    use crate::commands::bad;
    use crate::{Result, Session};

    const OFF: &str = "sending to a nearby computer is not in this build";

    pub(super) fn receive(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("nearby.receive", OFF))
    }
    pub(super) fn send(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("nearby.send", OFF))
    }
    pub(super) fn list(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("nearby.list", OFF))
    }
    pub(super) fn wait(_: &mut Session, _: &Value) -> Result<Value> {
        Err(bad("nearby.wait", OFF))
    }
}

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("nearby.receive", "Receive from a Nearby Computer", r#"{"port":int?,"folder":str?,"wait":bool?,"accept":bool?,"discovery":bool?}"#, net::receive),
        spec(
            "nearby.send",
            "Send to a Nearby Computer",
            r#"{"code":str,"path":str,"address":str?,"proxies":bool?,"trim":bool?,"wait":bool?,"discovery":bool?}"#,
            net::send,
        ),
        spec("nearby.list", "List Nearby Computers Waiting to Receive", r#"{"timeoutMs":int?}"#, net::list),
        spec("nearby.wait", "Wait for the Nearby Transfer", r#"{"accept":bool?,"folder":str?,"import":bool?,"timeoutS":int?}"#, net::wait),
        spec("nearby.status", "Nearby Receive Status", "{}", status_cmd),
        spec("nearby.accept", "Accept the Nearby Transfer", "{}", |s, _| answer(s, true, "nearby.accept")),
        spec("nearby.decline", "Decline the Nearby Transfer", "{}", |s, _| answer(s, false, "nearby.decline")),
        spec("nearby.stop", "Stop Receiving from a Nearby Computer", "{}", stop_cmd),
    ]
}
