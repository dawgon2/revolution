//! End-to-end and hostile-peer tests for nearby transfer.

use super::net::send_with;
use super::transfer::{self, Package, Served};
use super::wire::{self, KIND_DATA, KIND_OFFER, Offer, Record, Role, Session, Stream, handshake};
use super::*;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

// ---------- helpers ----------

fn fast() -> Timeouts {
    Timeouts {
        hello: Duration::from_secs(5),
        handshake: Duration::from_secs(5),
        idle: Duration::from_secs(3),
        accept_prompt: Duration::from_secs(5),
        code_lifetime: Duration::from_secs(120),
    }
}

fn recv_opts() -> ReceiverOptions {
    ReceiverOptions { port: 0, probe_port: None, mdns: false, timeouts: fast() }
}

fn send_opts() -> SendOptions {
    SendOptions {
        timeouts: fast(),
        retries: 3,
        retry_delay: Duration::from_millis(150),
        busy_retries: 3,
        busy_delay: Duration::from_millis(200),
        connect_timeout: Duration::from_secs(2),
        chunk_bytes: 4096,
        probe_port: PROBE_PORT,
    }
}

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("fc-nearby-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Deterministic xorshift.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn package(dir: &Path, size: usize, seed: u64) -> PathBuf {
    let path = dir.join("My Film.revpkg");
    std::fs::write(&path, Rng(seed | 1).bytes(size)).unwrap();
    path
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn local(r: &Receiver) -> Target {
    Target::Candidates(vec![Candidate { channel: r.code().channel(), addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), r.port()) }])
}

fn other_code(c: Code) -> Code {
    let s = c.to_string();
    let parts: Vec<&str> = s.split('-').collect();
    let w0 = words::lookup(parts[1]).unwrap();
    Code::from_parts(c.channel(), [w0.wrapping_add(1), words::lookup(parts[2]).unwrap(), words::lookup(parts[3]).unwrap()]).unwrap()
}

/// In-memory duplex stream (one end of [`pipe`]).
struct PipeEnd {
    rx: mpsc::Receiver<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
    poll: Duration,
}

fn pipe() -> (PipeEnd, PipeEnd) {
    let (t1, r1) = mpsc::channel();
    let (t2, r2) = mpsc::channel();
    let poll = Duration::from_millis(50);
    (PipeEnd { rx: r1, tx: t2, buf: Vec::new(), pos: 0, poll }, PipeEnd { rx: r2, tx: t1, buf: Vec::new(), pos: 0, poll })
}

