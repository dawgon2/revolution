//! Both sides of one transfer over an authenticated [`Session`] (no sockets here).

use super::wire::{self, MAX_COMPUTER_CHARS, MAX_DATA, MAX_NAME_BYTES, Offer, Record, Role, Session, Stream, Wait, handshake};
use super::{Code, MAX_PACKAGE_BYTES, NearbyError, Progress, Result, Timeouts};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Error record texts both sides understand.
const MSG_CANCELLED: &str = "cancelled";
const MSG_BUSY: &str = "busy";
const MSG_CHECKSUM: &str = "checksum";

/// How long a side that stopped keeps reading so its Error record reaches a peer still writing.
const DRAIN: Duration = Duration::from_secs(2);

fn io_err(what: &str, e: std::io::Error) -> NearbyError {
    NearbyError::Io(format!("{what}: {e}"))
}

/// Maps the other side's Error record to our error.
pub(crate) fn peer_error(msg: &str) -> NearbyError {
    match msg {
        MSG_CANCELLED => NearbyError::PeerCancelled,
        MSG_BUSY => NearbyError::Busy,
        MSG_CHECKSUM => NearbyError::ChecksumMismatch,
        other => NearbyError::Peer(other.chars().map(|c| if super::is_hidden_char(c) { '?' } else { c }).take(200).collect()),
    }
}

/// An authenticated sender's offer, waiting for Accept or Decline.
pub struct Pending {
    pub session: Session,
    pub offer: Offer,
    pub sha: [u8; 32],
    pub received_at: Instant,
}

pub enum Served {
    Offer(Box<Pending>),
    /// A resumed connection offered a different package (declined automatically).
    DeclinedOther,
}

/// Receiver: handshake, then the offer. `resume_sha` is the package this code already accepted.
pub fn serve(stream: Box<dyn Stream>, code: &Code, t: &Timeouts, resume_sha: Option<[u8; 32]>, cancel: &AtomicBool) -> Result<Served> {
    let mut session = handshake(stream, Role::Receiver, code, t, cancel)?;
    let offer = match session.recv(None, cancel)? {
        Record::Offer(o) => o,
        Record::Error(m) => return Err(peer_error(&m)),
        _ => return Err(NearbyError::Protocol("expected an offer".into())),
    };
    let sha = offer.validate()?;
    if resume_sha.is_some_and(|prev| prev != sha) {
        let _ = session.send(&Record::Decline);
        return Ok(Served::DeclinedOther);
    }
    Ok(Served::Offer(Box::new(Pending { session, offer, sha, received_at: Instant::now() })))
}

/// Receiver: answers Decline.
pub fn decline(p: &mut Pending) -> Result<()> {
    p.session.send(&Record::Decline)
}

/// Receiver: answers Accept and writes the package to `<dest>/<sha>.part` (continuing an
/// existing one), checks the SHA-256 and renames it to `<dest>/<sha>.revpkg`.
/// A resumable error keeps the `.part`; every other error deletes it.
pub fn receive_into(p: &mut Pending, dest: &Path, progress: Progress, cancel: &AtomicBool, t: &Timeouts) -> Result<PathBuf> {
    if p.received_at.elapsed() > t.accept_prompt {
        return Err(NearbyError::Timeout);
    }
    let hex = wire::hex(&p.sha);
    let part = dest.join(format!("{hex}.part"));
    let result = receive_part(p, dest, &part, progress, cancel);
    match result {
        Ok(()) => {}
        Err(e) => {
            if !e.is_resumable() {
                let _ = fs::remove_file(&part);
            }
            match &e {
                NearbyError::Cancelled => {
                    let _ = p.session.send(&Record::Error(MSG_CANCELLED.into()));
                    p.session.drain(DRAIN);
                }
                NearbyError::ChecksumMismatch => {
                    let _ = p.session.send(&Record::Error(MSG_CHECKSUM.into()));
                }
                _ => {}
            }
            return Err(e);
        }
    }
    let done = dest.join(format!("{hex}.revpkg"));
    if done.exists() {
        let _ = fs::remove_file(&done);
    }
    if let Err(e) = fs::rename(&part, &done) {
        let _ = fs::remove_file(&part);
        return Err(io_err("could not save the package", e));
    }
    let _ = p.session.send(&Record::Done);
    Ok(done)
}

