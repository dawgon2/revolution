//! The WebDAV backend (RFC 4918, byte ranges per RFC 7233): Nextcloud, ownCloud, Synology,
//! Apache / nginx WebDAV.
//!
//! - `list` / `stat`: PROPFIND (Depth 1 / 0), the multistatus parsed with roxmltree. Every
//!   `href` the server returns is percent-decoded and checked as a [`RemotePath`] under the
//!   location's root; anything outside the listed folder, or not a valid path, is skipped.
//! - `read_range`: GET with `Range`; a server that ignores ranges (200) is handled by skipping.
//! - `write`: PUT to a temporary sibling, then MOVE over the target, so a failed upload never
//!   leaves a truncated file under the real name.
//! - `create_new`: PUT with `If-None-Match: *` (atomic on the server: lock files).
//! - `delete` refuses folders (a WebDAV DELETE on a folder is recursive).

use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read};
use std::sync::atomic::{AtomicU64, Ordering};

use ureq::{AsSendBody, SendBody};

use crate::http::{self, basic_auth, encode_segment, parse_http_date, percent_decode, split_url, status_error, transport_error};
use crate::{Entry, Kind, Location, MAX_LIST_ENTRIES, MAX_READ_BYTES, Progress, RemoteError, RemotePath, RemoteStore, Result};

/// The largest PROPFIND reply read (a listing of [`MAX_LIST_ENTRIES`] files fits easily).
pub const MAX_PROPFIND_BYTES: u64 = 64 << 20;
/// The most XML nodes parsed from one PROPFIND reply.
const MAX_XML_NODES: u32 = 2_000_000;
/// A server that ignores `Range` sends the whole file from the start; we skip at most this many
/// bytes to reach the asked offset before giving up.
const MAX_IGNORED_RANGE_SKIP: u64 = 4 << 30;
/// ETags longer than this are dropped.
const MAX_ETAG_CHARS: usize = 1024;

const PROPFIND_BODY: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<d:propfind xmlns:d="DAV:"><d:prop>"#,
    r#"<d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getetag/>"#,
    r#"</d:prop></d:propfind>"#
);

const DAV: &str = "DAV:";

/// A WebDAV location.
pub struct WebDav {
    /// `scheme://authority`.
    origin: String,
    /// The decoded segments of the URL's path plus the location's root: everything lives below.
    base: Vec<String>,
    /// `/` + the decoded base segments + `/`: what a server `href` must start with.
    base_decoded: String,
    user: String,
    /// The `Authorization` header (holds the secret: never printed).
    auth: Option<String>,
    agent: ureq::Agent,
}

impl fmt::Debug for WebDav {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebDav")
            .field("origin", &self.origin)
            .field("base", &self.base_decoded)
            .field("user", &self.user)
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// One `<response>` of a multistatus reply.
#[derive(Debug, Clone)]
struct Raw {
    href: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
    etag: Option<String>,
}

enum Propfind {
    Found(Vec<Raw>),
    /// 3xx: the server wants the other form (with / without a trailing slash).
    Redirect,
}

impl WebDav {
    /// Connects (lazily: nothing is sent yet) to `location` (`url` + `root`), signing in with
    /// `location.user` and `secret` (HTTP Basic; none when both are empty).
    pub fn new(location: &Location, secret: &str) -> Result<Self> {
        if location.kind != Kind::WebDav {
            return Err(RemoteError::BadPath(format!("{} is not a WebDAV location", location.name)));
        }
        let (origin, path) = split_url(location.url.trim())?;
        let mut base = Vec::new();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            let seg = percent_decode(seg).unwrap_or_else(|| seg.to_string());
            if seg == "." || seg == ".." || seg.contains('/') || seg.chars().any(char::is_control) {
                return Err(RemoteError::BadPath(format!("{}: bad folder {seg:?} in the address", location.url)));
            }
            base.push(seg);
        }
        let root = RemotePath::parse(&location.root)?;
        base.extend(root.segments().map(str::to_string));
        let base_decoded = if base.is_empty() { "/".to_string() } else { format!("/{}/", base.join("/")) };
        let auth = (!location.user.is_empty() || !secret.is_empty()).then(|| basic_auth(&location.user, secret));
        Ok(WebDav { origin, base, base_decoded, user: location.user.clone(), auth, agent: http::agent() })
    }