impl Read for PipeEnd {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv_timeout(self.poll) {
                Ok(v) => {
                    self.buf = v;
                    self.pos = 0;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for PipeEnd {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if !data.is_empty() {
            self.tx.send(data.to_vec()).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for PipeEnd {
    fn set_poll(&mut self, wait: Duration) -> io::Result<()> {
        self.poll = wait.max(Duration::from_millis(1));
        Ok(())
    }
}

/// Reads a fixed script, then end of stream; writes vanish.
struct Script {
    input: Vec<u8>,
    pos: usize,
}

impl Read for Script {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.input.len() - self.pos);
        out[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Script {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for Script {}

/// Shared-buffer stream for record-layer tampering tests.
#[derive(Clone)]
struct Mem {
    input: Arc<Mutex<VecDeque<u8>>>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Mem {
    fn new() -> Mem {
        Mem { input: Arc::default(), output: Arc::default() }
    }
}

impl Read for Mem {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut q = self.input.lock().unwrap();
        let n = out.len().min(q.len());
        for slot in out.iter_mut().take(n) {
            *slot = q.pop_front().unwrap();
        }
        Ok(n)
    }
}

impl Write for Mem {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.output.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for Mem {}

/// TCP that fails (and closes) after `left` written bytes.
struct FailAfter {
    inner: TcpStream,
    left: usize,
}

impl Read for FailAfter {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.inner.read(out)
    }
}

impl Write for FailAfter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.left == 0 {
            let _ = self.inner.shutdown(Shutdown::Both);
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = data.len().min(self.left);
        let n = self.inner.write(&data[..n])?;
        self.left -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Stream for FailAfter {
    fn set_poll(&mut self, wait: Duration) -> io::Result<()> {
        self.inner.set_poll(wait)
    }
}

fn tcp_connect(addr: SocketAddr) -> Result<Box<dyn Stream>> {
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(wire::lost)?;
    s.set_read_timeout(Some(wire::POLL)).map_err(wire::lost)?;
    Ok(Box::new(s))
}

fn no_progress() -> impl FnMut(u64, u64) {
    |_, _| {}
}

// ---------- end to end over loopback TCP ----------

#[test]
fn round_trip_over_tcp() {
    let src = TempDir::new("rt-src");
    let dest = TempDir::new("rt-dest");
    let path = package(&src.0, 50_000, 7);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let target = local(&r);
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        let offer = r.wait_offer(&never, Duration::from_secs(10)).unwrap();
        assert_eq!(offer.computer, "Studio PC");
        assert_eq!(offer.package_name, "My Film.revpkg");
        assert_eq!(offer.size, 50_000);
        assert!(!offer.is_resume);
        let mut last = 0;
        let out = offer.accept(
            &dest_dir,
            &mut |done, total| {
                assert!(done <= total);
                last = done;
            },
            &never,
        );
        assert_eq!(last, 50_000);
        assert!(!r.is_usable(), "a finished transfer burns the code");
        out
    });
    let never = AtomicBool::new(false);
    let mut sent = 0;
    let outcome = send(&code, &target, &path, "Studio PC", &mut |d, _| sent = d, &never, &send_opts()).unwrap();
    assert_eq!(outcome.resumed_from, 0);
    assert_eq!(outcome.attempts, 1);
    assert_eq!(sent, 50_000);
    let got = recv.join().unwrap().unwrap();
    assert_eq!(std::fs::read(&got).unwrap(), std::fs::read(&path).unwrap());
    let name = got.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with(".revpkg") && name.len() == 64 + 7, "{name}");
    assert_eq!(entries(&dest.0), vec![name]);
}

#[test]
fn typed_address_and_empty_package() {
    let src = TempDir::new("addr-src");
    let dest = TempDir::new("addr-dest");
    let path = package(&src.0, 0, 1);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let target = Target::Address(format!("127.0.0.1:{}", r.port()));
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let never = AtomicBool::new(false);
    send(&code, &target, &path, "Laptop", &mut no_progress(), &never, &send_opts()).unwrap();
    let got = recv.join().unwrap().unwrap();
    assert_eq!(std::fs::read(&got).unwrap().len(), 0);
}

#[test]
fn udp_probe_finds_the_port() {
    let free = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let r = Receiver::start(ReceiverOptions { probe_port: Some(free), ..recv_opts() }).unwrap();
    if !r.warnings().is_empty() {
        eprintln!("skipping: {:?}", r.warnings());
        return;
    }
    let found = discovery_probe(r.code().channel(), free);
    assert_eq!(found, vec![Candidate { channel: r.code().channel(), addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), r.port()) }]);
    let other = if r.code().channel() == 1 { 2 } else { 1 };
    assert!(discovery_probe(other, free).is_empty());

    // A typed address without a port asks over UDP.
    let src = TempDir::new("probe-src");
    let dest = TempDir::new("probe-dest");
    let path = package(&src.0, 1000, 3);
    let code = r.code();
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let never = AtomicBool::new(false);
    let opts = SendOptions { probe_port: free, ..send_opts() };
    send(&code, &Target::Address("127.0.0.1".into()), &path, "Laptop", &mut no_progress(), &never, &opts).unwrap();
    recv.join().unwrap().unwrap();
}

fn discovery_probe(channel: u8, port: u16) -> Vec<Candidate> {
    super::discovery::probe_hosts(&[Ipv4Addr::LOCALHOST.into()], channel, port, Duration::from_millis(800))
}

#[test]
#[ignore = "needs multicast on the test machine"]
fn mdns_discovery() {
    let r = Receiver::start(ReceiverOptions { mdns: true, ..recv_opts() }).unwrap();
    let found = discover(r.code().channel(), Duration::from_secs(5));
    assert!(found.iter().any(|c| c.addr.port() == r.port()), "{found:?} {:?}", r.warnings());
    assert!(list_receivers(Duration::from_secs(5)).iter().any(|c| c.addr.port() == r.port()));
}

#[test]
fn wrong_code_fails_on_both_sides_and_burns_the_code() {
    let src = TempDir::new("wc-src");
    let dest = TempDir::new("wc-dest");
    let path = package(&src.0, 1000, 2);
    let r = Arc::new(Receiver::start(recv_opts()).unwrap());
    let code = r.code();
    let target = local(&r);
    let r2 = r.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r2.wait_offer(&never, Duration::from_secs(10)).err()
    });
    let never = AtomicBool::new(false);
    let res = send(&other_code(code), &target, &path, "Laptop", &mut no_progress(), &never, &send_opts());
    assert_eq!(res, Err(NearbyError::WrongCode));
    assert_eq!(recv.join().unwrap(), Some(NearbyError::WrongCode));
    assert!(!r.is_usable());
    assert!(entries(&dest.0).is_empty());
    // The right code no longer works either: one guess per code.
    let res = send(&code, &target, &path, "Laptop", &mut no_progress(), &never, &send_opts());
    assert_eq!(res, Err(NearbyError::Expired));
    assert_eq!(r.wait_offer(&never, Duration::from_secs(1)).err(), Some(NearbyError::Expired));
}

#[test]
fn decline_writes_nothing() {
    let src = TempDir::new("dec-src");
    let dest = TempDir::new("dec-dest");
    let path = package(&src.0, 1000, 4);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let target = local(&r);
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().decline()
    });
    let never = AtomicBool::new(false);
    let res = send(&code, &target, &path, "Laptop", &mut no_progress(), &never, &send_opts());
    assert_eq!(res, Err(NearbyError::Declined));
    recv.join().unwrap().unwrap();
    assert!(entries(&dest.0).is_empty());
}

