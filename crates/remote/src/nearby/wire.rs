//! The wire protocol over any byte stream (ADR 0008 §5–6): hello, symmetric SPAKE2, key
//! confirmation, then encrypted records.
//!
//! Hello (each side, 43 bytes): `REVNEAR1`, version (1), role (1 sender, 2 receiver), the
//! 33-byte SPAKE2 message. A receiver that can't take a sender answers with only the first ten
//! bytes and a role of [`ROLE_BUSY`] or [`ROLE_GONE`], then closes.
//!
//! Record: `u32` big-endian ciphertext length, the kind byte, the ChaCha20-Poly1305 ciphertext.
//! The kind byte travels in the clear but is the associated data, so changing it fails the
//! record. The nonce is a per-direction 64-bit counter that is never sent: a replayed, dropped
//! or reordered record fails too.
//!
//! Confirmation: the sender sends its tag first; the receiver checks it before sending its own
//! (or a plaintext Reject frame), so a stranger who guessed learns nothing without spending the
//! guess. Each tag is HMAC-SHA256 over the transcript (both hellos) with its own key.

use super::{Code, MAX_PACKAGE_BYTES, NearbyError, Timeouts};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MAGIC: &[u8; 8] = b"REVNEAR1";
pub const VERSION: u8 = 1;
pub const ROLE_SENDER: u8 = 1;
pub const ROLE_RECEIVER: u8 = 2;
/// "Busy with another sender."
pub const ROLE_BUSY: u8 = 0x10;
/// "This code can't be used any more."
pub const ROLE_GONE: u8 = 0x11;
const HEADER_LEN: usize = 10;
const SPAKE_MSG_LEN: usize = 33;
const HELLO_LEN: usize = HEADER_LEN + SPAKE_MSG_LEN;

/// Largest plaintext of one Data record.
pub const MAX_DATA: usize = 1 << 20;
const TAG_LEN: usize = 16;
/// Largest Offer JSON.
pub const MAX_OFFER_JSON: usize = 4096;
/// Largest Error message.
pub const MAX_ERROR_MSG: usize = 512;
/// Longest sender computer name (characters).
pub const MAX_COMPUTER_CHARS: usize = 64;
/// Longest package file name (bytes).
pub const MAX_NAME_BYTES: usize = 255;

pub const KIND_CONFIRM: u8 = 1;
pub const KIND_OFFER: u8 = 2;
pub const KIND_ACCEPT: u8 = 3;
pub const KIND_DECLINE: u8 = 4;
pub const KIND_DATA: u8 = 5;
pub const KIND_DONE: u8 = 6;
pub const KIND_ERROR: u8 = 7;
/// Plaintext, zero-length: "your confirmation failed" (only in place of the receiver's Confirm).
const KIND_REJECT: u8 = 0xFF;

/// How long one blocking read waits before the loop looks at cancel and deadlines.
pub const POLL: Duration = Duration::from_millis(200);

/// A byte stream the protocol runs over (TCP in the product, in-memory pipes in tests).
pub trait Stream: Read + Write + Send {
    /// How long one `read` may block before failing with `WouldBlock` / `TimedOut`.
    fn set_poll(&mut self, _wait: Duration) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for TcpStream {
    fn set_poll(&mut self, wait: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(wait.max(Duration::from_millis(1))))
    }
}

/// Which side of the transfer we are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Sender => ROLE_SENDER,
            Role::Receiver => ROLE_RECEIVER,
        }
    }
}

/// How long a read may take.
#[derive(Clone, Copy, Debug)]
pub enum Wait {
    /// Fails at this instant whatever arrives.
    Until(Instant),
    /// Fails when nothing arrives for this long.
    Idle(Duration),
}

pub(crate) fn lost(e: io::Error) -> NearbyError {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => NearbyError::Timeout,
        _ => NearbyError::ConnectionLost(e.to_string()),
    }
}

