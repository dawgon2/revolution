//! Shared HTTP plumbing for the HTTP-based backends (WebDAV, S3-compatible): the agent (pure-Rust
//! TLS, timeouts, no automatic redirects, statuses returned rather than turned into errors),
//! status / error mapping, capped body reads and percent-encoding.

use std::io::Read;
use std::time::Duration;

use crate::{RemoteError, Result};

/// Connecting gives up after this long.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Waiting for a response's headers (after the request body is sent) gives up after this long.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// Receiving one response body (at most [`crate::MAX_READ_BYTES`]) gives up after this long.
pub const RECV_BODY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Sending one request body (a media upload) gives up after this long.
pub const SEND_BODY_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);

/// The agent every HTTP backend uses: rustls with the RustCrypto provider and the OS's
/// certificate verifier (plain `http://` works too, for LAN servers and tests), the timeouts
/// above, no redirects (a redirect is reported, never followed with our credentials), any
/// method (PROPFIND, MKCOL, MOVE), and HTTP error statuses returned as responses so the caller
/// maps them ([`status_error`]).
pub fn agent() -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .http_status_as_error(false)
        .max_redirects(0)
        .allow_non_standard_methods(true)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(RECV_BODY_TIMEOUT))
        .timeout_send_body(Some(SEND_BODY_TIMEOUT))
        .build()
        .into()
}

/// The error for an unexpected HTTP status. `what` names the request (`"PUT /a/b"`).
/// 401 → Unauthorized, 403 → Forbidden, 404/410 → NotFound, 412 → AlreadyExists (a
/// conditional write lost), 413 → TooLarge, 408 / 429 / 5xx → Network (transient: retrying or
/// resuming may succeed), everything else → Protocol.
pub fn status_error(status: u16, what: &str) -> RemoteError {
    match status {
        401 => RemoteError::Unauthorized,
        403 => RemoteError::Forbidden(what.to_string()),
        404 | 410 => RemoteError::NotFound(what.to_string()),
        412 => RemoteError::AlreadyExists(what.to_string()),
        413 => RemoteError::TooLarge(format!("{what}: the server refused the size")),
        507 => RemoteError::Network(format!("{what}: the server is out of space (507)")),
        408 => RemoteError::Network(format!("{what}: the server timed out (408)")),
        429 => RemoteError::Network(format!("{what}: the server is busy (429), try again later")),
        s @ 500..=599 => RemoteError::Network(format!("{what}: server error (HTTP {s})")),
        s => RemoteError::Protocol(format!("{what}: HTTP {s}")),
    }
}

/// The error for a failed request: connection problems and timeouts → Network (transient),
/// everything else → Protocol.
pub fn transport_error(e: ureq::Error, what: &str) -> RemoteError {
    use ureq::Error as E;
    match e {
        E::StatusCode(s) => status_error(s, what),
        E::Timeout(t) => RemoteError::Network(format!("{what}: timed out ({t})")),
        E::HostNotFound => RemoteError::Network(format!("{what}: host not found")),
        E::ConnectionFailed => RemoteError::Network(format!("{what}: connection failed")),
        E::Io(io) => RemoteError::Network(format!("{what}: {io}")),
        E::BodyExceedsLimit(n) => RemoteError::TooLarge(format!("{what}: more than {n} bytes")),
        other => RemoteError::Protocol(format!("{what}: {other}")),
    }
}

/// Reads a response body completely, refusing more than `cap` bytes ([`RemoteError::TooLarge`];
/// a larger announced `Content-Length` is refused before reading anything).
pub fn read_body(body: &mut ureq::Body, cap: u64, what: &str) -> Result<Vec<u8>> {
    if body.content_length().is_some_and(|n| n > cap) {
        return Err(RemoteError::TooLarge(format!("{what}: reply larger than {cap} bytes")));
    }
    let mut reader = body.as_reader();
    let out = read_up_to(&mut reader, cap.saturating_add(1), what)?;
    if out.len() as u64 > cap {
        return Err(RemoteError::TooLarge(format!("{what}: reply larger than {cap} bytes")));
    }
    Ok(out)
}