#[test]
fn sender_cancel() {
    let src = TempDir::new("sc-src");
    let dest = TempDir::new("sc-dest");
    let path = package(&src.0, 200_000, 5);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let target = local(&r);
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let cancel = AtomicBool::new(false);
    let res = send(
        &code,
        &target,
        &path,
        "Laptop",
        &mut |d, _| {
            if d >= 20_000 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
        &send_opts(),
    );
    assert_eq!(res, Err(NearbyError::Cancelled));
    assert_eq!(recv.join().unwrap(), Err(NearbyError::PeerCancelled));
    assert!(entries(&dest.0).is_empty(), "{:?}", entries(&dest.0));
}

#[test]
fn receiver_cancel() {
    let src = TempDir::new("rc-src");
    let dest = TempDir::new("rc-dest");
    let path = package(&src.0, 400_000, 6);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let target = local(&r);
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let cancel = AtomicBool::new(false);
        let offer = r.wait_offer(&cancel, Duration::from_secs(10)).unwrap();
        let out = offer.accept(
            &dest_dir,
            &mut |d, _| {
                if d >= 20_000 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        );
        (out, r.is_usable())
    });
    let never = AtomicBool::new(false);
    let res = send(&code, &target, &path, "Laptop", &mut no_progress(), &never, &send_opts());
    assert_eq!(res, Err(NearbyError::PeerCancelled));
    let (out, usable) = recv.join().unwrap();
    assert_eq!(out, Err(NearbyError::Cancelled));
    assert!(!usable);
    assert!(entries(&dest.0).is_empty());
}

#[test]
fn waiting_can_be_cancelled_and_times_out() {
    let r = Receiver::start(recv_opts()).unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(r.wait_offer(&cancel, Duration::from_secs(5)).err(), Some(NearbyError::Cancelled));
    let never = AtomicBool::new(false);
    assert_eq!(r.wait_offer(&never, Duration::from_millis(200)).err(), Some(NearbyError::Timeout));
    assert!(r.is_usable());
    let short = Receiver::start(ReceiverOptions { timeouts: Timeouts { code_lifetime: Duration::from_millis(100), ..fast() }, ..recv_opts() }).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(short.wait_offer(&never, Duration::from_secs(5)).err(), Some(NearbyError::Expired));
}

#[test]
fn drop_mid_transfer_then_resume() {
    let src = TempDir::new("res-src");
    let dest = TempDir::new("res-dest");
    let path = package(&src.0, 100_000, 8);
    let r = Receiver::start(recv_opts()).unwrap();
    let code = r.code();
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), r.port());
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        let mut results = Vec::new();
        for _ in 0..5 {
            let offer = r.wait_offer(&never, Duration::from_secs(10)).unwrap();
            let resumed = offer.is_resume;
            let res = offer.accept(&dest_dir, &mut no_progress(), &never);
            let done = !matches!(&res, Err(e) if e.is_resumable());
            results.push((resumed, res));
            if done {
                break;
            }
        }
        results
    });
    let never = AtomicBool::new(false);
    let mut first = true;
    let outcome = send_with(&code, &[addr], &path, "Laptop", &mut no_progress(), &never, &send_opts(), &mut |a| {
        if std::mem::take(&mut first) {
            // Handshake + offer + ~40 KB of data, then the line goes dead.
            let inner = TcpStream::connect_timeout(&a, Duration::from_secs(2)).map_err(wire::lost)?;
            inner.set_read_timeout(Some(wire::POLL)).map_err(wire::lost)?;
            return Ok(Box::new(FailAfter { inner, left: 40_000 }));
        }
        tcp_connect(a)
    })
    .unwrap();
    assert!(outcome.resumed_from > 0 && outcome.resumed_from < 100_000, "{outcome:?}");
    assert!(outcome.attempts >= 2);
    let results = recv.join().unwrap();
    let (resumed, last) = results.last().unwrap();
    assert!(*resumed);
    let got = last.clone().unwrap();
    assert!(matches!(results[0].1, Err(NearbyError::ConnectionLost(_))), "{results:?}");
    assert_eq!(std::fs::read(&got).unwrap(), std::fs::read(&path).unwrap());
    assert_eq!(entries(&dest.0).len(), 1);
}

#[test]
fn resume_declines_a_different_package_and_busy_peers() {
    let src = TempDir::new("diff-src");
    let dest = TempDir::new("diff-dest");
    let path_a = package(&src.0, 60_000, 9);
    let path_b = src.0.join("other.revpkg");
    std::fs::write(&path_b, Rng(99).bytes(5000)).unwrap();
    let r = Arc::new(Receiver::start(recv_opts()).unwrap());
    let code = r.code();
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), r.port());
    let target = local(&r);
    let never = AtomicBool::new(false);

    // A starts, drops (no automatic retries), leaving a .part.
    let r1 = r.clone();
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r1.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let opts = SendOptions { retries: 0, ..send_opts() };
    let res = send_with(&code, &[addr], &path_a, "Laptop", &mut no_progress(), &never, &opts, &mut |a| {
        let inner = TcpStream::connect_timeout(&a, Duration::from_secs(2)).map_err(wire::lost)?;
        inner.set_read_timeout(Some(wire::POLL)).map_err(wire::lost)?;
        Ok(Box::new(FailAfter { inner, left: 30_000 }))
    });
    assert!(matches!(res, Err(NearbyError::ConnectionLost(_))), "{res:?}");
    assert!(matches!(recv.join().unwrap(), Err(NearbyError::ConnectionLost(_))));
    assert!(r.is_usable());
    assert_eq!(entries(&dest.0).len(), 1);

    // B with the same code is declined automatically; the receiver keeps waiting.
    let r2 = r.clone();
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        let offer = r2.wait_offer(&never, Duration::from_secs(10)).unwrap();
        assert!(offer.is_resume);
        // While A's offer is pending, a third sender is told the receiver is busy.
        std::thread::sleep(Duration::from_millis(600));
        offer.accept(&dest_dir, &mut no_progress(), &never)
    });
    let res = send(&code, &target, &path_b, "Intruder", &mut no_progress(), &never, &send_opts());
    assert_eq!(res, Err(NearbyError::Declined));
    let a = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        send(&code, &local_target(addr, code), &path_a, "Laptop", &mut no_progress(), &never, &send_opts())
    });
    std::thread::sleep(Duration::from_millis(250));
    let busy = send(&code, &target, &path_b, "Third", &mut no_progress(), &never, &SendOptions { retries: 0, busy_retries: 0, ..send_opts() });
    assert_eq!(busy, Err(NearbyError::Busy));
    let outcome = a.join().unwrap().unwrap();
    assert!(outcome.resumed_from > 0);
    let got = recv.join().unwrap().unwrap();
    assert_eq!(std::fs::read(got).unwrap().len(), 60_000);
}

