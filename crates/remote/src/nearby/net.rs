//! TCP on top of the protocol core: the listening [`Receiver`] and [`send`].

use super::discovery::{self, Advert, Candidate, PROBE_PORT};
use super::transfer::{self, Package, Pending, Served};
use super::wire::{self, POLL, ROLE_BUSY, ROLE_GONE, Stream};
use super::{Code, NearbyError, Progress, Result, Timeouts, spawn_guarded};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A sender tries at most this many receivers.
pub const MAX_SEND_CANDIDATES: usize = 8;

/// The longest typed address.
const MAX_ADDRESS_LEN: usize = 255;

/// Receiver settings.
#[derive(Clone, Debug)]
pub struct ReceiverOptions {
    /// TCP port (0 = any free port; the `nearby.port` setting for strict firewalls).
    pub port: u16,
    /// UDP port answering broadcast probes (`None` = don't answer; normally [`PROBE_PORT`]).
    pub probe_port: Option<u16>,
    /// Advertise over mDNS / DNS-SD.
    pub mdns: bool,
    pub timeouts: Timeouts,
}

impl Default for ReceiverOptions {
    fn default() -> Self {
        ReceiverOptions { port: 0, probe_port: Some(PROBE_PORT), mdns: true, timeouts: Timeouts::default() }
    }
}

/// State shared by the receiver, its worker threads and its pending offer.
pub(crate) struct Shared {
    pub stop: AtomicBool,
    busy: AtomicBool,
    burned: AtomicBool,
    expires: Instant,
    accepted_sha: Mutex<Option<[u8; 32]>>,
}

impl Shared {
    pub fn usable(&self) -> bool {
        !self.burned.load(Ordering::Relaxed) && Instant::now() < self.expires
    }
    fn burn(&self) {
        self.burned.store(true, Ordering::Relaxed);
    }
}

/// Clears the busy flag when the current peer is done with.
struct BusyGuard(Arc<Shared>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Relaxed);
    }
}

/// A receiver waiting for a sender: listening on TCP, advertised on the network, showing its
/// [`Code`]. Dropping it stops everything.
pub struct Receiver {
    code: Code,
    port: u16,
    shared: Arc<Shared>,
    conns: Mutex<mpsc::Receiver<TcpStream>>,
    threads: Vec<JoinHandle<()>>,
    advert: Option<Advert>,
    warnings: Vec<String>,
    timeouts: Timeouts,
}

impl Receiver {
    /// Picks a new code, opens the TCP port on all interfaces and advertises it.
    /// Discovery problems don't fail the start; they are listed in [`Receiver::warnings`].
    pub fn start(opts: ReceiverOptions) -> Result<Receiver> {
        let code = Code::generate()?;
        let listener =
            TcpListener::bind(("0.0.0.0", opts.port)).map_err(|e| NearbyError::Io(format!("Could not open port {} for receiving: {e}", opts.port)))?;
        let port = listener.local_addr().map_err(|e| NearbyError::Io(e.to_string()))?.port();
        listener.set_nonblocking(true).map_err(|e| NearbyError::Io(e.to_string()))?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            burned: AtomicBool::new(false),
            expires: super::after(opts.timeouts.code_lifetime),
            accepted_sha: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel();
        let mut threads = Vec::new();
        let sh = shared.clone();
        threads.push(spawn_guarded("nearby-listen", move || listen_loop(&listener, &sh, &tx))?);
        let mut warnings = Vec::new();
        if let Some(probe) = opts.probe_port {
            match UdpSocket::bind(("0.0.0.0", probe)) {
                Ok(sock) => {
                    let sh = shared.clone();
                    let channel = code.channel();
                    match spawn_guarded("nearby-probe", move || discovery::answer_probes(&sock, channel, port, &sh)) {
                        Ok(h) => threads.push(h),
                        Err(e) => warnings.push(e.to_string()),
                    }
                }
                Err(e) => warnings.push(format!("broadcast discovery is off (UDP port {probe}: {e})")),
            }
        }
        let advert = if opts.mdns {
            match Advert::start(code.channel(), port) {
                Ok(a) => Some(a),
                Err(e) => {
                    warnings.push(format!("mDNS discovery is off ({e})"));
                    None
                }
            }
        } else {
            None
        };
        let mut r = Receiver { code, port, shared, conns: Mutex::new(rx), threads, advert, warnings, timeouts: opts.timeouts };
        r.threads.shrink_to_fit();
        Ok(r)
    }

    /// The code to show.
    pub fn code(&self) -> Code {
        self.code
    }