/// Fills `buf` from the stream, honouring the wait and the cancel flag. Bounded: every pass
/// either makes progress or checks the deadline.
pub(crate) fn read_full(s: &mut dyn Stream, buf: &mut [u8], wait: Wait, cancel: &AtomicBool) -> Result<(), NearbyError> {
    let mut done = 0usize;
    let mut deadline = match wait {
        Wait::Until(t) => t,
        Wait::Idle(d) => super::after(d),
    };
    while done < buf.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(NearbyError::Cancelled);
        }
        let rest = buf.get_mut(done..).ok_or_else(|| NearbyError::Protocol("read past the buffer".into()))?;
        match s.read(rest) {
            Ok(0) => return Err(NearbyError::ConnectionLost("the other computer closed the connection".into())),
            Ok(n) => {
                done = done.saturating_add(n);
                if let Wait::Idle(d) = wait {
                    deadline = super::after(d);
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(lost(e)),
        }
        if done < buf.len() && Instant::now() >= deadline {
            return Err(NearbyError::Timeout);
        }
    }
    Ok(())
}

fn write_all(s: &mut dyn Stream, bytes: &[u8]) -> Result<(), NearbyError> {
    s.write_all(bytes).map_err(lost)?;
    s.flush().map_err(lost)
}

/// What a sender offers (the Offer record's JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub computer: String,
    pub name: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the whole package.
    pub sha256: String,
}