fn local_target(addr: SocketAddr, code: Code) -> Target {
    Target::Candidates(vec![Candidate { channel: code.channel(), addr }])
}

#[test]
fn missing_and_unreachable() {
    let never = AtomicBool::new(false);
    let code = Code::generate().unwrap();
    let dir = TempDir::new("missing");
    let res = send(&code, &local_target("127.0.0.1:9".parse().unwrap(), code), &dir.0.join("nope.revpkg"), "x", &mut no_progress(), &never, &send_opts());
    assert!(matches!(res, Err(NearbyError::NotFound(_))));
    let path = package(&dir.0, 10, 1);
    // Nothing listens on port 1 (needs root to bind): connection refused, no receiver.
    let res = send(&code, &local_target("127.0.0.1:1".parse().unwrap(), code), &path, "x", &mut no_progress(), &never, &send_opts());
    // a refused / firewalled port says so (the PC ↔ Dell check met ufw dropping it)
    assert!(matches!(&res, Err(NearbyError::Unreachable(a)) if a == "127.0.0.1:1"), "{res:?}");
    assert!(res.is_err_and(|e| e.to_string().contains("firewall")));
    let res = send(&code, &Target::Candidates(vec![]), &path, "x", &mut no_progress(), &never, &send_opts());
    assert!(matches!(res, Err(NearbyError::NotFound(_))));
    let res = send(&code, &Target::Address(String::new()), &path, "x", &mut no_progress(), &never, &send_opts());
    assert!(matches!(res, Err(NearbyError::NotFound(_))));
    let res = send(&code, &Target::Address("x".repeat(300)), &path, "x", &mut no_progress(), &never, &send_opts());
    assert!(matches!(res, Err(NearbyError::NotFound(_))));
}

// ---------- the record layer ----------

fn session_pair() -> (Session, Session, Mem, Mem) {
    let a = Mem::new();
    let b = Mem::new();
    let (sa, sb) = Session::pair_for_tests(Box::new(a.clone()), Box::new(b.clone()));
    (sa, sb, a, b)
}

/// Moves what `from` wrote into `to`'s input, through `f`.
fn deliver(from: &Mem, to: &Mem, f: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let bytes = std::mem::take(&mut *from.output.lock().unwrap());
    to.input.lock().unwrap().extend(f(bytes));
}

fn frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        out.push(bytes[i..i + 5 + len].to_vec());
        i += 5 + len;
    }
    out
}

#[test]
fn records_round_trip() {
    let never = AtomicBool::new(false);
    let (mut a, mut b, ma, mb) = session_pair();
    a.send(&Record::Accept(42)).unwrap();
    a.send_data(&[7u8; 1000]).unwrap();
    a.send(&Record::Done).unwrap();
    a.send(&Record::Error("é".repeat(400))).unwrap();
    deliver(&ma, &mb, |v| v);
    assert_eq!(b.recv(None, &never).unwrap(), Record::Accept(42));
    assert_eq!(b.recv(None, &never).unwrap(), Record::Data(vec![7u8; 1000]));
    assert_eq!(b.recv(None, &never).unwrap(), Record::Done);
    match b.recv(None, &never).unwrap() {
        Record::Error(m) => assert!(m.len() <= 512 && m.starts_with('é')),
        other => panic!("{other:?}"),
    }
    assert!(a.send_data(&[]).is_err());
    assert!(a.send_data(&vec![0u8; wire::MAX_DATA + 1]).is_err());
}

#[test]
fn replayed_reordered_and_flipped_records_fail() {
    let never = AtomicBool::new(false);
    // Replay: the same record twice.
    let (mut a, mut b, ma, mb) = session_pair();
    a.send(&Record::Accept(1)).unwrap();
    deliver(&ma, &mb, |v| [v.clone(), v].concat());
    assert_eq!(b.recv(None, &never).unwrap(), Record::Accept(1));
    assert_eq!(b.recv(None, &never), Err(NearbyError::Tampered));

    // Reorder: second before first.
    let (mut a, mut b, ma, mb) = session_pair();
    a.send(&Record::Accept(1)).unwrap();
    a.send(&Record::Accept(2)).unwrap();
    deliver(&ma, &mb, |v| {
        let f = frames(&v);
        [f[1].clone(), f[0].clone()].concat()
    });
    assert_eq!(b.recv(None, &never), Err(NearbyError::Tampered));

    // Dropped record: the next one fails.
    let (mut a, mut b, ma, mb) = session_pair();
    a.send(&Record::Accept(1)).unwrap();
    a.send(&Record::Accept(2)).unwrap();
    deliver(&ma, &mb, |v| frames(&v)[1].clone());
    assert_eq!(b.recv(None, &never), Err(NearbyError::Tampered));

    // Every single bit flip in a record (length, kind or ciphertext) is an error, never a panic.
    let (mut a, _, ma, _) = session_pair();
    a.send_data(b"hello world").unwrap();
    let clean = std::mem::take(&mut *ma.output.lock().unwrap());
    for bit in 0..clean.len() * 8 {
        let (_, mut b, _, mb) = session_pair();
        let mut v = clean.clone();
        v[bit / 8] ^= 1 << (bit % 8);
        mb.input.lock().unwrap().extend(v);
        assert!(b.recv(Some(wire::Wait::Idle(Duration::from_millis(5))), &never).is_err(), "bit {bit}");
    }
}