    /// The TCP port (for "Enter the other computer's address" and firewall help).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Discovery that couldn't start (the sender can still type the address).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whether the code can still be used (not burned, not expired).
    pub fn is_usable(&self) -> bool {
        self.shared.usable()
    }

    /// Waits up to `timeout` for a sender to connect, prove it has the code and offer a
    /// package. Strangers and broken connections are ignored (it keeps waiting). A wrong code
    /// returns [`NearbyError::WrongCode`] and burns this code: start a new `Receiver`.
    /// After a dropped transfer, call it again to let the sender resume.
    pub fn wait_offer(&self, cancel: &AtomicBool, timeout: Duration) -> Result<IncomingOffer> {
        let until = super::after(timeout);
        let rx = self.conns.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let stop = if cancel.load(Ordering::Relaxed) {
                Some(NearbyError::Cancelled)
            } else if !self.shared.usable() {
                self.shared.burn();
                Some(NearbyError::Expired)
            } else if Instant::now() >= until {
                Some(NearbyError::Timeout)
            } else {
                None
            };
            if let Some(e) = stop {
                self.drop_queued(&rx);
                return Err(e);
            }
            let stream = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(s) => s,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(NearbyError::Io("the receiver stopped listening".into())),
            };
            let guard = BusyGuard(self.shared.clone());
            if prepare(&stream, &self.timeouts).is_err() {
                continue;
            }
            let resume = *self.shared.accepted_sha.lock().unwrap_or_else(PoisonError::into_inner);
            match transfer::serve(Box::new(stream), &self.code, &self.timeouts, resume, cancel) {
                Ok(Served::Offer(pending)) => {
                    let pending = *pending;
                    let is_resume = resume == Some(pending.sha);
                    return Ok(IncomingOffer::new(pending, Some(guard), is_resume, self.timeouts));
                }
                Ok(Served::DeclinedOther) => continue,
                Err(NearbyError::WrongCode) => {
                    self.shared.burn();
                    return Err(NearbyError::WrongCode);
                }
                Err(NearbyError::Cancelled) => return Err(NearbyError::Cancelled),
                Err(_) => continue,
            }
        }
    }
}

impl Receiver {
    /// Turns away connections queued while nobody was waiting, so they aren't handshaken
    /// later and the busy slot frees.
    fn drop_queued(&self, rx: &mpsc::Receiver<TcpStream>) {
        let mut drained = false;
        for _ in 0..16 {
            let Ok(mut s) = rx.try_recv() else { break };
            let role = if self.shared.usable() { ROLE_BUSY } else { ROLE_GONE };
            wire::reply_unavailable(&mut s, role);
            drained = true;
        }
        if drained {
            self.shared.busy.store(false, Ordering::Relaxed);
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.advert = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn prepare(s: &TcpStream, t: &Timeouts) -> std::io::Result<()> {
    s.set_nonblocking(false)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(POLL))?;
    s.set_write_timeout(Some(t.idle.max(Duration::from_millis(1))))
}

fn listen_loop(listener: &TcpListener, shared: &Shared, tx: &mpsc::Sender<TcpStream>) {
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
                if !shared.usable() {
                    wire::reply_unavailable(&mut s, ROLE_GONE);
                } else if shared.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                    wire::reply_unavailable(&mut s, ROLE_BUSY);
                } else if tx.send(s).is_err() {
                    return;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(30)),
        }
    }
}

/// A sender's offer: show "<computer> wants to send <package_name> (<size>)" and call
/// [`accept`](IncomingOffer::accept) or [`decline`](IncomingOffer::decline). Nothing has been
/// written to disk yet.
pub struct IncomingOffer {
    /// The sending computer's name (≤ 64 characters).
    pub computer: String,
    /// The package's file name (≤ 255 bytes; for display only).
    pub package_name: String,
    /// Bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the package.
    pub sha256: String,
    /// The same package this code already accepted (a resumed transfer).
    pub is_resume: bool,
    pending: Pending,
    guard: Option<BusyGuard>,
    timeouts: Timeouts,
}

