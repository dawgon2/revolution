//! Sending a package to a nearby computer without a server ([ADR 0008]).
//!
//! The receiver shows a [`Code`] (`7-velvet-river-lamp`) and waits ([`Receiver`]); the sender
//! types it, finds the receiver by its channel number ([`discover`], or a typed address) and
//! [`send`]s a `.revpkg`. The words authenticate a SPAKE2 key exchange, so nobody else on the
//! network can read, change or guess their way into a transfer. Nothing is written before the
//! receiver accepts; a dropped connection resumes from the `.part` file.
//!
//! The protocol core ([`wire`], `transfer`) runs over any byte stream; TCP, mDNS and UDP are thin
//! layers on top. Everything is blocking `std::net` with timeouts, meant for worker threads.
//!
//! [ADR 0008]: ../../../../docs/adr/0008-nearby-transfer.md

mod code;
mod discovery;
mod net;
mod transfer;
pub mod wire;
pub mod words;

#[cfg(test)]
mod tests;

pub use code::{Code, MAX_CHANNEL};
pub use discovery::{Candidate, DEFAULT_PORT, MAX_CANDIDATES, PROBE_PORT, SERVICE_TYPE, discover, list_receivers};
pub use net::{IncomingOffer, MAX_SEND_CANDIDATES, Receiver, ReceiverOptions, SendOptions, SendOutcome, Target, send};

use std::time::Duration;

/// The largest package that can be sent (4 TiB).
pub const MAX_PACKAGE_BYTES: u64 = 4 << 40;

/// What can go wrong sending or receiving. The messages are written for the person at the
/// computer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NearbyError {
    #[error("That isn't a valid code. It looks like 7-velvet-river-lamp: a number and three words.")]
    BadCode,
    #[error("The code didn't match. Check it on the other computer and try again.")]
    WrongCode,
    #[error("The other computer declined the transfer.")]
    Declined,
    #[error("The other computer is busy with another transfer. Try again in a moment.")]
    Busy,
    #[error("This code can't be used any more. Ask for a new one.")]
    Expired,
    #[error("Cancelled.")]
    Cancelled,
    #[error(
        "A computer on channel {0} didn't confirm the code. If this keeps happening, someone else on the network may be answering; ask the receiver for a new code."
    )]
    Unconfirmed(u8),
    #[error("{} computers are waiting on this channel. Pick the right one.", .0.len())]
    Ambiguous(Vec<Candidate>),
    #[error("The other computer cancelled the transfer.")]
    PeerCancelled,
    #[error("The other computer stopped answering.")]
    Timeout,
    #[error("The connection was lost ({0}).")]
    ConnectionLost(String),
    /// The connection couldn't be opened (refused, or dropped by a firewall).
    #[error("Couldn't reach {0}. A firewall on the other computer may be blocking it: allow Revolution there, or set a fixed port and open it.")]
    Unreachable(String),
    #[error("{0}")]
    Io(String),
    #[error("The other computer sent something unexpected ({0}).")]
    Protocol(String),
    #[error("The data was changed on the way, so the transfer stopped.")]
    Tampered,
    #[error("Too large: {0}.")]
    TooLarge(String),
    #[error("The received package is damaged (its checksum doesn't match), so it was deleted.")]
    ChecksumMismatch,
    #[error("{0}")]
    NotFound(String),
    #[error("The other computer reported a problem: {0}")]
    Peer(String),
}

impl NearbyError {
    /// Whether trying again with the same code may continue the transfer (a dropped
    /// connection or a peer that stopped answering).
    pub fn is_resumable(&self) -> bool {
        matches!(self, NearbyError::ConnectionLost(_) | NearbyError::Timeout | NearbyError::Unreachable(_))
    }
}

pub type Result<T> = std::result::Result<T, NearbyError>;

/// Progress callback: bytes done so far, total bytes.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// How long each stage may take (ADR 0008 §10). Tests shorten them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// The other side's hello and key exchange message (a silent connection holds the
    /// receiver's single slot only this long).
    pub hello: Duration,
    /// Hello, key exchange and confirmation together.
    pub handshake: Duration,
    /// No bytes at all for this long during a transfer.
    pub idle: Duration,
    /// The receiver's Accept / Decline prompt.
    pub accept_prompt: Duration,
    /// How long a receiver's code is valid.
    pub code_lifetime: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            hello: Duration::from_secs(5),
            handshake: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            accept_prompt: Duration::from_secs(120),
            code_lifetime: Duration::from_secs(600),
        }
    }
}

/// `Instant::now() + d`, saturating far in the future instead of overflowing.
pub(crate) fn after(d: Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(d).or_else(|| now.checked_add(Duration::from_secs(1 << 30))).unwrap_or(now)
}

/// Unicode format characters (category Cf: bidi overrides, zero-width marks, tags …), which
/// can make a name display as something else.
pub(crate) fn is_format_char(c: char) -> bool {
    matches!(
        u32::from(c),
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Whether a character is unsafe to show in a name or message.
pub(crate) fn is_hidden_char(c: char) -> bool {
    c.is_control() || is_format_char(c)
}

/// Runs `f` on a named thread; a panic inside ends only that thread.
pub(crate) fn spawn_guarded(name: &str, f: impl FnOnce() + Send + 'static) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        })
        .map_err(|e| NearbyError::Io(format!("could not start a worker thread: {e}")))
}