#[test]
fn huge_lengths_are_refused_before_allocating() {
    let never = AtomicBool::new(false);
    for (kind, len) in [(KIND_DATA, u32::MAX), (KIND_DATA, (wire::MAX_DATA + 17) as u32), (KIND_OFFER, 5000), (0x42, 100), (KIND_DATA, 3)] {
        let (_, mut b, _, mb) = session_pair();
        let mut hdr = len.to_be_bytes().to_vec();
        hdr.push(kind);
        mb.input.lock().unwrap().extend(hdr);
        let e = b.recv(None, &never).unwrap_err();
        assert!(matches!(e, NearbyError::TooLarge(_) | NearbyError::Protocol(_)), "{e:?}");
    }
}

// ---------- hostile peers ----------

fn hello(role: u8, rng: &mut Rng, real_spake: bool) -> Vec<u8> {
    let mut v = wire::MAGIC.to_vec();
    v.push(wire::VERSION);
    v.push(role);
    if real_spake {
        let pw = rng.bytes(8);
        let (_, msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(&spake2::Password::new(pw), &spake2::Identity::new(b"x"));
        v.extend(msg);
    } else {
        let n = rng.below(40) as usize;
        v.extend(rng.bytes(n));
    }
    v
}

#[test]
fn fuzz_before_the_key_exchange() {
    let code = Code::generate().unwrap();
    let never = AtomicBool::new(false);
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let t = Timeouts { handshake: Duration::from_millis(200), idle: Duration::from_millis(200), ..fast() };
    for i in 0..400u32 {
        let role = if i % 2 == 0 { Role::Sender } else { Role::Receiver };
        let peer_role = if role == Role::Sender { wire::ROLE_RECEIVER } else { wire::ROLE_SENDER };
        let input = match i % 6 {
            0 => {
                let n = rng.below(120) as usize;
                rng.bytes(n)
            }
            1 => hello(peer_role, &mut rng, false),
            2 => hello([wire::ROLE_BUSY, wire::ROLE_GONE, 0, 9, peer_role][rng.below(5) as usize], &mut rng, false),
            3 => {
                let mut v = hello(peer_role, &mut rng, true);
                v.extend(u32::MAX.to_be_bytes());
                v.push(wire::KIND_CONFIRM);
                v
            }
            4 => {
                let mut v = hello(peer_role, &mut rng, true);
                let n = rng.below(80) as usize;
                v.extend(rng.bytes(n));
                v
            }
            _ => {
                let mut v = hello(peer_role, &mut rng, true);
                v.extend(48u32.to_be_bytes());
                v.push(wire::KIND_CONFIRM);
                v.extend(rng.bytes(48));
                let cut = rng.below(v.len() as u64 + 1) as usize;
                if i % 4 == 1 {
                    v.truncate(cut);
                }
                v
            }
        };
        let res = handshake(Box::new(Script { input: input.clone(), pos: 0 }), role, &code, &t, &never);
        assert!(res.is_err(), "iteration {i}");
        if role == Role::Receiver {
            let res = transfer::serve(Box::new(Script { input, pos: 0 }), &code, &t, None, &never);
            assert!(res.is_err(), "iteration {i}");
        }
    }
}

/// An authenticated but hostile sender: sends garbage at every stage after the key exchange.
#[test]
fn fuzz_authenticated_sender() {
    let code = Code::generate().unwrap();
    let mut rng = Rng(0x0dd_f00d_0002);
    let t = Timeouts { idle: Duration::from_millis(300), ..fast() };
    for i in 0..120u32 {
        let dest = TempDir::new("fz-recv");
        let (ea, eb) = pipe();
        let dest_dir = dest.0.clone();
        let recv = std::thread::spawn(move || {
            let never = AtomicBool::new(false);
            match transfer::serve(Box::new(ea), &code, &t, None, &never)? {
                Served::Offer(mut p) => transfer::receive_into(&mut p, &dest_dir, &mut |_, _| {}, &never, &t),
                Served::DeclinedOther => Err(NearbyError::Declined),
            }
        });
        let never = AtomicBool::new(false);
        let mut s = handshake(Box::new(eb), Role::Sender, &code, &t, &never).unwrap();
        let size = rng.below(5000);
        let offer = Offer { computer: "Fuzz".into(), name: "f.revpkg".into(), size, sha256: wire::hex(&rng.bytes(32)) };
        let stage = i % 7;
        if stage >= 3 {
            let _ = s.send(&Record::Offer(offer));
        }
        for _ in 0..rng.below(6) + 1 {
            let _ = match stage {
                0 | 3 => {
                    let n = rng.below(64) as usize;
                    s.stream_mut().write_all(&rng.bytes(n)).map_err(wire::lost)
                }
                1 | 4 => {
                    let mut hdr = (rng.next() as u32 | 0x8000_0000).to_be_bytes().to_vec();
                    hdr.push((rng.below(9)) as u8);
                    s.stream_mut().write_all(&hdr).map_err(wire::lost)
                }
                2 | 5 => {
                    let n = rng.below(80) as usize;
                    s.send_raw(rng.below(9) as u8, &rng.bytes(n))
                }
                _ => {
                    // Plausible records with wrong numbers: too much data, early Done, bad hash.
                    match rng.below(4) {
                        0 => s.send_data(&rng.bytes(size as usize + 1)),
                        1 => s.send(&Record::Done),
                        2 => {
                            if size > 0 {
                                let _ = s.send_data(&rng.bytes(size as usize));
                            }
                            s.send(&Record::Done)
                        }
                        _ => s.send(&Record::Accept(rng.next())),
                    }
                }
            };
        }
        drop(s);
        let res = recv.join().expect("the receiver must not panic");
        assert!(res.is_err(), "iteration {i}: {res:?}");
        assert!(!entries(&dest.0).iter().any(|e| e.ends_with(".revpkg")), "iteration {i}");
    }
}

/// An authenticated but hostile receiver.
#[test]
fn fuzz_authenticated_receiver() {
    let code = Code::generate().unwrap();
    let src = TempDir::new("fz-send");
    let path = package(&src.0, 3000, 11);
    let never = AtomicBool::new(false);
    let pkg = Package::open(&path, &mut |_, _| {}, &never).unwrap();
    let mut rng = Rng(0xfeed_beef_0003);
    let t = Timeouts { idle: Duration::from_millis(300), accept_prompt: Duration::from_millis(300), ..fast() };
    for i in 0..120u32 {
        let (ea, eb) = pipe();
        let pkg = pkg.clone();
        let send = std::thread::spawn(move || {
            let never = AtomicBool::new(false);
            transfer::send_once(Box::new(ea), &code, &pkg, "Laptop", 512, &mut |_, _| {}, &never, &t)
        });
        let mut s = handshake(Box::new(eb), Role::Receiver, &code, &t, &never).unwrap();
        let _ = s.recv(None, &never);
        let stage = i % 5;
        for _ in 0..rng.below(5) + 1 {
            let _ = match stage {
                0 => {
                    let n = rng.below(64) as usize;
                    s.stream_mut().write_all(&rng.bytes(n)).map_err(wire::lost)
                }
                1 => {
                    let mut hdr = (rng.next() as u32 | 0x8000_0000).to_be_bytes().to_vec();
                    hdr.push(rng.below(9) as u8);
                    s.stream_mut().write_all(&hdr).map_err(wire::lost)
                }
                2 => {
                    let n = rng.below(80) as usize;
                    s.send_raw(rng.below(9) as u8, &rng.bytes(n))
                }
                3 => s.send(&Record::Accept(3000 + 1 + rng.below(1 << 40))),
                _ => {
                    let _ = s.send(&Record::Accept(rng.below(3001)));
                    let n = rng.below(40) as usize;
                    s.send_raw(rng.below(9) as u8, &rng.bytes(n))
                }
            };
        }
        drop(s);
        let res = send.join().expect("the sender must not panic");
        // Stage 4 could, by a fluke of timing, end with a valid Done after the data.
        if stage != 4 {
            assert!(res.is_err(), "iteration {i}: {res:?}");
        }
    }
}

#[test]
fn oversize_offer_fields_are_rejected() {
    let code = Code::generate().unwrap();
    let t = fast();
    let cases: Vec<Box<dyn Fn(&mut Session) -> Result<()> + Send>> = vec![
        Box::new(|s| s.send(&Record::Offer(Offer { computer: "c".repeat(65), name: "n".into(), size: 1, sha256: "0".repeat(64) }))),
        Box::new(|s| s.send(&Record::Offer(Offer { computer: "c".into(), name: "n".repeat(256), size: 1, sha256: "0".repeat(64) }))),
        Box::new(|s| s.send(&Record::Offer(Offer { computer: "c".into(), name: "n".into(), size: MAX_PACKAGE_BYTES + 1, sha256: "0".repeat(64) }))),
        Box::new(|s| s.send_raw(KIND_OFFER, &vec![b' '; 5000])),
    ];
    for (i, case) in cases.into_iter().enumerate() {
        let (ea, eb) = pipe();
        let recv = std::thread::spawn(move || {
            let never = AtomicBool::new(false);
            transfer::serve(Box::new(ea), &code, &t, None, &never).err()
        });
        let never = AtomicBool::new(false);
        let mut s = handshake(Box::new(eb), Role::Sender, &code, &t, &never).unwrap();
        let _ = case(&mut s);
        let err = recv.join().unwrap();
        assert!(matches!(err, Some(NearbyError::TooLarge(_))), "case {i}: {err:?}");
    }
}

#[test]
fn checksum_mismatch_deletes_the_part() {
    let code = Code::generate().unwrap();
    let t = fast();
    let dest = TempDir::new("sum");
    let (ea, eb) = pipe();
    let dest_dir = dest.0.clone();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        let Served::Offer(p) = transfer::serve(Box::new(ea), &code, &t, None, &never).unwrap() else { panic!() };
        IncomingOffer::for_tests(*p, t).accept(&dest_dir, &mut |_, _| {}, &never)
    });
    let never = AtomicBool::new(false);
    let mut s = handshake(Box::new(eb), Role::Sender, &code, &t, &never).unwrap();
    s.send(&Record::Offer(Offer { computer: "c".into(), name: "n".into(), size: 10, sha256: "0".repeat(64) })).unwrap();
    assert_eq!(s.recv(None, &never).unwrap(), Record::Accept(0));
    s.send_data(&[1u8; 10]).unwrap();
    s.send(&Record::Done).unwrap();
    assert_eq!(s.recv(None, &never).unwrap(), Record::Error("checksum".into()));
    assert_eq!(recv.join().unwrap(), Err(NearbyError::ChecksumMismatch));
    assert!(entries(&dest.0).is_empty());
}