impl IncomingOffer {
    fn new(pending: Pending, guard: Option<BusyGuard>, is_resume: bool, timeouts: Timeouts) -> IncomingOffer {
        IncomingOffer {
            computer: pending.offer.computer.clone(),
            package_name: pending.offer.name.clone(),
            size: pending.offer.size,
            sha256: pending.offer.sha256.clone(),
            is_resume,
            pending,
            guard,
            timeouts,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(pending: Pending, timeouts: Timeouts) -> IncomingOffer {
        IncomingOffer::new(pending, None, false, timeouts)
    }

    fn shared(&self) -> Option<&Shared> {
        self.guard.as_ref().map(|g| g.0.as_ref())
    }

    /// Receives the package into `dest_dir` (as `<sha256>.part`, continuing one that is already
    /// there), checks its SHA-256 and returns the final `<dest_dir>/<sha256>.revpkg`. If the
    /// connection drops ([`NearbyError::is_resumable`]) the `.part` stays and the code stays
    /// valid: call [`Receiver::wait_offer`] again for the sender's retry.
    pub fn accept(mut self, dest_dir: &Path, progress: Progress, cancel: &AtomicBool) -> Result<PathBuf> {
        if let Some(sh) = self.shared() {
            *sh.accepted_sha.lock().unwrap_or_else(PoisonError::into_inner) = Some(self.pending.sha);
        }
        let r = transfer::receive_into(&mut self.pending, dest_dir, progress, cancel, &self.timeouts);
        let keep = matches!(&r, Err(e) if e.is_resumable());
        if let (false, Some(sh)) = (keep, self.shared()) {
            sh.burn();
        }
        r
    }

    /// Says no; the code is used up.
    pub fn decline(mut self) -> Result<()> {
        if let Some(sh) = self.shared() {
            sh.burn();
        }
        transfer::decline(&mut self.pending)
    }
}

/// Where to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Receivers found by [`discover`](super::discover) (tried in order, at most 8).
    Candidates(Vec<Candidate>),
    /// A typed address: `host`, `host:port`, `<lan-address>`, `[fe80::1]:5000`. Without a port
    /// the receiver is asked for it over UDP.
    Address(String),
}

/// Sender settings.
#[derive(Clone, Debug)]
pub struct SendOptions {
    pub timeouts: Timeouts,
    /// Automatic retries of a dropped transfer (resuming).
    pub retries: u32,
    pub retry_delay: Duration,
    /// Tries more of a receiver that answers "busy" before moving on to the next one.
    pub busy_retries: u32,
    pub busy_delay: Duration,
    pub connect_timeout: Duration,
    /// Data record size (capped at 1 MiB).
    pub chunk_bytes: usize,
    /// UDP port receivers answer probes on.
    pub probe_port: u16,
}

impl Default for SendOptions {
    fn default() -> Self {
        SendOptions {
            timeouts: Timeouts::default(),
            retries: 3,
            retry_delay: Duration::from_secs(1),
            busy_retries: 3,
            busy_delay: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(10),
            chunk_bytes: 256 * 1024,
            probe_port: PROBE_PORT,
        }
    }
}

/// How a send finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendOutcome {
    /// The receiver that took it.
    pub addr: SocketAddr,
    /// Where the last (successful) connection continued from (0 = from the start).
    pub resumed_from: u64,
    /// Connections made to that receiver.
    pub attempts: u32,
}

/// Sends the package at `path` to the receiver showing `code`.
///
/// Blocking, for a worker thread: a typed host name is looked up with the system resolver
/// (which may take a while on a slow network) and every connection has timeouts.
/// With several discovered receivers on the code's channel (different ports, so different
/// computers) it returns [`NearbyError::Ambiguous`] instead of spending a guess on each: the UI
/// asks which one and sends again with only that candidate. Several addresses of one
/// receiver (same port) are tried in order.
pub fn send(
    code: &Code,
    target: &Target,
    path: &Path,
    computer_name: &str,
    progress: Progress,
    cancel: &AtomicBool,
    opts: &SendOptions,
) -> Result<SendOutcome> {
    let addrs: Vec<SocketAddr> = match target {
        Target::Candidates(c) => {
            let mut mine: Vec<Candidate> = Vec::new();
            for cand in c.iter().filter(|c| c.channel == code.channel()) {
                if !mine.contains(cand) && mine.len() < MAX_SEND_CANDIDATES {
                    mine.push(*cand);
                }
            }
            let first_port = mine.first().map(|c| c.addr.port());
            if mine.iter().any(|c| Some(c.addr.port()) != first_port) {
                return Err(NearbyError::Ambiguous(mine));
            }
            mine.iter().map(|c| c.addr).collect()
        }
        Target::Address(text) => resolve_address(text, code.channel(), opts.probe_port)?,
    };
    if addrs.is_empty() {
        return Err(NearbyError::NotFound(format!("No receiver with code {} was found. Enter the other computer's address.", code.channel())));
    }
    let timeout = opts.connect_timeout;
    let idle = opts.timeouts.idle;
    send_with(code, &addrs, path, computer_name, progress, cancel, opts, &mut |addr| {
        let s = TcpStream::connect_timeout(&addr, timeout).map_err(|_| NearbyError::Unreachable(addr.to_string()))?;
        let _ = s.set_nodelay(true);
        s.set_read_timeout(Some(POLL)).map_err(wire::lost)?;
        s.set_write_timeout(Some(idle.max(Duration::from_millis(1)))).map_err(wire::lost)?;
        Ok(Box::new(s))
    })
}