impl Offer {
    /// Checks every field (names capped, no control characters, size capped, hash shape).
    pub fn validate(&self) -> Result<[u8; 32], NearbyError> {
        if self.computer.chars().count() > MAX_COMPUTER_CHARS {
            return Err(NearbyError::TooLarge("the sending computer's name is too long".into()));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(NearbyError::TooLarge("the package name is too long".into()));
        }
        if self.computer.chars().chain(self.name.chars()).any(super::is_hidden_char) {
            return Err(NearbyError::Protocol("a name holds control or hidden formatting characters".into()));
        }
        if self.size > MAX_PACKAGE_BYTES {
            return Err(NearbyError::TooLarge("the package is larger than 4 TB".into()));
        }
        parse_hex32(&self.sha256).ok_or_else(|| NearbyError::Protocol("the offer's checksum is malformed".into()))
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let pair = s.get(i.checked_mul(2)?..i.checked_mul(2)?.checked_add(2)?)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a character boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// One decoded record.
#[derive(Debug, PartialEq, Eq)]
pub enum Record {
    Confirm(Vec<u8>),
    Offer(Offer),
    Accept(u64),
    Decline,
    Data(Vec<u8>),
    Done,
    Error(String),
}

fn max_plain(kind: u8) -> Option<usize> {
    match kind {
        KIND_CONFIRM => Some(32),
        KIND_OFFER => Some(MAX_OFFER_JSON),
        KIND_ACCEPT => Some(8),
        KIND_DECLINE | KIND_DONE => Some(0),
        KIND_DATA => Some(MAX_DATA),
        KIND_ERROR => Some(MAX_ERROR_MSG),
        _ => None,
    }
}

fn decode(kind: u8, plain: Vec<u8>) -> Result<Record, NearbyError> {
    let bad = |what: &str| NearbyError::Protocol(format!("malformed {what} record"));
    match kind {
        KIND_CONFIRM if plain.len() == 32 => Ok(Record::Confirm(plain)),
        KIND_OFFER => {
            let offer: Offer = serde_json::from_slice(&plain).map_err(|_| bad("offer"))?;
            offer.validate()?;
            Ok(Record::Offer(offer))
        }
        KIND_ACCEPT => {
            let b: [u8; 8] = plain.as_slice().try_into().map_err(|_| bad("accept"))?;
            Ok(Record::Accept(u64::from_be_bytes(b)))
        }
        KIND_DECLINE if plain.is_empty() => Ok(Record::Decline),
        KIND_DONE if plain.is_empty() => Ok(Record::Done),
        KIND_DATA if !plain.is_empty() => Ok(Record::Data(plain)),
        KIND_ERROR => Ok(Record::Error(String::from_utf8_lossy(&plain).into_owned())),
        _ => Err(bad("unexpected")),
    }
}

/// An authenticated, encrypted connection.
pub struct Session {
    stream: Box<dyn Stream>,
    send_key: ChaCha20Poly1305,
    recv_key: ChaCha20Poly1305,
    send_ctr: u64,
    recv_ctr: u64,
    idle: Duration,
}

fn nonce(ctr: u64) -> Nonce {
    let mut n = [0u8; 12];
    if let Some(tail) = n.get_mut(4..) {
        tail.copy_from_slice(&ctr.to_be_bytes());
    }
    Nonce::from(n)
}

impl Session {
    /// Encrypts and sends one record of `kind` with `plain` (the caller keeps within the caps).
    pub fn send_raw(&mut self, kind: u8, plain: &[u8]) -> Result<(), NearbyError> {
        let ct = self
            .send_key
            .encrypt(&nonce(self.send_ctr), Payload { msg: plain, aad: &[kind] })
            .map_err(|_| NearbyError::Protocol("could not encrypt a record".into()))?;
        self.send_ctr = self.send_ctr.checked_add(1).ok_or_else(|| NearbyError::Protocol("too many records".into()))?;
        let len = u32::try_from(ct.len()).map_err(|_| NearbyError::TooLarge("record too large".into()))?;
        let mut frame = Vec::with_capacity(ct.len().saturating_add(5));
        frame.extend_from_slice(&len.to_be_bytes());
        frame.push(kind);
        frame.extend_from_slice(&ct);
        write_all(self.stream.as_mut(), &frame)
    }

    pub fn send(&mut self, rec: &Record) -> Result<(), NearbyError> {
        match rec {
            Record::Confirm(tag) => self.send_raw(KIND_CONFIRM, tag),
            Record::Offer(o) => {
                let json = serde_json::to_vec(o).map_err(|e| NearbyError::Protocol(e.to_string()))?;
                if json.len() > MAX_OFFER_JSON {
                    return Err(NearbyError::TooLarge("the offer is too large".into()));
                }
                self.send_raw(KIND_OFFER, &json)
            }
            Record::Accept(off) => self.send_raw(KIND_ACCEPT, &off.to_be_bytes()),
            Record::Decline => self.send_raw(KIND_DECLINE, &[]),
            Record::Data(d) => self.send_data(d),
            Record::Done => self.send_raw(KIND_DONE, &[]),
            Record::Error(m) => self.send_raw(KIND_ERROR, truncate_bytes(m, MAX_ERROR_MSG).as_bytes()),
        }
    }

    pub fn send_data(&mut self, data: &[u8]) -> Result<(), NearbyError> {
        if data.is_empty() || data.len() > MAX_DATA {
            return Err(NearbyError::Protocol("bad data chunk size".into()));
        }
        self.send_raw(KIND_DATA, data)
    }

    /// The next record (kind, plaintext), with the Reject frame passed through as `KIND_REJECT`.
    fn recv_raw(&mut self, wait: Wait, cancel: &AtomicBool) -> Result<(u8, Vec<u8>), NearbyError> {
        let mut hdr = [0u8; 5];
        read_full(self.stream.as_mut(), &mut hdr, wait, cancel)?;
        self.recv_after_header(hdr, wait, cancel)
    }

    fn recv_after_header(&mut self, hdr: [u8; 5], wait: Wait, cancel: &AtomicBool) -> Result<(u8, Vec<u8>), NearbyError> {
        let [a, b, c, d, kind] = hdr;
        let len = usize::try_from(u32::from_be_bytes([a, b, c, d])).unwrap_or(usize::MAX);
        if kind == KIND_REJECT && len == 0 {
            return Ok((KIND_REJECT, Vec::new()));
        }
        let max = max_plain(kind).ok_or_else(|| NearbyError::Protocol(format!("unknown record kind {kind}")))?;
        if len > max.saturating_add(TAG_LEN) {
            return Err(NearbyError::TooLarge(format!("a record of {len} bytes")));
        }
        if len < TAG_LEN {
            return Err(NearbyError::Protocol("a record is too short".into()));
        }
        let mut ct = vec![0u8; len];
        read_full(self.stream.as_mut(), &mut ct, wait, cancel)?;
        let plain = self.recv_key.decrypt(&nonce(self.recv_ctr), Payload { msg: &ct, aad: &[kind] }).map_err(|_| NearbyError::Tampered)?;
        self.recv_ctr = self.recv_ctr.checked_add(1).ok_or_else(|| NearbyError::Protocol("too many records".into()))?;
        Ok((kind, plain))
    }

    /// The next record; `None` for `wait` means the idle timeout.
    pub fn recv(&mut self, wait: Option<Wait>, cancel: &AtomicBool) -> Result<Record, NearbyError> {
        let wait = wait.unwrap_or(Wait::Idle(self.idle));
        let (kind, plain) = self.recv_raw(wait, cancel)?;
        if kind == KIND_REJECT {
            return Err(NearbyError::Protocol("unexpected reject".into()));
        }
        decode(kind, plain)
    }

    /// A record if one has started arriving, without waiting (about a millisecond).
    pub fn poll(&mut self, cancel: &AtomicBool) -> Result<Option<Record>, NearbyError> {
        let mut hdr = [0u8; 5];
        self.stream.set_poll(Duration::from_millis(1)).map_err(lost)?;
        let first = self.stream.read(&mut hdr);
        self.stream.set_poll(POLL).map_err(lost)?;
        let n = match first {
            Ok(0) => return Err(NearbyError::ConnectionLost("the other computer closed the connection".into())),
            Ok(n) => n.min(hdr.len()),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => return Ok(None),
            Err(e) => return Err(lost(e)),
        };
        let wait = Wait::Idle(self.idle);
        if let Some(rest) = hdr.get_mut(n..) {
            read_full(self.stream.as_mut(), rest, wait, cancel)?;
        }
        let (kind, plain) = self.recv_after_header(hdr, wait, cancel)?;
        if kind == KIND_REJECT {
            return Err(NearbyError::Protocol("unexpected reject".into()));
        }
        decode(kind, plain).map(Some)
    }

    /// Reads and discards whatever arrives until the peer closes or `for_at_most` passes (so
    /// a final Error record reaches a peer that is still writing, instead of a reset).
    pub fn drain(&mut self, for_at_most: Duration) {
        let until = super::after(for_at_most);
        let mut buf = [0u8; 16 * 1024];
        while Instant::now() < until {
            match self.stream.read(&mut buf) {
                Ok(0) => return,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
                Err(_) => return,
            }
        }
    }

    #[cfg(test)]
    pub fn stream_mut(&mut self) -> &mut dyn Stream {
        self.stream.as_mut()
    }

    #[cfg(test)]
    pub fn pair_for_tests(a: Box<dyn Stream>, b: Box<dyn Stream>) -> (Session, Session) {
        let k1 = ChaCha20Poly1305::new(&Key::from([1u8; 32]));
        let k2 = ChaCha20Poly1305::new(&Key::from([2u8; 32]));
        let idle = Duration::from_secs(2);
        (
            Session { stream: a, send_key: k1.clone(), recv_key: k2.clone(), send_ctr: 0, recv_ctr: 0, idle },
            Session { stream: b, send_key: k2, recv_key: k1, send_ctr: 0, recv_ctr: 0, idle },
        )
    }
}

type HmacSha256 = Hmac<Sha256>;

fn expand(hk: &Hkdf<Sha256>, info: &[u8]) -> Result<[u8; 32], NearbyError> {
    let mut k = [0u8; 32];
    hk.expand(info, &mut k).map_err(|_| NearbyError::Protocol("key derivation failed".into()))?;
    Ok(k)
}

fn tag(key: &[u8; 32], label: &[u8], transcript: &[u8]) -> Result<HmacSha256, NearbyError> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| NearbyError::Protocol("bad confirmation key".into()))?;
    mac.update(label);
    mac.update(transcript);
    Ok(mac)
}