#[test]
fn handshake_over_a_pipe_and_wrong_code() {
    let code = Code::generate().unwrap();
    let t = fast();
    let (ea, eb) = pipe();
    let h = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        handshake(Box::new(ea), Role::Receiver, &code, &t, &never).map(|_| ())
    });
    let never = AtomicBool::new(false);
    assert!(handshake(Box::new(eb), Role::Sender, &code, &t, &never).is_ok());
    assert!(h.join().unwrap().is_ok());

    let (ea, eb) = pipe();
    let h = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        handshake(Box::new(ea), Role::Receiver, &code, &t, &never).map(|_| ())
    });
    let wrong = other_code(code);
    assert_eq!(handshake(Box::new(eb), Role::Sender, &wrong, &t, &never).err(), Some(NearbyError::WrongCode));
    assert_eq!(h.join().unwrap().err(), Some(NearbyError::WrongCode));

    // Two senders talking to each other: refused at the hello.
    let (ea, eb) = pipe();
    let h = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        handshake(Box::new(ea), Role::Sender, &code, &t, &never).map(|_| ())
    });
    assert!(matches!(handshake(Box::new(eb), Role::Sender, &code, &t, &never).err(), Some(NearbyError::Protocol(_))));
    assert!(h.join().unwrap().is_err());
}