/// [`send`] with the connection supplied by `connect` (tests wrap the stream).
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_with(
    code: &Code,
    addrs: &[SocketAddr],
    path: &Path,
    computer_name: &str,
    progress: Progress,
    cancel: &AtomicBool,
    opts: &SendOptions,
    connect: &mut dyn FnMut(SocketAddr) -> Result<Box<dyn Stream>>,
) -> Result<SendOutcome> {
    let pkg = Package::open(path, progress, cancel)?;
    let mut last = NearbyError::NotFound("No receiver answered.".into());
    for &addr in addrs.iter().take(MAX_SEND_CANDIDATES) {
        let mut attempts = 0u32;
        let mut busy_tries = 0u32;
        let mut authenticated = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(NearbyError::Cancelled);
            }
            attempts = attempts.saturating_add(1);
            let result = connect(addr)
                .map_err(|e| (e, false))
                .and_then(|stream| transfer::send_once(stream, code, &pkg, computer_name, opts.chunk_bytes, progress, cancel, &opts.timeouts));
            let (err, authed) = match result {
                Ok(offset) => return Ok(SendOutcome { addr, resumed_from: offset, attempts }),
                Err(e) => e,
            };
            authenticated |= authed;
            let fatal = matches!(
                err,
                NearbyError::WrongCode
                    | NearbyError::Unconfirmed(_)
                    | NearbyError::Ambiguous(_)
                    | NearbyError::Declined
                    | NearbyError::Cancelled
                    | NearbyError::PeerCancelled
                    | NearbyError::ChecksumMismatch
                    | NearbyError::TooLarge(_)
                    | NearbyError::NotFound(_)
                    | NearbyError::Io(_)
            );
            if fatal {
                return Err(err);
            }
            if authenticated {
                let retry = err.is_resumable() || err == NearbyError::Busy;
                if retry && attempts <= opts.retries {
                    sleep_cancellable(opts.retry_delay, cancel);
                    continue;
                }
                return Err(err);
            }
            if err == NearbyError::Busy && busy_tries < opts.busy_retries {
                busy_tries = busy_tries.saturating_add(1);
                sleep_cancellable(opts.busy_delay, cancel);
                continue;
            }
            last = err;
            break;
        }
    }
    Err(last)
}

fn sleep_cancellable(d: Duration, cancel: &AtomicBool) {
    let until = super::after(d);
    while Instant::now() < until && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())));
    }
}

/// Turns a typed address into socket addresses (asking the receiver for its port when none
/// was typed).
fn resolve_address(text: &str, channel: u8, probe_port: u16) -> Result<Vec<SocketAddr>> {
    let text = text.trim();
    let bad = || NearbyError::NotFound(format!("Couldn't find the computer \"{}\".", wire::truncate_bytes(text, 64)));
    if text.is_empty() || text.len() > MAX_ADDRESS_LEN {
        return Err(bad());
    }
    if let Ok(a) = text.parse::<SocketAddr>() {
        return Ok(vec![a]);
    }
    let has_port = text.rsplit_once(':').is_some_and(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok());
    if has_port {
        let addrs: Vec<SocketAddr> = text.to_socket_addrs().map_err(|_| bad())?.take(MAX_SEND_CANDIDATES).collect();
        return Ok(addrs);
    }
    let host = text.trim_start_matches('[').trim_end_matches(']');
    let ips: Vec<std::net::IpAddr> = (host, 0u16).to_socket_addrs().map_err(|_| bad())?.map(|a| a.ip()).take(MAX_SEND_CANDIDATES).collect();
    let found = discovery::probe_hosts(&ips, channel, probe_port, Duration::from_secs(2));
    if found.is_empty() {
        return Err(NearbyError::NotFound(format!(
            "The computer \"{}\" didn't answer. Add the port shown on the other computer (address:port).",
            wire::truncate_bytes(text, 64)
        )));
    }
    Ok(found.into_iter().map(|c| c.addr).collect())
}