fn receive_part(p: &mut Pending, dest: &Path, part: &Path, progress: Progress, cancel: &AtomicBool) -> Result<()> {
    let size = p.offer.size;
    fs::create_dir_all(dest).map_err(|e| io_err("could not create the inbox folder", e))?;
    let mut file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(part).map_err(|e| io_err("could not create the package file", e))?;
    let mut hasher = Sha256::new();
    let existing = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut offset = if existing <= size { hash_prefix(&mut file, existing, &mut hasher, cancel)? } else { 0 };
    if offset != existing {
        hasher = Sha256::new();
        offset = 0;
    }
    file.set_len(offset).map_err(|e| io_err("could not prepare the package file", e))?;
    file.seek(SeekFrom::Start(offset)).map_err(|e| io_err("could not prepare the package file", e))?;
    p.session.send(&Record::Accept(offset))?;
    let mut total = offset;
    progress(total, size);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(NearbyError::Cancelled);
        }
        match p.session.recv(None, cancel)? {
            Record::Data(d) => {
                let len = u64::try_from(d.len()).unwrap_or(u64::MAX);
                total = total.checked_add(len).filter(|n| *n <= size).ok_or_else(|| NearbyError::Protocol("more data than offered".into()))?;
                file.write_all(&d).map_err(|e| io_err("could not write the package", e))?;
                hasher.update(&d);
                progress(total, size);
            }
            Record::Done => break,
            Record::Error(m) => return Err(peer_error(&m)),
            _ => return Err(NearbyError::Protocol("unexpected record during the transfer".into())),
        }
    }
    if total != size {
        return Err(NearbyError::Protocol("the transfer ended early".into()));
    }
    file.sync_all().map_err(|e| io_err("could not save the package", e))?;
    let got: [u8; 32] = hasher.finalize().into();
    if got != p.sha {
        return Err(NearbyError::ChecksumMismatch);
    }
    Ok(())
}

/// Feeds the first `len` bytes of `file` to `hasher`; returns how many it could read.
fn hash_prefix(file: &mut File, len: u64, hasher: &mut Sha256, cancel: &AtomicBool) -> Result<u64> {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Ok(0);
    }
    let mut buf = vec![0u8; MAX_DATA];
    let mut done = 0u64;
    while done < len {
        if cancel.load(Ordering::Relaxed) {
            return Err(NearbyError::Cancelled);
        }
        let want = usize::try_from(len.saturating_sub(done)).unwrap_or(usize::MAX).min(buf.len());
        let chunk = buf.get_mut(..want).unwrap_or_default();
        match file.read(chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                hasher.update(chunk.get(..n).unwrap_or_default());
                done = done.saturating_add(u64::try_from(n).unwrap_or(0));
            }
        }
    }
    Ok(done)
}

/// What the sender sends.
#[derive(Clone, Debug)]
pub struct Package {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub sha: [u8; 32],
}

impl Package {
    /// Reads the file's size and SHA-256.
    pub fn open(path: &Path, progress: Progress, cancel: &AtomicBool) -> Result<Package> {
        let missing = || NearbyError::NotFound(format!("The package {} wasn't found.", path.display()));
        let meta = fs::metadata(path).map_err(|_| missing())?;
        if !meta.is_file() {
            return Err(missing());
        }
        let size = meta.len();
        if size > MAX_PACKAGE_BYTES {
            return Err(NearbyError::TooLarge("the package is larger than 4 TB".into()));
        }
        let mut file = File::open(path).map_err(|e| io_err("could not open the package", e))?;
        let mut hasher = Sha256::new();
        let done = hash_prefix(&mut file, size, &mut hasher, cancel)?;
        if done != size {
            return Err(NearbyError::Io("could not read the whole package".into()));
        }
        progress(0, size);
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let clean: String = file_name.chars().filter(|c| !super::is_hidden_char(*c)).collect();
        let name = wire::truncate_bytes(&clean, MAX_NAME_BYTES).to_string();
        Ok(Package { path: path.to_path_buf(), name, size, sha: hasher.finalize().into() })
    }
}

