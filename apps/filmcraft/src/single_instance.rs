//! One running FilmCraft per data folder (LINUX-PLAN L1.7): opening a project from the file
//! manager, or starting FilmCraft again, hands the files to the window that is already open
//! instead of starting a second app on the same auto-save and settings.
//!
//! The first instance owns a session D-Bus name (`io.github.dawgon2.Revolution` for the default data
//! folder; another data folder gets its own name, so the fork's `--data-dir` launcher and a
//! packaged install don't meet) and serves `ai.storyteller.filmcraft.Instance.Open(files,
//! activation_token)` at `/ai/storyteller/filmcraft`. A later launch finds the name taken, calls
//! `Open` with its files (made absolute: the two processes have different working folders) and
//! exits. The running app opens / imports them as commands and asks for the window to come
//! forward (on Wayland the taskbar entry asks for attention instead; the token is passed for
//! when winit can use it).
//!
//! Skipped (a normal independent start) with `--control` (agents and tests run many instances),
//! `--new-instance`, and when there is no session bus. Linux only for now; elsewhere every start
//! is independent, as before.

use std::path::{Path, PathBuf};

/// The bus name for a data folder: the plain app id for the default folder, else the app id plus
/// a hash of the folder (bus name elements can't hold arbitrary characters).
pub fn bus_name(data_dir: &Path, is_default: bool) -> String {
    if is_default {
        return crate::desktop::DESKTOP_ID.to_string();
    }
    // FNV-1a over the path bytes: stable across runs and builds (std's hasher is not)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data_dir.as_os_str().as_encoded_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}.d{h:016x}", crate::desktop::DESKTOP_ID)
}

/// `files` made absolute against `cwd` (relative names from a terminal or a launcher).
pub fn absolute(files: &[String], cwd: Option<&Path>) -> Vec<String> {
    files
        .iter()
        .filter(|f| !f.is_empty())
        .map(|f| {
            let p = Path::new(f);
            match cwd {
                Some(c) if p.is_relative() => c.join(p).to_string_lossy().into_owned(),
                _ => f.clone(),
            }
        })
        .collect()
}

/// The object path the instance service lives at.
pub const OBJECT_PATH: &str = "/ai/storyteller/filmcraft";

/// What start-up should do.
pub enum Start {
    /// Run normally. Holds the D-Bus connection that owns the name (kept alive for the whole run),
    /// if the name could be claimed.
    Run(Option<Holder>),
    /// Another instance took the files: exit now.
    HandedOff,
}

/// Keeps the instance name owned while alive.
pub struct Holder {
    #[cfg(target_os = "linux")]
    _conn: zbus::blocking::Connection,
}

/// What a later launch hands to the running instance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HandOff {
    /// Absolute paths of projects / media to open.
    pub files: Vec<String>,
    /// `XDG_ACTIVATION_TOKEN` of the launch (empty without one).
    pub token: String,
    /// `--empty` (the desktop file's "New Project" action): start a new project there.
    pub new_project: bool,
}