// ---------- review fixes ----------

/// TCP that flips bits in one written byte.
struct Mutate {
    inner: TcpStream,
    at: usize,
    xor: u8,
    written: usize,
}

impl Read for Mutate {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.inner.read(out)
    }
}

impl Write for Mutate {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut v = data.to_vec();
        if self.at >= self.written && self.at < self.written + v.len() {
            v[self.at - self.written] ^= self.xor;
        }
        self.inner.write_all(&v)?;
        self.written += v.len();
        Ok(v.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Stream for Mutate {
    fn set_poll(&mut self, wait: Duration) -> io::Result<()> {
        self.inner.set_poll(wait)
    }
}

#[test]
fn tampered_or_wrong_kind_confirm_burns_the_code() {
    let src = TempDir::new("tc-src");
    let path = package(&src.0, 100, 12);
    // Byte 43 + 4 is the Confirm's kind byte; 43 + 9 is inside its ciphertext.
    for (at, xor) in [(47usize, 0x03u8), (52, 0x10)] {
        let r = Arc::new(Receiver::start(recv_opts()).unwrap());
        let code = r.code();
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), r.port());
        let r2 = r.clone();
        let recv = std::thread::spawn(move || r2.wait_offer(&AtomicBool::new(false), Duration::from_secs(10)).err());
        let never = AtomicBool::new(false);
        let res = send_with(&code, &[addr], &path, "x", &mut no_progress(), &never, &send_opts(), &mut |a| {
            let inner = TcpStream::connect_timeout(&a, Duration::from_secs(2)).map_err(wire::lost)?;
            inner.set_read_timeout(Some(wire::POLL)).map_err(wire::lost)?;
            Ok(Box::new(Mutate { inner, at, xor, written: 0 }))
        });
        assert_eq!(res, Err(NearbyError::WrongCode), "byte {at}");
        assert_eq!(recv.join().unwrap(), Some(NearbyError::WrongCode), "byte {at}");
        assert!(!r.is_usable());
    }
}