/// Writes the short "can't take you" hello (busy or code gone) and returns.
pub fn reply_unavailable(s: &mut dyn Stream, role: u8) {
    let mut hello = Vec::with_capacity(HEADER_LEN);
    hello.extend_from_slice(MAGIC);
    hello.push(VERSION);
    hello.push(role);
    let _ = write_all(s, &hello);
}

/// Runs the hello, key exchange and confirmation. A wrong code is [`NearbyError::WrongCode`]
/// (on the receiver only once the sender's confirmation arrived and failed: only then has the
/// guess been spent).
pub fn handshake(mut stream: Box<dyn Stream>, role: Role, code: &Code, t: &Timeouts, cancel: &AtomicBool) -> Result<Session, NearbyError> {
    let deadline = Wait::Until(super::after(t.handshake));
    let hello_deadline = Wait::Until(super::after(t.hello.min(t.handshake)));
    let identity = format!("revolution-nearby-v1/{}", code.channel());
    let (spake, msg) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.normalised()), &Identity::new(identity.as_bytes()));
    if msg.len() != SPAKE_MSG_LEN {
        return Err(NearbyError::Protocol("unexpected key exchange message size".into()));
    }
    let mut mine = Vec::with_capacity(HELLO_LEN);
    mine.extend_from_slice(MAGIC);
    mine.push(VERSION);
    mine.push(role.byte());
    mine.extend_from_slice(&msg);
    write_all(stream.as_mut(), &mine)?;

    let mut theirs = [0u8; HELLO_LEN];
    let (head, spake_msg) = theirs.split_at_mut(HEADER_LEN);
    read_full(stream.as_mut(), head, hello_deadline, cancel)?;
    if head.get(..8) != Some(MAGIC.as_slice()) {
        return Err(NearbyError::Protocol("that is not a Revolution computer".into()));
    }
    if head.get(8) != Some(&VERSION) {
        return Err(NearbyError::Protocol("the other computer runs a different version of Revolution".into()));
    }
    let want = match role {
        Role::Sender => ROLE_RECEIVER,
        Role::Receiver => ROLE_SENDER,
    };
    match head.get(9).copied() {
        Some(r) if r == want => {}
        Some(ROLE_BUSY) if role == Role::Sender => return Err(NearbyError::Busy),
        Some(ROLE_GONE) if role == Role::Sender => return Err(NearbyError::Expired),
        _ => return Err(NearbyError::Protocol("unexpected hello".into())),
    }
    read_full(stream.as_mut(), spake_msg, hello_deadline, cancel)?;
    let key = spake.finish(spake_msg).map_err(|_| NearbyError::Protocol("bad key exchange message".into()))?;

    let (sender_hello, receiver_hello) = match role {
        Role::Sender => (mine.as_slice(), theirs.as_slice()),
        Role::Receiver => (theirs.as_slice(), mine.as_slice()),
    };
    let mut h = Sha256::new();
    h.update(sender_hello);
    h.update(receiver_hello);
    let transcript = h.finalize();
    let hk = Hkdf::<Sha256>::new(Some(&transcript), &key);
    let k_s2r = expand(&hk, b"revolution-nearby-v1 sender to receiver record key")?;
    let k_r2s = expand(&hk, b"revolution-nearby-v1 receiver to sender record key")?;
    let k_cs = expand(&hk, b"revolution-nearby-v1 sender confirm key")?;
    let k_cr = expand(&hk, b"revolution-nearby-v1 receiver confirm key")?;
    let (send_k, recv_k) = match role {
        Role::Sender => (k_s2r, k_r2s),
        Role::Receiver => (k_r2s, k_s2r),
    };
    let mut session = Session {
        stream,
        send_key: ChaCha20Poly1305::new(&Key::from(send_k)),
        recv_key: ChaCha20Poly1305::new(&Key::from(recv_k)),
        send_ctr: 0,
        recv_ctr: 0,
        idle: t.idle,
    };
    let sender_tag = tag(&k_cs, b"sender", &transcript)?;
    let receiver_tag = tag(&k_cr, b"receiver", &transcript)?;
    match role {
        Role::Sender => {
            // From here on the peer may have spent a guess on our tag: anything but its
            // verified confirmation stops the send (no silent move to another candidate).
            let unconfirmed = NearbyError::Unconfirmed(code.channel());
            if let Err(e) = session.send_raw(KIND_CONFIRM, &sender_tag.finalize().into_bytes()) {
                return Err(if e == NearbyError::Cancelled { e } else { unconfirmed });
            }
            match session.recv_raw(deadline, cancel) {
                Ok((KIND_REJECT, _)) | Err(NearbyError::Tampered) => Err(NearbyError::WrongCode),
                Ok((KIND_CONFIRM, plain)) => receiver_tag.verify_slice(&plain).map(|()| session).map_err(|_| NearbyError::WrongCode),
                Err(NearbyError::Cancelled) => Err(NearbyError::Cancelled),
                Ok(_) | Err(_) => Err(unconfirmed),
            }
        }
        Role::Receiver => {
            let ok = match session.recv_raw(deadline, cancel) {
                Ok((KIND_CONFIRM, plain)) => sender_tag.verify_slice(&plain).is_ok(),
                Ok(_) | Err(NearbyError::Tampered) => false,
                Err(e) => return Err(e),
            };
            if !ok {
                let _ = write_all(session.stream.as_mut(), &[0, 0, 0, 0, KIND_REJECT]);
                return Err(NearbyError::WrongCode);
            }
            session.send_raw(KIND_CONFIRM, &receiver_tag.finalize().into_bytes())?;
            Ok(session)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let b: [u8; 32] = std::array::from_fn(|i| (i * 7) as u8);
        assert_eq!(parse_hex32(&hex(&b)), Some(b));
        assert_eq!(parse_hex32("AB"), None);
        assert_eq!(parse_hex32(&"G".repeat(64)), None);
        assert_eq!(parse_hex32(&hex(&b).to_uppercase()), None);
    }

    #[test]
    fn truncation_keeps_char_boundaries() {
        assert_eq!(truncate_bytes("héllo", 2), "h");
        assert_eq!(truncate_bytes("héllo", 3), "hé");
        assert_eq!(truncate_bytes("abc", 10), "abc");
    }

    fn offer() -> Offer {
        Offer { computer: "Studio".into(), name: "film.revpkg".into(), size: 10, sha256: "0".repeat(64) }
    }

    #[test]
    fn offer_caps() {
        assert!(offer().validate().is_ok());
        let mut o = offer();
        o.computer = "x".repeat(65);
        assert!(matches!(o.validate(), Err(NearbyError::TooLarge(_))));
        let mut o = offer();
        o.computer = "é".repeat(64);
        assert!(o.validate().is_ok());
        let mut o = offer();
        o.name = "n".repeat(256);
        assert!(matches!(o.validate(), Err(NearbyError::TooLarge(_))));
        let mut o = offer();
        o.name = "a\nb".into();
        assert!(o.validate().is_err());
        let mut o = offer();
        o.size = MAX_PACKAGE_BYTES + 1;
        assert!(matches!(o.validate(), Err(NearbyError::TooLarge(_))));
        let mut o = offer();
        o.sha256 = "zz".into();
        assert!(o.validate().is_err());
        assert!(decode(KIND_OFFER, b"{}".to_vec()).is_err());
        assert!(decode(KIND_OFFER, b"not json".to_vec()).is_err());
        assert!(decode(KIND_ACCEPT, vec![1, 2, 3]).is_err());
        assert!(decode(KIND_DATA, vec![]).is_err());
        assert!(decode(KIND_DONE, vec![1]).is_err());
        assert!(decode(99, vec![]).is_err());
    }
}