/// Reads at most `max` bytes (fewer at the end of the stream). A read error → Network.
pub fn read_up_to(reader: &mut dyn Read, max: u64, what: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    while (out.len() as u64) < max {
        let room = usize::try_from(max.saturating_sub(out.len() as u64)).unwrap_or(usize::MAX).min(buf.len());
        let chunk = buf.get_mut(..room).unwrap_or_default();
        let n = match reader.read(chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(RemoteError::Network(format!("{what}: {e}"))),
        };
        if n == 0 {
            break;
        }
        out.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
    Ok(out)
}

/// Reads and drops `n` bytes; false if the stream ended first.
pub fn skip(reader: &mut dyn Read, n: u64, what: &str) -> Result<bool> {
    let mut left = n;
    let mut buf = vec![0u8; 64 << 10];
    while left > 0 {
        let room = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let chunk = buf.get_mut(..room).unwrap_or_default();
        let got = match reader.read(chunk) {
            Ok(g) => g,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(RemoteError::Network(format!("{what}: {e}"))),
        };
        if got == 0 {
            return Ok(false);
        }
        left = left.saturating_sub(got as u64);
    }
    Ok(true)
}

/// The `Authorization` header value for HTTP Basic authentication (RFC 7617).
pub fn basic_auth(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

/// Standard base64 with padding (RFC 4648 §4).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |i: u32| ALPHABET.get((i & 63) as usize).map_or('=', |&b| char::from(b));
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(sym(n >> 18));
        out.push(sym(n >> 12));
        out.push(if chunk.len() > 1 { sym(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(n) } else { '=' });
    }
    out
}

/// Percent-encodes one path segment: everything except RFC 3986 unreserved characters.
pub fn encode_segment(seg: &str) -> String {
    percent_encode(seg, false)
}

/// Percent-encodes `s` keeping exactly the RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`)
/// and, when `keep_slash`, `/` (the set S3 SigV4 canonical URIs need). Escapes are uppercase hex.
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes. `None` for a malformed escape or a result that isn't UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hi = hex(*bytes.get(i.checked_add(1)?)?)?;
            let lo = hex(*bytes.get(i.checked_add(2)?)?)?;
            out.push((hi << 4) | lo);
            i = i.checked_add(3)?;
        } else {
            out.push(b);
            i = i.checked_add(1)?;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Splits an `http(s)://authority/path` URL into (`scheme://authority`, `/path`). Refuses other
/// schemes, a missing host, control characters and query strings / fragments.
pub fn split_url(url: &str) -> Result<(String, String)> {
    let lower = url.to_ascii_lowercase();
    let rest_at = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return Err(RemoteError::BadPath(format!("{url}: only http:// and https:// addresses are supported")));
    };
    if url.contains(['?', '#']) || url.chars().any(char::is_control) {
        return Err(RemoteError::BadPath(format!("{url}: unexpected characters in the address")));
    }
    let rest = url.get(rest_at..).unwrap_or_default();
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest.get(..i).unwrap_or_default(), rest.get(i..).unwrap_or("/")),
        None => (rest, "/"),
    };
    if authority.contains('@') {
        // never echo the address: it holds a password
        return Err(RemoteError::BadPath("put the user name in User, not in the address".into()));
    }
    if authority.is_empty() {
        return Err(RemoteError::BadPath(format!("{url}: no server name")));
    }
    let origin = format!("{}{authority}", url.get(..rest_at).unwrap_or_default());
    Ok((origin, path.to_string()))
}

/// Seconds since the Unix epoch for a proleptic Gregorian date and time (UTC).
pub fn unix_time(year: i64, month: u32, day: u32, h: u32, m: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 || !(1..=9999).contains(&year) {
        return None;
    }
    // days from civil (Howard Hinnant's algorithm)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(s))
}

/// Parses an HTTP date in the RFC 1123 form (`Sun, 06 Nov 1994 08:49:37 GMT`; the weekday
/// is optional). `None` for anything else.
pub fn parse_http_date(s: &str) -> Option<i64> {
    let mut parts: Vec<&str> = s.split_whitespace().take(7).collect();
    if parts.first().is_some_and(|w| w.ends_with(',')) {
        parts.remove(0);
    }
    let [day, mon, year, time, zone] = parts.as_slice() else {
        return None;
    };
    if !matches!(*zone, "GMT" | "UTC" | "+0000") {
        return None;
    }
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let lower = mon.to_ascii_lowercase();
    let month = MONTHS.iter().position(|m| *m == lower)?;
    let day: u32 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let mut hms = time.split(':');
    let h: u32 = hms.next()?.parse().ok()?;
    let m: u32 = hms.next()?.parse().ok()?;
    let sec: u32 = hms.next()?.parse().ok()?;
    if hms.next().is_some() {
        return None;
    }
    unix_time(year, u32::try_from(month).ok()?.checked_add(1)?, day, h, m, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(i.as_bytes()), o);
        }
        assert_eq!(basic_auth("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    #[test]
    fn percent_round_trip_and_hostile_escapes() {
        let s = "a b/é%?#";
        assert_eq!(encode_segment(s), "a%20b%2F%C3%A9%25%3F%23");
        assert_eq!(percent_decode(&encode_segment(s)).unwrap(), s);
        assert_eq!(percent_encode("a b/c~d-_.é", true), "a%20b/c~d-_.%C3%A9");
        assert_eq!(percent_encode("a/b+c=", false), "a%2Fb%2Bc%3D");
        for bad in ["%", "%4", "%zz", "%FF"] {
            assert!(percent_decode(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(parse_http_date("06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Tue, 29 Feb 2000 12:00:00 GMT"), Some(951_825_600));
        for bad in [
            "",
            "garbage",
            "Sun, 06 Foo 1994 08:49:37 GMT",
            "Sun, 99 Nov 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 25:00:00 GMT",
            "Sun, 06 Nov 99999999999999999999 08:49:37 GMT",
            "Sun, 06 Nov 1994 08:49 GMT",
            "Sun, 06 Nov 1994 08:49:37 PST",
        ] {
            assert_eq!(parse_http_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn urls() {
        assert_eq!(split_url("https://h:8443/dav/files/").unwrap(), ("https://h:8443".into(), "/dav/files/".into()));
        assert_eq!(split_url("HTTP://h").unwrap(), ("HTTP://h".into(), "/".into()));
        for bad in ["ftp://h/", "https:///x", "https://h/?q", "h/x", "https://h/a\nb"] {
            assert!(split_url(bad).is_err(), "{bad}");
        }
        let err = split_url("https://ana:hunter2@h/dav/").unwrap_err().to_string();
        assert!(err.contains("User") && !err.contains("hunter2"), "{err}");
        assert!(split_url("https://h/dav/a@b/").is_ok(), "an @ in the path is fine");
    }

    #[test]
    fn transient_statuses() {
        for s in [408, 429, 500, 502, 503, 504, 507, 599] {
            assert!(status_error(s, "x").is_transient(), "{s}");
        }
        for s in [400, 401, 403, 404, 405, 409, 412, 413, 416] {
            assert!(!status_error(s, "x").is_transient(), "{s}");
        }
    }
}
