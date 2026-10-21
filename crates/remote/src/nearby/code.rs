//! The short code read off one screen and typed on the other: `<channel>-<word>-<word>-<word>`.

use super::NearbyError;
use super::words;
use std::fmt;

/// The highest channel number.
pub const MAX_CHANNEL: u8 = 99;

/// The longest text [`Code::parse`] looks at (anything longer is not a code).
const MAX_TYPED_LEN: usize = 128;

/// A transfer code, e.g. `7-violet-river-lamp`. The channel (1–99) is public: discovery
/// advertises it. The three words are the secret (24 bits).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code {
    channel: u8,
    words: [u8; 3],
}

impl Code {
    /// A fresh random code.
    pub fn generate() -> Result<Code, NearbyError> {
        let mut b = [0u8; 4];
        getrandom::getrandom(&mut b).map_err(|e| NearbyError::Io(format!("no randomness: {e}")))?;
        let [c, w0, w1, w2] = b;
        Ok(Code { channel: 1 + c % MAX_CHANNEL, words: [w0, w1, w2] })
    }

    /// A code from its parts (`None` when the channel is outside 1–99).
    pub fn from_parts(channel: u8, words: [u8; 3]) -> Option<Code> {
        (1..=MAX_CHANNEL).contains(&channel).then_some(Code { channel, words })
    }

    /// Reads a typed code, forgivingly: any case, extra spaces, and `-`, space, `.` or `_` between
    /// the parts; each word is recognised by its first four letters.
    pub fn parse(typed: &str) -> Result<Code, NearbyError> {
        let bad = || NearbyError::BadCode;
        if typed.len() > MAX_TYPED_LEN {
            return Err(bad());
        }
        let lower = typed.trim().to_ascii_lowercase();
        let mut parts = lower.split(|c: char| c == '-' || c == '.' || c == '_' || c.is_whitespace()).filter(|p| !p.is_empty());
        let channel: u8 = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
        let mut words = [0u8; 3];
        for slot in &mut words {
            *slot = parts.next().and_then(words::lookup).ok_or_else(bad)?;
        }
        if parts.next().is_some() {
            return Err(bad());
        }
        Code::from_parts(channel, words).ok_or_else(bad)
    }

    /// The public channel number (1–99).
    pub fn channel(&self) -> u8 {
        self.channel
    }

    /// The canonical text both sides feed into the key exchange (same as `Display`).
    pub(crate) fn normalised(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.words;
        write!(f, "{}-{}-{}-{}", self.channel, words::word(a), words::word(b), words::word(c))
    }
}

impl fmt::Debug for Code {
    // Never print the secret words in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({}-…)", self.channel)
    }
}

impl std::str::FromStr for Code {
    type Err = NearbyError;
    fn from_str(s: &str) -> Result<Code, NearbyError> {
        Code::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_round_trip() {
        for _ in 0..200 {
            let c = Code::generate().unwrap();
            assert!((1..=99).contains(&c.channel()));
            let text = c.to_string();
            assert_eq!(Code::parse(&text).unwrap(), c);
            assert_eq!(format!("{c:?}"), format!("Code({}-…)", c.channel()));
        }
    }

    #[test]
    fn display() {
        let idx = |w: &str| words::lookup(w).unwrap();
        let c = Code::from_parts(7, [idx("velvet"), idx("river"), idx("lamp")]).unwrap();
        assert_eq!(c.to_string(), "7-velvet-river-lamp");
        assert_eq!(c.channel(), 7);
        assert_eq!(c.normalised(), "7-velvet-river-lamp");
    }

    #[test]
    fn forgiving_parse() {
        let want = Code::parse("7-velvet-river-lamp").unwrap();
        for typed in [
            "7 velvet river lamp",
            "  7-VELVET-River-lamp ",
            "7.velv.rive.lamp",
            "07_velvets riverside   lamps",
            "7 - velvet - river - lamp",
            "7\tvelvet\nriver lamp",
        ] {
            assert_eq!(Code::parse(typed).unwrap(), want, "{typed}");
        }
    }

    #[test]
    fn bad_codes() {
        for typed in [
            "",
            "7",
            "7-velvet-river",
            "7-velvet-river-lamp-moon",
            "0-velvet-river-lamp",
            "100-velvet-river-lamp",
            "256-velvet-river-lamp",
            "x-velvet-river-lamp",
            "7-vel-river-lamp",
            "7-qqqq-river-lamp",
            "7-vélvet-river-lamp",
            "-7-velvet-river-lamp",
        ] {
            // "-7-…": the leading '-' is a separator, so this one is actually fine.
            if typed == "-7-velvet-river-lamp" {
                assert!(Code::parse(typed).is_ok());
                continue;
            }
            assert!(matches!(Code::parse(typed), Err(NearbyError::BadCode)), "{typed}");
        }
        assert!(Code::parse(&"7-velvet-river-lamp ".repeat(50)).is_err());
        assert!(Code::from_parts(0, [0; 3]).is_none());
        assert!(Code::from_parts(100, [0; 3]).is_none());
    }
}