    /// The absolute URL of `path` (folders get a trailing slash).
    fn url(&self, path: &RemotePath, folder: bool) -> String {
        let mut out = self.origin.clone();
        for seg in self.base.iter().map(String::as_str).chain(path.segments()) {
            out.push('/');
            out.push_str(&encode_segment(seg));
        }
        if folder || (self.base.is_empty() && path.is_root()) {
            out.push('/');
        }
        out
    }

    /// A server `href` (absolute URL or absolute path, percent-encoded) as a checked path under
    /// the location, or `None` when it is outside it or not a valid path.
    fn href_path(&self, href: &str) -> Option<RemotePath> {
        let href = href.trim();
        let lower = href.get(..8).unwrap_or(href).to_ascii_lowercase();
        let path = if lower.starts_with("http://") || lower.starts_with("https://") {
            // an absolute URL must name this server
            let (origin, path) = split_url(href).ok()?;
            if !origin.eq_ignore_ascii_case(&self.origin) {
                return None;
            }
            path
        } else {
            href.to_string()
        };
        if !path.starts_with('/') {
            return None;
        }
        let decoded = percent_decode(&path)?;
        let rest = match decoded.strip_prefix(self.base_decoded.as_str()) {
            Some(rest) => rest,
            // the root itself, without its trailing slash
            None if decoded == self.base_decoded.trim_end_matches('/') => "",
            None => return None,
        };
        // `RemotePath::parse` trims slashes at both ends; refuse a leading one here (`//x`).
        if rest.starts_with('/') {
            return None;
        }
        RemotePath::parse(rest).ok()
    }