/// Sender: one connection. Returns the offset the receiver continued from. The `bool` in the
/// error says whether the key exchange had succeeded (so the receiver is the real one).
pub fn send_once(
    stream: Box<dyn Stream>,
    code: &Code,
    pkg: &Package,
    computer: &str,
    chunk: usize,
    progress: Progress,
    cancel: &AtomicBool,
    t: &Timeouts,
) -> std::result::Result<u64, (NearbyError, bool)> {
    let mut session = handshake(stream, Role::Sender, code, t, cancel).map_err(|e| (e, false))?;
    send_authenticated(&mut session, pkg, computer, chunk, progress, cancel, t).map_err(|e| {
        if e == NearbyError::Cancelled {
            let _ = session.send(&Record::Error(MSG_CANCELLED.into()));
        }
        (e, true)
    })
}

fn send_authenticated(
    session: &mut Session,
    pkg: &Package,
    computer: &str,
    chunk: usize,
    progress: Progress,
    cancel: &AtomicBool,
    t: &Timeouts,
) -> Result<u64> {
    let clean: String = computer.chars().filter(|c| !super::is_hidden_char(*c)).take(MAX_COMPUTER_CHARS).collect();
    let offer = Offer { computer: clean, name: pkg.name.clone(), size: pkg.size, sha256: wire::hex(&pkg.sha) };
    session.send(&Record::Offer(offer))?;
    let offset = match session.recv(Some(Wait::Until(super::after(t.accept_prompt))), cancel)? {
        Record::Accept(off) if off <= pkg.size => off,
        Record::Accept(_) => return Err(NearbyError::Protocol("the receiver asked for a bad offset".into())),
        Record::Decline => return Err(NearbyError::Declined),
        Record::Error(m) => return Err(peer_error(&m)),
        _ => return Err(NearbyError::Protocol("expected Accept or Decline".into())),
    };
    let mut file = File::open(&pkg.path).map_err(|e| io_err("could not open the package", e))?;
    file.seek(SeekFrom::Start(offset)).map_err(|e| io_err("could not read the package", e))?;
    let mut buf = vec![0u8; chunk.clamp(1, MAX_DATA)];
    let mut sent = offset;
    progress(sent, pkg.size);
    while sent < pkg.size {
        if cancel.load(Ordering::Relaxed) {
            return Err(NearbyError::Cancelled);
        }
        let want = usize::try_from(pkg.size.saturating_sub(sent)).unwrap_or(usize::MAX).min(buf.len());
        let part = buf.get_mut(..want).unwrap_or_default();
        let n = file.read(part).map_err(|e| io_err("could not read the package", e))?;
        if n == 0 {
            return Err(NearbyError::Io("the package changed while it was being sent".into()));
        }
        let data = part.get(..n).unwrap_or_default();
        if let Err(e) = session.send_data(data) {
            // The receiver may have stopped on purpose: look for its Error record.
            if let Ok(Some(Record::Error(m))) = session.poll(cancel) {
                return Err(peer_error(&m));
            }
            return Err(e);
        }
        sent = sent.saturating_add(u64::try_from(n).unwrap_or(0));
        progress(sent, pkg.size);
        match session.poll(cancel)? {
            None => {}
            Some(Record::Error(m)) => return Err(peer_error(&m)),
            Some(_) => return Err(NearbyError::Protocol("unexpected record during the transfer".into())),
        }
    }
    session.send(&Record::Done)?;
    match session.recv(None, cancel)? {
        Record::Done => Ok(offset),
        Record::Error(m) => Err(peer_error(&m)),
        _ => Err(NearbyError::Protocol("expected the receiver's confirmation".into())),
    }
}