fn part_path(dest: &Path, pkg: &Path) -> PathBuf {
    let p = Package::open(pkg, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
    dest.join(format!("{}.part", wire::hex(&p.sha)))
}

#[test]
fn part_longer_than_the_offer_restarts_at_zero() {
    let src = TempDir::new("long-src");
    let dest = TempDir::new("long-dest");
    let path = package(&src.0, 20_000, 13);
    std::fs::write(part_path(&dest.0, &path), vec![9u8; 20_010]).unwrap();
    let r = Receiver::start(recv_opts()).unwrap();
    let (code, target, dest_dir) = (r.code(), local(&r), dest.0.clone());
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let outcome = send(&code, &target, &path, "x", &mut no_progress(), &AtomicBool::new(false), &send_opts()).unwrap();
    assert_eq!(outcome.resumed_from, 0);
    let got = recv.join().unwrap().unwrap();
    assert_eq!(std::fs::read(got).unwrap(), std::fs::read(&path).unwrap());
}

#[test]
fn part_with_a_corrupt_prefix_fails_the_checksum_and_is_deleted() {
    let src = TempDir::new("bad-src");
    let dest = TempDir::new("bad-dest");
    let path = package(&src.0, 20_000, 14);
    std::fs::write(part_path(&dest.0, &path), vec![9u8; 1000]).unwrap();
    let r = Receiver::start(recv_opts()).unwrap();
    let (code, target, dest_dir) = (r.code(), local(&r), dest.0.clone());
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    let res = send(&code, &target, &path, "x", &mut no_progress(), &AtomicBool::new(false), &send_opts());
    assert_eq!(res, Err(NearbyError::ChecksumMismatch));
    assert_eq!(recv.join().unwrap(), Err(NearbyError::ChecksumMismatch));
    assert!(entries(&dest.0).is_empty());
}

#[test]
fn sender_rejects_an_offset_past_the_end() {
    let code = Code::generate().unwrap();
    let t = fast();
    let src = TempDir::new("off");
    let path = package(&src.0, 100, 15);
    let pkg = Package::open(&path, &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
    let (ea, eb) = pipe();
    let send = std::thread::spawn(move || transfer::send_once(Box::new(ea), &code, &pkg, "x", 512, &mut |_, _| {}, &AtomicBool::new(false), &t));
    let never = AtomicBool::new(false);
    let mut s = handshake(Box::new(eb), Role::Receiver, &code, &t, &never).unwrap();
    assert!(matches!(s.recv(None, &never).unwrap(), Record::Offer(_)));
    s.send(&Record::Accept(101)).unwrap();
    assert!(matches!(send.join().unwrap(), Err((NearbyError::Protocol(_), true))));
}

#[test]
fn several_receivers_on_one_channel_are_ambiguous() {
    let src = TempDir::new("amb");
    let path = package(&src.0, 10, 16);
    let code = Code::generate().unwrap();
    let c = |port| Candidate { channel: code.channel(), addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port) };
    let target = Target::Candidates(vec![c(9), c(10), c(9)]);
    let res = send(&code, &target, &path, "x", &mut no_progress(), &AtomicBool::new(false), &send_opts());
    assert_eq!(res, Err(NearbyError::Ambiguous(vec![c(9), c(10)])));
}

#[test]
fn a_silent_connection_frees_the_slot_after_the_hello_deadline() {
    let src = TempDir::new("silent-src");
    let dest = TempDir::new("silent-dest");
    let path = package(&src.0, 5000, 17);
    let timeouts = Timeouts { hello: Duration::from_millis(300), ..fast() };
    let r = Receiver::start(ReceiverOptions { timeouts, ..recv_opts() }).unwrap();
    let (code, target, dest_dir) = (r.code(), local(&r), dest.0.clone());
    let silent = TcpStream::connect(("127.0.0.1", r.port())).unwrap();
    let recv = std::thread::spawn(move || {
        let never = AtomicBool::new(false);
        r.wait_offer(&never, Duration::from_secs(10)).unwrap().accept(&dest_dir, &mut no_progress(), &never)
    });
    std::thread::sleep(Duration::from_millis(50));
    let outcome = send(&code, &target, &path, "x", &mut no_progress(), &AtomicBool::new(false), &send_opts()).unwrap();
    assert!(outcome.attempts >= 2, "the first try should have met the busy slot: {outcome:?}");
    recv.join().unwrap().unwrap();
    drop(silent);
}

#[test]
fn queued_connections_are_turned_away_when_waiting_stops() {
    let r = Receiver::start(recv_opts()).unwrap();
    let mut early = TcpStream::connect(("127.0.0.1", r.port())).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.wait_offer(&AtomicBool::new(true), Duration::from_secs(1)).err(), Some(NearbyError::Cancelled));
    early.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut hello = [0u8; 10];
    early.read_exact(&mut hello).unwrap();
    assert_eq!(hello[9], wire::ROLE_BUSY);
}

#[test]
fn a_fake_receiver_that_closes_after_our_confirm_is_unconfirmed() {
    let src = TempDir::new("fake");
    let path = package(&src.0, 10, 18);
    let code = Code::generate().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let fake = std::thread::spawn(move || {
        let mut count = 0;
        let mut rng = Rng(77);
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    count += 1;
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    s.write_all(&hello(wire::ROLE_RECEIVER, &mut rng, true)).unwrap();
                    let mut buf = [0u8; 43 + 5 + 48];
                    let _ = s.read_exact(&mut buf);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        count
    });
    let target = Target::Candidates(vec![Candidate { channel: code.channel(), addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port) }]);
    let res = send(&code, &target, &path, "x", &mut no_progress(), &AtomicBool::new(false), &send_opts());
    assert_eq!(res, Err(NearbyError::Unconfirmed(code.channel())));
    stop.store(true, Ordering::Relaxed);
    assert_eq!(fake.join().unwrap(), 1, "no silent retry");
}

#[test]
fn hidden_characters_in_names_and_messages() {
    let o = Offer { computer: "Studio\u{202E}gpj".into(), name: "n".into(), size: 1, sha256: "0".repeat(64) };
    assert!(matches!(o.validate(), Err(NearbyError::Protocol(_))));
    let o = Offer { computer: "c".into(), name: "a\u{200B}b".into(), size: 1, sha256: "0".repeat(64) };
    assert!(o.validate().is_err());
    assert!(is_format_char('\u{FEFF}') && is_format_char('\u{E0041}') && !is_format_char('é'));
    assert_eq!(transfer::peer_error("bad\u{202E}thing"), NearbyError::Peer("bad?thing".into()));
}