    fn send(&self, method: &str, url: &str, headers: &[(&str, &str)], body: impl AsSendBody, what: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut b = ureq::http::Request::builder().method(method).uri(url);
        if let Some(auth) = &self.auth {
            b = b.header("Authorization", auth.as_str());
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b.body(body).map_err(|e| RemoteError::BadPath(format!("{what}: {e}")))?;
        self.agent.run(req).map_err(|e| transport_error(e, what))
    }

    fn propfind(&self, url: &str, depth: u8, what: &str) -> Result<Propfind> {
        let depth = if depth == 0 { "0" } else { "1" };
        let headers = [("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")];
        let mut resp = self.send("PROPFIND", url, &headers, PROPFIND_BODY.as_bytes(), what)?;
        match resp.status().as_u16() {
            207 => {
                let bytes = http::read_body(resp.body_mut(), MAX_PROPFIND_BYTES, what)?;
                parse_multistatus(&bytes, what).map(Propfind::Found)
            }
            301 | 302 | 307 | 308 => Ok(Propfind::Redirect),
            s => Err(status_error(s, what)),
        }
    }

    fn entry(&self, path: RemotePath, raw: &Raw) -> Entry {
        Entry { path, is_dir: raw.is_dir, size: if raw.is_dir { 0 } else { raw.size }, modified: raw.modified, etag: raw.etag.clone() }
    }

    /// Creates `path`'s missing parent folders.
    fn make_parents(&self, path: &RemotePath) -> Result<()> {
        match path.parent() {
            Some(parent) if !parent.is_root() => self.make_dir(&parent),
            _ => Ok(()),
        }
    }

    fn delete_quietly(&self, path: &RemotePath) {
        let _ = self.send("DELETE", &self.url(path, false), &[], (), "DELETE (clean-up)");
    }

    /// Stats `path`, refusing a folder; `Ok(false)` when nothing is there.
    fn file_exists(&self, path: &RemotePath) -> Result<bool> {
        match self.stat(path) {
            Ok(e) if e.is_dir => Err(RemoteError::AlreadyExists(format!("{path} is a folder"))),
            Ok(_) => Ok(true),
            Err(RemoteError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn move_file(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        let what = format!("MOVE {from} → {to}");
        let dest = self.url(to, false);
        let resp = self.send("MOVE", &self.url(from, false), &[("Destination", dest.as_str()), ("Overwrite", "T")], (), &what)?;
        match resp.status().as_u16() {
            200 | 201 | 204 => Ok(()),
            s => Err(status_error(s, &what)),
        }
    }
}

/// A unique-enough suffix for temporary upload names.
fn random_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    format!("{:016x}", h.finish())
}

/// `.<name>.part-<random>` next to `path` (the name shortened to keep the segment legal).
fn temp_path(path: &RemotePath) -> Result<RemotePath> {
    let parent = path.parent().ok_or_else(|| RemoteError::BadPath("can't write the root".into()))?;
    let name = path.name();
    let mut cut = name.len().min(200);
    while !name.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    parent.join(&format!(".{}.part-{}", name.get(..cut).unwrap_or_default(), random_suffix()))
}

fn is_dav(n: &roxmltree::Node, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == Some(DAV)
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| is_dav(c, name))
}

fn text(n: roxmltree::Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>().trim().to_string()
}

/// `HTTP/1.1 200 OK` → 200.
fn status_code(n: roxmltree::Node) -> Option<u16> {
    text(n).split_whitespace().nth(1)?.parse().ok()
}

/// Parses a 207 Multi-Status body (at most [`MAX_LIST_ENTRIES`] + 1 responses: the folder itself
/// is one of them).
fn parse_multistatus(bytes: &[u8], what: &str) -> Result<Vec<Raw>> {
    let s = std::str::from_utf8(bytes).map_err(|_| RemoteError::Protocol(format!("{what}: the listing is not UTF-8")))?;
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_XML_NODES, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(s, opts).map_err(|e| RemoteError::Protocol(format!("{what}: bad XML: {e}")))?;
    let root = doc.root_element();
    if !is_dav(&root, "multistatus") {
        return Err(RemoteError::Protocol(format!("{what}: not a WebDAV multistatus reply")));
    }
    let mut out = Vec::new();
    for resp in root.children().filter(|c| is_dav(c, "response")) {
        if out.len() > MAX_LIST_ENTRIES {
            break;
        }
        let Some(href) = child(resp, "href").map(text) else { continue };
        if child(resp, "status").and_then(status_code).is_some_and(|s| !(200..300).contains(&s)) {
            continue;
        }
        let mut raw = Raw { href, is_dir: false, size: 0, modified: None, etag: None };
        let mut found = false;
        for ps in resp.children().filter(|c| is_dav(c, "propstat")) {
            if child(ps, "status").and_then(status_code) != Some(200) {
                continue;
            }
            let Some(prop) = child(ps, "prop") else { continue };
            found = true;
            if let Some(rt) = child(prop, "resourcetype") {
                raw.is_dir = child(rt, "collection").is_some();
            }
            if let Some(len) = child(prop, "getcontentlength") {
                raw.size = text(len).parse().unwrap_or(0);
            }
            if let Some(lm) = child(prop, "getlastmodified") {
                raw.modified = parse_http_date(&text(lm));
            }
            if let Some(tag) = child(prop, "getetag") {
                let tag = text(tag);
                raw.etag = (!tag.is_empty() && tag.chars().count() <= MAX_ETAG_CHARS).then_some(tag);
            }
        }
        if found {
            out.push(raw);
        }
    }
    Ok(out)
}

/// The upload body: counts bytes, reports progress (false cancels), and never sends more than
/// `size` bytes. What went wrong is kept in `failure` (ureq only sees an I/O error).
struct Upload<'a, 'p> {
    inner: &'a mut dyn Read,
    size: u64,
    sent: u64,
    progress: Progress<'p>,
    failure: Option<RemoteError>,
}

impl Read for Upload<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("upload aborted"));
        }
        let remaining = self.size.saturating_sub(self.sent);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let room = usize::try_from(remaining).unwrap_or(usize::MAX).min(buf.len());
        let chunk = buf.get_mut(..room).unwrap_or_default();
        let n = match self.inner.read(chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
            Err(e) => {
                let msg = e.to_string();
                self.failure = Some(RemoteError::Io(e));
                return Err(io::Error::other(msg));
            }
        };
        if n == 0 {
            self.failure = Some(RemoteError::Protocol(format!("{} bytes instead of the announced {}", self.sent, self.size)));
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "upload source ended early"));
        }
        self.sent = self.sent.saturating_add(n as u64);
        if !(self.progress)(self.sent) {
            self.failure = Some(RemoteError::Cancelled);
            return Err(io::Error::other("cancelled"));
        }
        Ok(n)
    }
}