/// Claim the instance name for `data_dir`, or hand `files` (and `--empty`) to the instance that
/// has it.
pub fn claim(data_dir: Option<&Path>, files: &[String], new_project: bool, shared: &std::sync::Arc<crate::desktop::Shared>) -> Start {
    #[cfg(target_os = "linux")]
    {
        let Some(dir) = data_dir else { return Start::Run(None) };
        let default = filmcraft_engine::autosave::default_data_dir().is_some_and(|d| d == dir) && std::env::var_os("FILMCRAFT_DATA_DIR").is_none();
        let name = bus_name(dir, default);
        let cwd: Option<PathBuf> = std::env::current_dir().ok();
        let files = absolute(files, cwd.as_deref());
        let token = std::env::var("XDG_ACTIVATION_TOKEN").unwrap_or_default();
        linux::claim(&name, HandOff { files, token, new_project }, shared)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (data_dir, files, new_project, shared);
        Start::Run(None)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Arc;
    use std::time::Duration;

    use filmcraft_ui_egui::desktop::DesktopRequest;
    use zbus::blocking::Connection;
    use zbus::fdo::{RequestNameFlags, RequestNameReply};

    use super::{HandOff, Holder, OBJECT_PATH, Start};
    use crate::desktop::Shared;

    /// At most this many files per hand-off (a launcher bug can't flood the running app).
    const MAX_FILES: usize = 4096;

    struct Instance {
        shared: Arc<Shared>,
    }

    #[zbus::interface(name = "ai.storyteller.filmcraft.Instance")]
    impl Instance {
        /// Open `files` (projects are opened, media imported) and bring the window forward.
        fn open(&self, files: Vec<String>, activation_token: String) -> zbus::fdo::Result<()> {
            let _ = activation_token; // winit 0.30 can't activate an existing window with a foreign token
            self.refuse_while_closing()?;
            let files: Vec<String> = files.into_iter().take(MAX_FILES).collect();
            self.shared.push(if files.is_empty() { DesktopRequest::Raise } else { DesktopRequest::Open(files) });
            Ok(())
        }

        /// Start a new project (the desktop file's "New Project" action) and bring the window forward.
        fn new_project(&self, activation_token: String) -> zbus::fdo::Result<()> {
            let _ = activation_token;
            self.refuse_while_closing()?;
            self.shared.push(DesktopRequest::Command("file.newProject".into(), serde_json::json!({})));
            self.shared.push(DesktopRequest::Raise);
            Ok(())
        }
    }

    impl Instance {
        /// A closing app takes no files (the launch then starts its own window).
        fn refuse_while_closing(&self) -> zbus::fdo::Result<()> {
            if self.shared.is_closing() { Err(zbus::fdo::Error::Failed("Revolution is closing".into())) } else { Ok(()) }
        }
    }

    pub fn claim(name: &str, handoff: HandOff, shared: &Arc<Shared>) -> Start {
        let conn = match zbus::blocking::connection::Builder::session().and_then(|b| b.method_timeout(Duration::from_secs(5)).build()) {
            Ok(c) => c,
            Err(e) => {
                log::info!("single instance off: no session D-Bus ({e})");
                return Start::Run(None);
            }
        };
        if let Err(e) = conn.object_server().at(OBJECT_PATH, Instance { shared: Arc::clone(shared) }) {
            log::info!("single instance off: {e}");
            return Start::Run(None);
        }
        match conn.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Start::Run(Some(Holder { _conn: conn })),
            // zbus reports a taken name as `Error::NameTaken` (with DoNotQueue), some buses as `Exists`
            Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => hand_off(&conn, name, handoff),
            Err(e) => {
                log::info!("single instance off: {e}");
                Start::Run(None)
            }
        }
    }

    /// Give the files to the instance that owns `name`. If it doesn't answer (hung, quitting),
    /// start normally rather than lose the files.
    fn hand_off(conn: &Connection, name: &str, h: HandOff) -> Start {
        let sent = zbus::blocking::Proxy::new(conn, name.to_string(), OBJECT_PATH, "ai.storyteller.filmcraft.Instance").and_then(|p| {
            if h.new_project {
                p.call::<_, _, ()>("NewProject", &(h.token.as_str(),))?;
                if h.files.is_empty() {
                    return Ok(());
                }
            }
            p.call::<_, _, ()>("Open", &(h.files, h.token.as_str()))
        });
        match sent {
            Ok(()) => {
                log::info!("Revolution is already running: the files went to that window");
                Start::HandedOff
            }
            Err(e) => {
                #[cfg(test)]
                {
                    use std::io::Write;
                    let _ = writeln!(std::io::stderr(), "hand-off failed: {e}");
                }
                log::warn!("the running Revolution didn't answer ({e}); starting another one");
                Start::Run(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_names_are_valid_and_stable() {
        assert_eq!(bus_name(Path::new("/x"), true), "io.github.dawgon2.Revolution");
        let a = bus_name(Path::new("/home/u/.local/share/filmcraft-fork"), false);
        assert_eq!(a, bus_name(Path::new("/home/u/.local/share/filmcraft-fork"), false));
        assert_ne!(a, bus_name(Path::new("/home/u/.local/share/filmcraft-other"), false));
        // D-Bus: elements of [A-Za-z0-9_-], not starting with a digit, ≤ 255 bytes
        assert!(a.len() <= 255);
        for el in a.split('.') {
            assert!(!el.is_empty() && !el.starts_with(|c: char| c.is_ascii_digit()), "{a}");
            assert!(el.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{a}");
        }
        // hostile folder names still give a valid name
        let weird = bus_name(Path::new("/tmp/\u{0}/../ä b.c"), false);
        assert!(weird.starts_with("io.github.dawgon2.Revolution.d") && weird.len() == "io.github.dawgon2.Revolution.d".len() + 16);
    }

    #[test]
    fn relative_files_are_made_absolute() {
        let cwd = Path::new("/work");
        let got = absolute(&["a.mov".into(), "/abs/b.fcproj".into(), String::new(), "../c.mov".into()], Some(cwd));
        assert_eq!(got, vec!["/work/a.mov".to_string(), "/abs/b.fcproj".into(), "/work/../c.mov".into()]);
        assert_eq!(absolute(&["a.mov".into()], None), vec!["a.mov".to_string()]);
    }

    /// End to end on a private session bus (skipped without one: `dbus-run-session` provides it).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_second_claim_hands_its_files_to_the_first() {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            eprintln!("skipped: no session bus");
            return;
        }
        let name = format!("ai.storyteller.filmcraft.test{}", std::process::id());
        let first = std::sync::Arc::new(crate::desktop::Shared::default());
        let Start::Run(Some(holder)) = linux::claim(&name, HandOff::default(), &first) else { panic!("the first claim owns the name") };
        first.keep(Some(holder));
        let second = std::sync::Arc::new(crate::desktop::Shared::default());
        let files = vec!["/v/a.mov".to_string(), "/v/film.fcproj".into()];
        let got = linux::claim(&name, HandOff { files, token: "tok".into(), new_project: false }, &second);
        assert!(matches!(got, Start::HandedOff));
        let requests = first.take();
        assert_eq!(requests, vec![filmcraft_ui_egui::desktop::DesktopRequest::Open(vec!["/v/a.mov".into(), "/v/film.fcproj".into()])]);
        assert!(second.take().is_empty());
        // no files: just raise
        assert!(matches!(linux::claim(&name, HandOff::default(), &second), Start::HandedOff));
        assert_eq!(first.take(), vec![filmcraft_ui_egui::desktop::DesktopRequest::Raise]);
        // "New Project" action
        assert!(matches!(linux::claim(&name, HandOff { new_project: true, ..HandOff::default() }, &second), Start::HandedOff));
        assert_eq!(
            first.take(),
            vec![
                filmcraft_ui_egui::desktop::DesktopRequest::Command("file.newProject".into(), serde_json::json!({})),
                filmcraft_ui_egui::desktop::DesktopRequest::Raise
            ]
        );
        // once the first one closes, a launch starts its own window instead of losing its files
        first.close();
        let got = linux::claim(&name, HandOff { files: vec!["/v/b.mov".into()], ..HandOff::default() }, &second);
        assert!(matches!(got, Start::Run(_)), "a closed instance takes no hand-off");
        assert!(first.take().is_empty());
    }
}