impl WebDav {
    fn put_temp(&self, temp: &RemotePath, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        let what = format!("PUT {temp}");
        let len = size.to_string();
        let mut upload = Upload { inner: data, size, sent: 0, progress, failure: None };
        let result = self.send(
            "PUT",
            &self.url(temp, false),
            &[("Content-Length", len.as_str()), ("Content-Type", "application/octet-stream")],
            SendBody::from_reader(&mut upload),
            &what,
        );
        if let Some(failure) = upload.failure.take() {
            return Err(failure);
        }
        let resp = result?;
        match resp.status().as_u16() {
            200 | 201 | 204 => {}
            s => return Err(status_error(s, &what)),
        }
        if upload.sent != size {
            return Err(RemoteError::Protocol(format!("{} bytes instead of the announced {size}", upload.sent)));
        }
        // exactly `size`: the source must be finished
        let mut probe = [0u8; 1];
        loop {
            match upload.inner.read(&mut probe) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(RemoteError::Protocol(format!("more than the announced {size} bytes"))),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(RemoteError::Io(e)),
            }
        }
    }
}

impl RemoteStore for WebDav {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let what = format!("PROPFIND {dir}");
        let Propfind::Found(raws) = self.propfind(&self.url(dir, true), 1, &what)? else {
            return Err(RemoteError::Protocol(format!("{what}: redirected")));
        };
        let mut out = Vec::new();
        for raw in &raws {
            let Some(path) = self.href_path(&raw.href) else { continue };
            if &path == dir {
                if !raw.is_dir {
                    return Err(RemoteError::NotFound(format!("{dir} is not a folder")));
                }
                continue;
            }
            if path.parent().as_ref() != Some(dir) || out.len() >= MAX_LIST_ENTRIES {
                continue;
            }
            out.push(self.entry(path, raw));
        }
        Ok(out)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry> {
        let what = format!("PROPFIND {path}");
        let raws = match self.propfind(&self.url(path, path.is_root()), 0, &what)? {
            Propfind::Found(r) => r,
            Propfind::Redirect => match self.propfind(&self.url(path, true), 0, &what)? {
                Propfind::Found(r) => r,
                Propfind::Redirect => return Err(RemoteError::Protocol(format!("{what}: redirected"))),
            },
        };
        let raw = match raws.iter().find(|r| self.href_path(&r.href).as_ref() == Some(path)) {
            Some(r) => r,
            // Depth 0 answers for exactly this resource (a proxy may rewrite its href)
            None if raws.len() == 1 => raws.first().ok_or_else(|| RemoteError::NotFound(path.to_string()))?,
            None => return Err(RemoteError::Protocol(format!("{what}: the reply doesn't describe {path}"))),
        };
        Ok(self.entry(path.clone(), raw))
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(RemoteError::TooLarge(format!("a read of {len} bytes")));
        }
        if len == 0 {
            return self.stat(path).map(|_| Vec::new());
        }
        let what = format!("GET {path}");
        let range = match offset.checked_add(len - 1) {
            Some(end) => format!("bytes={offset}-{end}"),
            None => format!("bytes={offset}-"),
        };
        let mut resp = self.send("GET", &self.url(path, false), &[("Range", range.as_str())], (), &what)?;
        match resp.status().as_u16() {
            206 => {
                let start = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().strip_prefix("bytes ")?.split('-').next()?.trim().parse::<u64>().ok());
                if start.is_some_and(|s| s != offset) {
                    return Err(RemoteError::Protocol(format!("{what}: the server sent a different range")));
                }
                http::read_up_to(&mut resp.body_mut().as_reader(), len, &what)
            }
            200 => {
                if offset > MAX_IGNORED_RANGE_SKIP {
                    return Err(RemoteError::Protocol(format!("{what}: the server ignores byte ranges")));
                }
                let mut reader = resp.body_mut().as_reader();
                if !http::skip(&mut reader, offset, &what)? {
                    return Ok(Vec::new());
                }
                http::read_up_to(&mut reader, len, &what)
            }
            416 => Ok(Vec::new()),
            s => Err(status_error(s, &what)),
        }
    }

    fn write(&self, path: &RemotePath, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        if !self.file_exists(path)? {
            self.make_parents(path)?;
        }
        let temp = temp_path(path)?;
        let result = self.put_temp(&temp, data, size, progress).and_then(|()| self.move_file(&temp, path));
        if result.is_err() {
            self.delete_quietly(&temp);
        }
        result
    }

    fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let what = format!("PUT {path} (if absent)");
        let url = self.url(path, false);
        let headers = [("If-None-Match", "*"), ("Content-Type", "application/octet-stream")];
        let mut made_parents = false;
        loop {
            let resp = self.send("PUT", &url, &headers, bytes, &what)?;
            match resp.status().as_u16() {
                200 | 201 | 204 => return Ok(()),
                412 | 405 => return Err(RemoteError::AlreadyExists(path.to_string())),
                // RFC 4918 §9.7.1: a missing parent folder
                409 if !made_parents => {
                    self.make_parents(path)?;
                    made_parents = true;
                }
                s => return Err(status_error(s, &what)),
            }
        }
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        if self.stat(from)?.is_dir {
            return Err(RemoteError::Forbidden(format!("{from} is a folder")));
        }
        if from == to {
            return Ok(());
        }
        if !self.file_exists(to)? {
            self.make_parents(to)?;
        }
        self.move_file(from, to)
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        // a WebDAV DELETE on a folder deletes everything in it: never send one
        if path.is_root() {
            return Err(RemoteError::Forbidden(format!("{path} is a folder")));
        }
        let entry = self.stat(path)?;
        if entry.is_dir {
            return Err(RemoteError::Forbidden(format!("{path} is a folder")));
        }
        let what = format!("DELETE {path}");
        // If-Match: only the file we just looked at (not a folder made there in between)
        let headers: Vec<(&str, &str)> =
            entry.etag.as_deref().filter(|t| t.bytes().all(|b| (0x20..0x7f).contains(&b))).map(|t| ("If-Match", t)).into_iter().collect();
        let resp = self.send("DELETE", &self.url(path, false), &headers, (), &what)?;
        match resp.status().as_u16() {
            200 | 202 | 204 => Ok(()),
            412 => Err(RemoteError::Protocol(format!("{what}: changed on the server, try again"))),
            s => Err(status_error(s, &what)),
        }
    }

    fn make_dir(&self, dir: &RemotePath) -> Result<()> {
        if dir.is_root() {
            return Ok(());
        }
        match self.stat(dir) {
            Ok(e) if e.is_dir => return Ok(()),
            Ok(_) => return Err(RemoteError::AlreadyExists(format!("{dir} is a file"))),
            Err(RemoteError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let mut cur = RemotePath::root();
        let mut last_existed = false;
        for seg in dir.segments() {
            cur = cur.join(seg)?;
            let what = format!("MKCOL {cur}");
            let resp = self.send("MKCOL", &self.url(&cur, true), &[], (), &what)?;
            last_existed = match resp.status().as_u16() {
                200 | 201 | 204 => false,
                // already there (405 per RFC 4918 §9.3.1; some servers redirect to the slash form)
                405 | 301 | 302 | 307 | 308 => true,
                409 => return Err(RemoteError::AlreadyExists(format!("{cur}: a parent folder is a file or missing"))),
                s => return Err(status_error(s, &what)),
            };
        }
        if last_existed && !self.stat(dir)?.is_dir {
            return Err(RemoteError::AlreadyExists(format!("{dir} is a file")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dav(url: &str, root: &str) -> WebDav {
        let loc =
            Location { id: "x".into(), name: "x".into(), kind: Kind::WebDav, url: url.into(), user: "me".into(), root: root.into(), region: String::new() };
        WebDav::new(&loc, "hunter2").unwrap()
    }

    #[test]
    fn urls_and_hrefs() {
        let d = dav("https://h/remote.php/dav/files/John%20Doe", "films/2026");
        assert_eq!(d.url(&RemotePath::parse("a b/c.rev").unwrap(), false), "https://h/remote.php/dav/files/John%20Doe/films/2026/a%20b/c.rev");
        assert_eq!(d.url(&RemotePath::root(), true), "https://h/remote.php/dav/files/John%20Doe/films/2026/");
        let p = |s: &str| d.href_path(s).map(|p| p.as_str().to_string());
        assert_eq!(p("/remote.php/dav/files/John%20Doe/films/2026/a%20b/").as_deref(), Some("a b"));
        assert_eq!(p("HTTPS://H/remote.php/dav/files/John Doe/films/2026/x").as_deref(), Some("x"));
        assert_eq!(p("https://evil/remote.php/dav/files/John%20Doe/films/2026/x"), None);
        assert_eq!(p("/remote.php/dav/files/John%20Doe/films/2026").as_deref(), Some(""));
        for bad in [
            "/remote.php/dav/files/John%20Doe/films/2026/../../../etc/passwd",
            "/remote.php/dav/files/John%20Doe/films/2026/a/%2e%2e/b",
            "/remote.php/dav/files/John%20Doe/films/2026//x",
            "/remote.php/dav/files/other/x",
            "relative/x",
            "/remote.php/dav/files/John%20Doe/films/2026/%ZZ",
            "/remote.php/dav/files/John%20Doe/films/2026/a%00b",
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
        let root = dav("http://h:8080", "");
        assert_eq!(root.url(&RemotePath::root(), false), "http://h:8080/");
        assert_eq!(root.href_path("/a").unwrap().as_str(), "a");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let d = dav("https://h/", "");
        let s = format!("{d:?}");
        assert!(!s.contains("hunter2") && !s.contains(&http::base64(b"me:hunter2")), "{s}");
        assert!(s.contains("redacted"));
    }

    #[test]
    fn bad_locations_are_refused() {
        let mut loc = Location {
            id: "x".into(),
            name: "x".into(),
            kind: Kind::WebDav,
            url: "https://h/../x".into(),
            user: String::new(),
            root: String::new(),
            region: String::new(),
        };
        assert!(WebDav::new(&loc, "").is_err());
        loc.url = "https://h/".into();
        loc.root = "a/../b".into();
        assert!(WebDav::new(&loc, "").is_err());
        loc.root = String::new();
        loc.kind = Kind::S3;
        assert!(WebDav::new(&loc, "").is_err());
        loc.kind = Kind::WebDav;
        assert!(WebDav::new(&loc, "").unwrap().auth.is_none());
    }

    #[test]
    fn multistatus_with_any_prefix() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:x="urn:other">
 <D:response><D:href>/a/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <response xmlns="DAV:"><href>/a/f.mov</href>
   <propstat><prop><resourcetype/><getcontentlength> 42 </getcontentlength><getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</getlastmodified><getetag>"e1"</getetag></prop><status>HTTP/1.1 200 OK</status></propstat>
   <propstat><prop><x:quota/></prop><status>HTTP/1.1 404 Not Found</status></propstat>
 </response>
 <D:response><D:href>/a/gone</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>
 <D:response><D:href>/a/junk</D:href><D:propstat><D:prop><D:getcontentlength>-5</D:getcontentlength><D:getlastmodified>yesterday</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#;
        let r = parse_multistatus(xml.as_bytes(), "t").unwrap();
        assert_eq!(r.len(), 3);
        assert!(r[0].is_dir);
        assert_eq!((r[1].size, r[1].modified, r[1].etag.as_deref()), (42, Some(784_111_777), Some("\"e1\"")));
        assert_eq!((r[2].size, r[2].modified), (0, None));
        for bad in ["", "<x", "<multistatus/>", "<!DOCTYPE x [<!ENTITY a \"b\">]><D:multistatus xmlns:D=\"DAV:\"/>", "\u{ff}"] {
            assert!(parse_multistatus(bad.as_bytes(), "t").is_err(), "{bad}");
        }
        assert!(parse_multistatus(&[0xff, 0xfe], "t").is_err());
    }

    #[test]
    fn temp_names_stay_legal() {
        let long = "é".repeat(127);
        let p = RemotePath::parse(&format!("dir/{long}")).unwrap();
        let t = temp_path(&p).unwrap();
        assert_eq!(t.parent(), p.parent());
        assert!(t.name().starts_with(".é") && t.name().contains(".part-"));
        assert_ne!(temp_path(&p).unwrap(), t);
        assert!(temp_path(&RemotePath::root()).is_err());
    }
}
