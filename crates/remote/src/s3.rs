//! The S3-compatible backend (Amazon S3, MinIO, Garage, …), from the public S3 REST API and
//! Signature Version 4 documentation.
//!
//! - A [`Location`]'s `root` is `bucket/optional/prefix`, its `user` the access key id; the secret
//!   key is passed to [`S3::new`] (from the keyring) and only kept in memory.
//! - Path-style addressing (`https://host/bucket/key`) by default, which MinIO and Garage need;
//!   [`S3::virtual_hosted`] switches to `https://bucket.host/key`.
//! - Folders are key prefixes. [`RemoteStore::make_dir`] writes an empty `dir/` marker object so an
//!   empty folder shows up; listing uses `ListObjectsV2` with `delimiter=/`. Like
//!   [`crate::MemoryStore`], a path that is a folder (a prefix with children, or a marker) can't
//!   be written over.
//! - Writes are all-or-nothing on S3 (no temporary names): one `PUT` up to
//!   [`MULTIPART_THRESHOLD`], a multipart upload above it. Each part is retried from memory a few
//!   times; [`S3::write_resumable`] hands back a [`MultipartState`] when the upload still fails
//!   for a transient reason, so it can be resumed later (even after a restart: the state is
//!   serde).
//! - [`RemoteStore::create_new`] uses `If-None-Match: *` (conditional writes): AWS S3 (since
//!   2024), MinIO and recent Garage support it; a server that ignores the header would make lock
//!   files racy.
//! - [`RemoteStore::rename`] is `CopyObject` + `DELETE`: not atomic. If the delete fails, both
//!   objects stay and the error is returned.
//! - Only idempotent requests (GET, HEAD, DELETE, plain PUT) are retried after a dropped
//!   connection. `create_new` and `CompleteMultipartUpload` check the result instead.
//!
//! Everything a server sends is treated as hostile: replies are size-capped, keys only become
//! [`RemotePath`]s through [`RemotePath::parse`] after stripping the location's prefix, and keys
//! outside the prefix are skipped. The secret never appears in errors or `Debug` output.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::{self, parse_http_date, percent_encode, transport_error};
use crate::{Entry, Kind, Location, MAX_LIST_ENTRIES, MAX_READ_BYTES, Progress, RemoteError, RemotePath, RemoteStore, Result};

/// Single `PUT` up to this size; a multipart upload above it.
pub const MULTIPART_THRESHOLD: u64 = 64 << 20;
/// The size of one multipart part (raised for huge files so there are at most [`MAX_PARTS`]).
pub const PART_SIZE: u64 = 64 << 20;
/// S3's smallest part (all but the last).
pub const MIN_PART_SIZE: u64 = 5 << 20;
/// S3's largest part.
pub const MAX_PART_SIZE: u64 = 5 << 30;
/// S3's most parts per upload.
pub const MAX_PARTS: u64 = 10_000;
/// S3's largest object.
pub const MAX_OBJECT_BYTES: u64 = 5 << 40;
/// The largest object [`RemoteStore::rename`] moves (one `CopyObject`).
pub const MAX_COPY_BYTES: u64 = 5 << 30;
/// The largest listing page accepted.
pub const MAX_LIST_REPLY_BYTES: u64 = 64 << 20;
/// Attempts per multipart part before the upload is handed back for resuming.
pub const PART_ATTEMPTS: u32 = 3;
/// The largest small XML reply (create / complete upload, copy result).
const MAX_SMALL_REPLY_BYTES: u64 = 1 << 20;
/// The largest error body read.
const MAX_ERROR_BYTES: u64 = 64 << 10;
/// The most listing pages followed (S3 pages hold up to 1000 entries; a server that never stops
/// paging can't loop us forever).
const MAX_LIST_PAGES: usize = MAX_LIST_ENTRIES / 1000 + 10;
/// A server that ignores `Range` makes us read and drop everything before the offset: at most
/// this much.
const MAX_IGNORED_RANGE_SKIP: u64 = 4 << 30;
/// Longest ETag / upload id / continuation token kept.
const MAX_TOKEN_BYTES: usize = 4096;
/// The slowest upload speed a body send is given time for (plus [`BASE_SEND_TIMEOUT`]).
const MIN_UPLOAD_BYTES_PER_SEC: u64 = 256 << 10;
const BASE_SEND_TIMEOUT: Duration = Duration::from_secs(60);
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

// ---------------------------------------------------------------------------------------------
// The store

/// An S3-compatible location.
pub struct S3 {
    https: bool,
    /// `host[:port]`, lower-case.
    authority: String,
    /// A path in front of everything (reverse proxies), without leading / trailing `/` ("" none).
    base_path: String,
    bucket: String,
    /// Key prefix (checked like a [`RemotePath`]), without leading / trailing `/` ("" none).
    prefix: String,
    region: String,
    access_key: String,
    secret: String,
    path_style: bool,
    multipart_threshold: u64,
    part_size: u64,
    agent: ureq::Agent,
}

impl fmt::Debug for S3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3")
            .field("endpoint", &format_args!("{}://{}/{}", if self.https { "https" } else { "http" }, self.authority, self.base_path))
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("region", &self.region)
            .field("access_key", &self.access_key)
            .field("secret", &"<redacted>")
            .field("path_style", &self.path_style)
            .finish()
    }
}

/// One finished part of a multipart upload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedPart {
    /// 1-based part number.
    pub number: u32,
    pub etag: String,
}

/// An interrupted multipart upload that [`S3::write_resumable`] can continue: the upload id and
/// the parts already on the server. Serialisable, so it can survive a restart. Parts are all
/// `part_size` bytes (the last one shorter), so the upload continues at [`Self::bytes_done`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartState {
    pub path: RemotePath,
    pub upload_id: String,
    /// The whole file's size.
    pub size: u64,
    pub part_size: u64,
    /// Parts 1..=n, in order.
    pub parts: Vec<CompletedPart>,
}

impl MultipartState {
    /// Bytes already uploaded: where the data must continue from on resume.
    pub fn bytes_done(&self) -> u64 {
        (self.parts.len() as u64).saturating_mul(self.part_size).min(self.size)
    }

    fn check(&self, path: &RemotePath, size: u64) -> Result<()> {
        if &self.path != path || self.size != size {
            return Err(RemoteError::Protocol("the saved upload is for another file or size".into()));
        }
        if self.part_size == 0 || self.part_size > MAX_PART_SIZE || self.upload_id.is_empty() || self.upload_id.len() > MAX_TOKEN_BYTES {
            return Err(RemoteError::Protocol("the saved upload is damaged".into()));
        }
        let max_parts = size.div_ceil(self.part_size).max(1);
        if self.parts.len() as u64 > max_parts.min(MAX_PARTS) {
            return Err(RemoteError::Protocol("the saved upload has too many parts".into()));
        }
        for (i, p) in self.parts.iter().enumerate() {
            if u64::from(p.number) != (i as u64).saturating_add(1) || p.etag.is_empty() || p.etag.len() > MAX_TOKEN_BYTES {
                return Err(RemoteError::Protocol("the saved upload's parts are damaged".into()));
            }
        }
        Ok(())
    }
}

/// A failed [`S3::write_resumable`]: the error, and (for a transient failure of a multipart
/// upload) what to resume from. Without `resume`, nothing is left on the server.
#[derive(Debug)]
pub struct WriteFailure {
    pub error: RemoteError,
    pub resume: Option<MultipartState>,
}

impl From<RemoteError> for WriteFailure {
    fn from(error: RemoteError) -> Self {
        WriteFailure { error, resume: None }
    }
}

impl S3 {
    /// Connects to an S3 location (no network traffic yet). `secret` is the secret access key.
    pub fn new(location: &Location, secret: &str) -> Result<Self> {
        if location.kind != Kind::S3 {
            return Err(RemoteError::BadPath("not an S3 location".into()));
        }
        let (https, authority, base_path) = parse_endpoint(&location.url)?;
        let root = location.root.trim_matches('/');
        let (bucket, prefix) = root.split_once('/').unwrap_or((root, ""));
        if bucket.is_empty() || bucket.len() > 255 || !bucket.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
            return Err(RemoteError::BadPath("the bucket name may only hold letters, digits, '.', '-' and '_'".into()));
        }
        let prefix = RemotePath::parse(prefix)?.as_str().to_string();
        let region = if location.region.trim().is_empty() { "us-east-1".to_string() } else { location.region.trim().to_string() };
        if region.len() > 64 || !region.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
            return Err(RemoteError::BadPath("the region may only hold letters, digits, '-' and '_'".into()));
        }
        let access_key = location.user.trim().to_string();
        if access_key.is_empty() || access_key.len() > 256 || !access_key.bytes().all(|b| b.is_ascii_graphic() && b != b'/' && b != b',') {
            return Err(RemoteError::BadPath("the access key id is empty or has invalid characters".into()));
        }
        if secret.is_empty() {
            return Err(RemoteError::Unauthorized);
        }
        Ok(S3 {
            https,
            authority,
            base_path,
            bucket: bucket.to_string(),
            prefix,
            region,
            access_key,
            secret: secret.to_string(),
            path_style: true,
            multipart_threshold: MULTIPART_THRESHOLD,
            part_size: PART_SIZE,
            agent: http::agent(),
        })
    }

    /// Virtual-hosted addressing (`https://bucket.host/key`) instead of path-style.
    pub fn virtual_hosted(mut self) -> Self {
        self.path_style = false;
        self
    }

    /// Tests: smaller multipart threshold and part size (S3 itself refuses parts under 5 MiB).
    #[doc(hidden)]
    pub fn set_multipart_sizes(&mut self, threshold: u64, part_size: u64) {
        self.multipart_threshold = threshold;
        self.part_size = part_size.clamp(1, MAX_PART_SIZE);
    }

    /// The object key of `path` (`prefix/path`).
    fn key(&self, path: &RemotePath) -> String {
        match (self.prefix.is_empty(), path.is_root()) {
            (true, _) => path.as_str().to_string(),
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}/{}", self.prefix, path.as_str()),
        }
    }

    /// The key prefix of everything inside `dir` ("" or ending in `/`).
    fn dir_prefix(&self, dir: &RemotePath) -> String {
        let k = self.key(dir);
        if k.is_empty() { k } else { format!("{k}/") }
    }

    /// `(authority, encoded path)` of a request on `key` ("" for the bucket).
    fn target(&self, key: &str) -> (String, String) {
        let mut path = String::new();
        if !self.base_path.is_empty() {
            path.push('/');
            path.push_str(&self.base_path);
        }
        let authority = if self.path_style {
            path.push('/');
            path.push_str(&percent_encode(&self.bucket, false));
            self.authority.clone()
        } else {
            format!("{}.{}", self.bucket, self.authority)
        };
        if !key.is_empty() || !self.path_style {
            path.push('/');
            path.push_str(&encode_key(key));
        }
        (authority, path)
    }

    /// Signs and sends one request, once. `send_timeout` overrides the body send timeout.
    fn send_once(&self, req: Req<'_>, send_timeout: Option<Duration>) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let (authority, path) = self.target(req.key);
        let payload_hash = match &req.body {
            Body::Empty => EMPTY_SHA256.to_string(),
            Body::Bytes(b) => hex(&Sha256::digest(b)),
            Body::Stream(..) => UNSIGNED_PAYLOAD.to_string(),
        };
        let amz_date = amz_date(now_secs());
        let mut headers: Vec<(String, String)> =
            vec![("host".into(), authority.clone()), ("x-amz-content-sha256".into(), payload_hash.clone()), ("x-amz-date".into(), amz_date.clone())];
        headers.extend(req.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())));
        let sig = SignInput {
            method: req.method,
            path: &path,
            query: &req.query,
            headers: &headers,
            payload_hash: &payload_hash,
            amz_date: &amz_date,
            region: &self.region,
            access_key: &self.access_key,
            secret: &self.secret,
        };
        let auth = sig.authorization();
        let query = canonical_query(&req.query);
        let uri = format!("{}://{authority}{path}{}{query}", if self.https { "https" } else { "http" }, if query.is_empty() { "" } else { "?" });
        let mut b = ureq::http::Request::builder().method(req.method).uri(uri);
        for (k, v) in &headers {
            b = b.header(k.as_str(), v.as_str());
        }
        b = b.header("authorization", auth);
        let len = match &req.body {
            // S3 wants a Content-Length on every PUT / POST, even an empty one
            Body::Empty => matches!(req.method, "PUT" | "POST").then_some(0),
            Body::Bytes(bytes) => Some(bytes.len() as u64),
            Body::Stream(_, n) => Some(*n),
        };
        if let Some(n) = len {
            b = b.header("content-length", n.to_string());
        }
        match req.body {
            Body::Empty => run(&self.agent, b.body(())?, send_timeout),
            Body::Bytes(bytes) => run(&self.agent, b.body(bytes)?, send_timeout),
            Body::Stream(reader, _) => run(&self.agent, b.body(ureq::SendBody::from_reader(reader))?, send_timeout),
        }
    }

    /// Sends a request without a streamed body. Idempotent ones (GET, HEAD, DELETE, a plain PUT)
    /// are sent once more when the connection fails (a kept-alive connection the server closed
    /// while idle); others never are.
    fn send(&self, req: Req<'_>, what: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let Req { method, key, query, headers, body } = req;
        let (bytes, has_body): (&[u8], bool) = match body {
            Body::Empty => (&[], false),
            Body::Bytes(b) => (b, true),
            Body::Stream(..) => return Err(RemoteError::Protocol("internal: a streamed body sent without its size check".into())),
        };
        let make = |query: Vec<(String, String)>, headers: Vec<(String, String)>| Req {
            method,
            key,
            query,
            headers,
            body: if has_body { Body::Bytes(bytes) } else { Body::Empty },
        };
        let idempotent = matches!(method, "GET" | "HEAD" | "DELETE" | "PUT") && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("if-none-match"));
        match self.send_once(make(query.clone(), headers.clone()), None) {
            Err(e) if idempotent && is_connection_error(&e) => self.send_once(make(query, headers), None).map_err(|e| transport_error(e, what)),
            r => r.map_err(|e| transport_error(e, what)),
        }
    }

    /// [`Self::send`], with a non-2xx status turned into an error.
    fn send_ok(&self, req: Req<'_>, what: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut resp = self.send(req, what)?;
        if resp.status().is_success() { Ok(resp) } else { Err(status_err(&mut resp, what)) }
    }

    /// One `ListObjectsV2` page.
    fn list_page(&self, prefix: &str, delimiter: bool, token: Option<&str>, max_keys: Option<u32>, what: &str) -> Result<ListPage> {
        let mut query = vec![("list-type".to_string(), "2".to_string()), ("prefix".to_string(), prefix.to_string())];
        if delimiter {
            query.push(("delimiter".into(), "/".into()));
        }
        if let Some(t) = token {
            query.push(("continuation-token".into(), t.to_string()));
        }
        if let Some(m) = max_keys {
            query.push(("max-keys".into(), m.to_string()));
        }
        let mut resp = self.send_ok(Req { method: "GET", key: "", query, headers: Vec::new(), body: Body::Empty }, what)?;
        let bytes = http::read_body(resp.body_mut(), MAX_LIST_REPLY_BYTES, what)?;
        parse_list(&bytes)
    }

    /// Whether `path` is a folder: a `path/` marker or anything under `path/`.
    fn is_folder(&self, path: &RemotePath) -> Result<bool> {
        if path.is_root() {
            return Ok(true);
        }
        let page = self.list_page(&self.dir_prefix(path), false, None, Some(1), &path.to_string())?;
        Ok(!page.contents.is_empty() || !page.prefixes.is_empty())
    }

    fn refuse_folder(&self, path: &RemotePath) -> Result<()> {
        if self.is_folder(path)? {
            return Err(RemoteError::AlreadyExists(format!("{path} is a folder")));
        }
        Ok(())
    }

    /// Turns a listed key (or common prefix) into a path directly inside `dir` (`None`: outside
    /// the location's prefix, not a valid path, or not a direct child).
    fn listed(&self, key: &str, dir: &RemotePath) -> Option<RemotePath> {
        let root = self.dir_prefix(&RemotePath::root());
        let rest = key.strip_prefix(root.as_str())?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.is_empty() || rest.starts_with('/') || rest.ends_with('/') {
            return None;
        }
        let path = RemotePath::parse(rest).ok()?;
        (path.as_str() == rest && path.parent().as_ref() == Some(dir)).then_some(path)
    }

    fn head(&self, path: &RemotePath) -> Result<Option<Entry>> {
        let what = path.to_string();
        let mut resp = self.send(Req { method: "HEAD", key: &self.key(path), query: Vec::new(), headers: Vec::new(), body: Body::Empty }, &what)?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(status_err(&mut resp, &what));
        }
        let h = resp.headers();
        let size = h.get("content-length").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
        let etag = h.get("etag").and_then(|v| v.to_str().ok()).filter(|s| s.len() <= MAX_TOKEN_BYTES).map(str::to_string);
        let modified = h.get("last-modified").and_then(|v| v.to_str().ok()).and_then(parse_http_date);
        Ok(Some(Entry { path: path.clone(), is_dir: false, size, modified, etag }))
    }

    /// Writes `path`, resuming `resume` (from [`WriteFailure::resume`]) when given. `data` must
    /// then be positioned at [`MultipartState::bytes_done`]; `progress` still reports bytes of
    /// the whole file. Files up to the multipart threshold go up in one `PUT` (never
    /// resumable). Each part is tried [`PART_ATTEMPTS`] times; when a part still fails for a
    /// transient reason (a dropped connection, a timeout, a busy server) the upload is left
    /// open on the server and its state returned. On any other failure (or cancel) it is
    /// aborted. Callers that give up on a returned state call [`S3::abort_upload`] (S3 bills
    /// open uploads' parts; a bucket lifecycle rule also cleans them up).
    pub fn write_resumable(
        &self,
        path: &RemotePath,
        data: &mut dyn Read,
        size: u64,
        progress: Progress,
        resume: Option<MultipartState>,
    ) -> std::result::Result<(), WriteFailure> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()).into());
        }
        if size > MAX_OBJECT_BYTES {
            return Err(RemoteError::TooLarge(format!("{path}: S3 objects are at most 5 TiB")).into());
        }
        let mut state = match resume {
            Some(st) => {
                st.check(path, size)?;
                st
            }
            None => {
                self.refuse_folder(path)?;
                if size <= self.multipart_threshold {
                    return self.put_single(path, data, size, progress).map_err(WriteFailure::from);
                }
                let part_size = self.part_size.max(size.div_ceil(MAX_PARTS)).clamp(1, MAX_PART_SIZE);
                let upload_id = self.create_upload(path)?;
                MultipartState { path: path.clone(), upload_id, size, part_size, parts: Vec::new() }
            }
        };
        match self.upload_parts(&mut state, data, progress) {
            Ok(()) => Ok(()),
            Err(error) if error.is_transient() => Err(WriteFailure { error, resume: Some(state) }),
            Err(error) => {
                let _ = self.abort_upload(&state);
                Err(WriteFailure { error, resume: None })
            }
        }
    }

    /// Drops an unfinished multipart upload and its parts.
    pub fn abort_upload(&self, state: &MultipartState) -> Result<()> {
        let query = vec![("uploadId".to_string(), state.upload_id.clone())];
        match self.send_ok(Req { method: "DELETE", key: &self.key(&state.path), query, headers: Vec::new(), body: Body::Empty }, &state.path.to_string()) {
            Ok(_) | Err(RemoteError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn put_single(&self, path: &RemotePath, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        let what = path.to_string();
        if size == 0 {
            let mut probe = [0u8; 1];
            if data.read(&mut probe)? > 0 {
                return Err(RemoteError::Protocol("more than the announced 0 bytes".into()));
            }
        }
        let mut reader = Counting { inner: data, remaining: size, done: 0, progress, check_extra: true, fail: None };
        let req = Req { method: "PUT", key: &self.key(path), query: Vec::new(), headers: Vec::new(), body: Body::Stream(&mut reader, size) };
        let r = self.send_once(req, Some(send_timeout(size)));
        let mut resp = r.map_err(|e| reader.error(e, size, &what))?;
        if !resp.status().is_success() {
            return Err(status_err(&mut resp, &what));
        }
        Ok(())
    }

    fn create_upload(&self, path: &RemotePath) -> Result<String> {
        let what = path.to_string();
        let query = vec![("uploads".to_string(), String::new())];
        // a POST: never retried (a lost reply would leave an orphan upload behind)
        let mut resp = self.send_ok(Req { method: "POST", key: &self.key(path), query, headers: Vec::new(), body: Body::Empty }, &what)?;
        let bytes = http::read_body(resp.body_mut(), MAX_SMALL_REPLY_BYTES, &what)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| RemoteError::Protocol("the upload reply is not UTF-8".into()))?;
        let doc = parse_xml(text)?;
        check_xml_error(&doc, &what)?;
        let id = doc.descendants().find(|n| n.tag_name().name() == "UploadId").and_then(|n| n.text()).map(str::trim).unwrap_or_default();
        if id.is_empty() || id.len() > MAX_TOKEN_BYTES {
            return Err(RemoteError::Protocol("the server sent no upload id".into()));
        }
        Ok(id.to_string())
    }

    /// Uploads one part from memory, trying up to [`PART_ATTEMPTS`] times on transient errors.
    fn upload_part(&self, state: &MultipartState, key: &str, number: u32, buf: &[u8], offset: u64, progress: Progress) -> Result<String> {
        let what = state.path.to_string();
        let len = buf.len() as u64;
        let mut last = RemoteError::Network(format!("{what}: part {number} not sent"));
        for _ in 0..PART_ATTEMPTS {
            let mut src = buf;
            let mut reader = Counting { inner: &mut src, remaining: len, done: offset, progress: &mut *progress, check_extra: false, fail: None };
            let query = vec![("partNumber".to_string(), number.to_string()), ("uploadId".to_string(), state.upload_id.clone())];
            let req = Req { method: "PUT", key, query, headers: Vec::new(), body: Body::Stream(&mut reader, len) };
            let error = match self.send_once(req, Some(send_timeout(len))) {
                Err(e) => reader.error(e, state.size, &what),
                Ok(mut resp) if !resp.status().is_success() => status_err(&mut resp, &what),
                Ok(resp) => {
                    let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or_default().trim().to_string();
                    if etag.is_empty() || etag.len() > MAX_TOKEN_BYTES {
                        return Err(RemoteError::Protocol("the server sent no part ETag".into()));
                    }
                    return Ok(etag);
                }
            };
            if !error.is_transient() {
                return Err(error);
            }
            last = error;
        }
        Err(last)
    }

    fn upload_parts(&self, state: &mut MultipartState, data: &mut dyn Read, progress: Progress) -> Result<()> {
        let what = state.path.to_string();
        let key = self.key(&state.path);
        let mut offset = state.bytes_done();
        let cap = usize::try_from(state.part_size.min(state.size.saturating_sub(offset))).map_err(|_| RemoteError::TooLarge("part too large".into()))?;
        let mut buf: Vec<u8> = Vec::with_capacity(cap);
        while offset < state.size {
            let len = state.part_size.min(state.size.saturating_sub(offset));
            let number = u32::try_from(state.parts.len().saturating_add(1)).map_err(|_| RemoteError::TooLarge("too many parts".into()))?;
            if u64::from(number) > MAX_PARTS {
                return Err(RemoteError::TooLarge(format!("{what}: more than {MAX_PARTS} parts")));
            }
            buf.clear();
            (&mut *data).take(len).read_to_end(&mut buf)?;
            if (buf.len() as u64) < len {
                return Err(RemoteError::Protocol(format!("{} bytes instead of the announced {}", offset.saturating_add(buf.len() as u64), state.size)));
            }
            let etag = self.upload_part(state, &key, number, &buf, offset, &mut *progress)?;
            state.parts.push(CompletedPart { number, etag });
            offset = offset.saturating_add(len);
        }
        let mut probe = [0u8; 1];
        if data.read(&mut probe)? > 0 {
            return Err(RemoteError::Protocol(format!("more than the announced {} bytes", state.size)));
        }
        self.complete_upload(state, &key)
    }

    /// `CompleteMultipartUpload`. Never re-sent: when the reply is lost, a HEAD of the key
    /// decides (present with the expected size: done).
    fn complete_upload(&self, state: &MultipartState, key: &str) -> Result<()> {
        let what = state.path.to_string();
        let mut xml = String::from("<CompleteMultipartUpload>");
        for p in &state.parts {
            let _ = write!(xml, "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>", p.number, xml_escape(&p.etag));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let query = vec![("uploadId".to_string(), state.upload_id.clone())];
        let headers = vec![("content-type".to_string(), "application/xml".to_string())];
        let mut resp = match self.send_once(Req { method: "POST", key, query, headers, body: Body::Bytes(xml.as_bytes()) }, None) {
            Ok(r) => r,
            Err(e) => {
                let err = transport_error(e, &what);
                if err.is_transient() && self.head(&state.path).ok().flatten().is_some_and(|e| e.size == state.size) {
                    return Ok(());
                }
                return Err(err);
            }
        };
        if !resp.status().is_success() {
            return Err(status_err(&mut resp, &what));
        }
        // S3 can answer 200 and still report an error in the body.
        let bytes = http::read_body(resp.body_mut(), MAX_SMALL_REPLY_BYTES, &what)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| RemoteError::Protocol("the completion reply is not UTF-8".into()))?;
        if !text.trim().is_empty() {
            check_xml_error(&parse_xml(text)?, &what)?;
        }
        Ok(())
    }
}

impl RemoteStore for S3 {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let prefix = self.dir_prefix(dir);
        let what = dir.to_string();
        let mut out: BTreeMap<RemotePath, Entry> = BTreeMap::new();
        let mut token: Option<String> = None;
        let mut seen_any = false;
        for _ in 0..MAX_LIST_PAGES {
            let page = self.list_page(&prefix, true, token.as_deref(), None, &what)?;
            seen_any |= !page.contents.is_empty() || !page.prefixes.is_empty();
            for c in &page.contents {
                if c.key.ends_with('/') {
                    if let Some(path) = self.listed(&c.key, dir) {
                        out.insert(path.clone(), Entry { path, is_dir: true, size: 0, modified: None, etag: None });
                    }
                } else if let Some(path) = self.listed(&c.key, dir) {
                    out.entry(path.clone()).or_insert(Entry { path, is_dir: false, size: c.size, modified: c.modified, etag: c.etag.clone() });
                }
            }
            for p in &page.prefixes {
                if let Some(path) = self.listed(p, dir) {
                    out.insert(path.clone(), Entry { path, is_dir: true, size: 0, modified: None, etag: None });
                }
            }
            if out.len() >= MAX_LIST_ENTRIES || !page.truncated {
                break;
            }
            match page.next {
                Some(n) if token.as_deref() != Some(n.as_str()) => token = Some(n),
                _ => return Err(RemoteError::Protocol("a truncated listing without a new continuation token".into())),
            }
        }
        if !seen_any && !dir.is_root() {
            return Err(RemoteError::NotFound(what));
        }
        Ok(out.into_values().take(MAX_LIST_ENTRIES).collect())
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry> {
        if path.is_root() {
            return Ok(Entry { path: path.clone(), is_dir: true, size: 0, modified: None, etag: None });
        }
        if let Some(e) = self.head(path)? {
            return Ok(e);
        }
        if !self.is_folder(path)? {
            return Err(RemoteError::NotFound(path.to_string()));
        }
        Ok(Entry { path: path.clone(), is_dir: true, size: 0, modified: None, etag: None })
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(RemoteError::TooLarge(format!("a read of {len} bytes")));
        }
        if path.is_root() {
            return Err(RemoteError::NotFound(path.to_string()));
        }
        if len == 0 {
            return match self.head(path)? {
                Some(_) => Ok(Vec::new()),
                None => Err(RemoteError::NotFound(path.to_string())),
            };
        }
        let what = path.to_string();
        let last = offset.saturating_add(len - 1);
        let headers = vec![("range".to_string(), format!("bytes={offset}-{last}"))];
        let mut resp = self.send(Req { method: "GET", key: &self.key(path), query: Vec::new(), headers, body: Body::Empty }, &what)?;
        match resp.status().as_u16() {
            206 => {
                if let Some(start) = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).and_then(content_range_start)
                    && start != offset
                {
                    return Err(RemoteError::Protocol(format!("asked for bytes from {offset}, got them from {start}")));
                }
                http::read_body(resp.body_mut(), len, &what).map_err(|e| match e {
                    RemoteError::TooLarge(_) => RemoteError::Protocol(format!("{what}: more than the {len} bytes asked for")),
                    e => e,
                })
            }
            200 => {
                // The server ignored the range: skip to `offset` (bounded), keep `len`.
                if offset > MAX_IGNORED_RANGE_SKIP {
                    return Err(RemoteError::Protocol(format!("{what}: the server ignores byte ranges")));
                }
                let mut body = resp.body_mut().as_reader();
                if !http::skip(&mut body, offset, &what)? {
                    return Ok(Vec::new());
                }
                http::read_up_to(&mut body, len, &what)
            }
            416 => Ok(Vec::new()),
            _ => Err(status_err(&mut resp, &what)),
        }
    }

    fn write(&self, path: &RemotePath, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        match self.write_resumable(path, data, size, progress, None) {
            Ok(()) => Ok(()),
            Err(WriteFailure { error, resume }) => {
                if let Some(st) = resume {
                    let _ = self.abort_upload(&st);
                }
                Err(error)
            }
        }
    }

    fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        self.refuse_folder(path)?;
        let what = path.to_string();
        let key = self.key(path);
        let req =
            || Req { method: "PUT", key: &key, query: Vec::new(), headers: vec![("if-none-match".to_string(), "*".to_string())], body: Body::Bytes(bytes) };
        let (mut resp, retried) = match self.send_once(req(), None) {
            Ok(r) => (r, false),
            // The reply may be lost after the server stored it: try once more, and if the
            // object exists now, it is ours when it holds our bytes.
            Err(e) if is_connection_error(&e) => (self.send_once(req(), None).map_err(|e| transport_error(e, &what))?, true),
            Err(e) => return Err(transport_error(e, &what)),
        };
        if resp.status().is_success() {
            return Ok(());
        }
        match status_err(&mut resp, &what) {
            RemoteError::AlreadyExists(_) if retried => {
                let cap = (bytes.len() as u64).saturating_add(1).min(MAX_READ_BYTES);
                if self.read_range(path, 0, cap)? == bytes { Ok(()) } else { Err(RemoteError::AlreadyExists(what)) }
            }
            e => Err(e),
        }
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        if to.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let src = self.stat(from)?;
        if src.is_dir {
            return Err(RemoteError::Forbidden(format!("{from} is a folder")));
        }
        if src.size > MAX_COPY_BYTES {
            return Err(RemoteError::TooLarge(format!("{from}: moving files over 5 GiB on S3 is not supported yet")));
        }
        if from == to {
            return Ok(());
        }
        self.refuse_folder(to)?;
        let source = format!("/{}/{}", percent_encode(&self.bucket, false), encode_key(&self.key(from)));
        let headers = vec![("x-amz-copy-source".to_string(), source)];
        let what = to.to_string();
        let mut resp = self.send_ok(Req { method: "PUT", key: &self.key(to), query: Vec::new(), headers, body: Body::Bytes(&[]) }, &what)?;
        let bytes = http::read_body(resp.body_mut(), MAX_SMALL_REPLY_BYTES, &what)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| RemoteError::Protocol("the copy reply is not UTF-8".into()))?;
        if !text.trim().is_empty() {
            check_xml_error(&parse_xml(text)?, &what)?;
        }
        // not atomic: if this fails, both copies stay and the caller sees the error
        self.send_ok(Req { method: "DELETE", key: &self.key(from), query: Vec::new(), headers: Vec::new(), body: Body::Empty }, &from.to_string())?;
        Ok(())
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        if self.stat(path)?.is_dir {
            return Err(RemoteError::Forbidden(format!("{path} is a folder")));
        }
        self.send_ok(Req { method: "DELETE", key: &self.key(path), query: Vec::new(), headers: Vec::new(), body: Body::Empty }, &path.to_string())?;
        Ok(())
    }

    fn make_dir(&self, dir: &RemotePath) -> Result<()> {
        if dir.is_root() {
            return Ok(());
        }
        if self.head(dir)?.is_some() {
            return Err(RemoteError::AlreadyExists(format!("{dir} is a file")));
        }
        let key = format!("{}/", self.key(dir));
        self.send_ok(Req { method: "PUT", key: &key, query: Vec::new(), headers: Vec::new(), body: Body::Bytes(&[]) }, &dir.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Requests and replies (the agent, transport / status mapping and capped reads are in
// `crate::http`; this adds S3's XML error codes)

enum Body<'a> {
    Empty,
    /// A small body, hashed into the signature.
    Bytes(&'a [u8]),
    /// A streamed body of exactly this many bytes (`UNSIGNED-PAYLOAD`).
    Stream(&'a mut dyn Read, u64),
}

struct Req<'a> {
    method: &'static str,
    key: &'a str,
    /// Decoded name / value pairs.
    query: Vec<(String, String)>,
    /// Extra headers (signed).
    headers: Vec<(String, String)>,
    body: Body<'a>,
}

fn run(
    agent: &ureq::Agent,
    request: ureq::http::Request<impl ureq::AsSendBody>,
    send_timeout: Option<Duration>,
) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    use ureq::RequestExt as _;
    match send_timeout {
        Some(t) => request.with_agent(agent).configure().timeout_send_body(Some(t)).build().run(),
        None => agent.run(request),
    }
}

/// The time allowed to send a body of `bytes`: a minute plus [`MIN_UPLOAD_BYTES_PER_SEC`].
fn send_timeout(bytes: u64) -> Duration {
    BASE_SEND_TIMEOUT.saturating_add(Duration::from_secs(bytes / MIN_UPLOAD_BYTES_PER_SEC))
}

/// A failure of the connection itself (before any reply): worth one retry for idempotent requests.
fn is_connection_error(e: &ureq::Error) -> bool {
    matches!(e, ureq::Error::Io(_) | ureq::Error::ConnectionFailed)
}

/// `(https, authority, base path)` of an endpoint URL.
fn parse_endpoint(url: &str) -> Result<(bool, String, String)> {
    let (origin, path) = http::split_url(url.trim())?;
    let lower = origin.to_ascii_lowercase();
    let (https, authority) = match (lower.strip_prefix("https://"), lower.strip_prefix("http://")) {
        (Some(a), _) => (true, a.to_string()),
        (None, Some(a)) => (false, a.to_string()),
        (None, None) => return Err(RemoteError::BadPath("the S3 endpoint must start with https:// or http://".into())),
    };
    if authority.len() > 255 || !authority.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']' | b'_')) {
        return Err(RemoteError::BadPath("the S3 endpoint's host name is invalid".into()));
    }
    let path = path.trim_matches('/');
    if path.len() > 1024 || !path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/')) || path.contains("//") {
        return Err(RemoteError::BadPath("the S3 endpoint's path may only hold letters, digits, '-', '_', '.', '~' and '/'".into()));
    }
    Ok((https, authority, path.to_string()))
}

/// The error of a non-2xx reply (reads at most a small error body).
fn status_err(resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> RemoteError {
    let status = resp.status().as_u16();
    let body = http::read_up_to(&mut resp.body_mut().as_reader(), MAX_ERROR_BYTES, what).unwrap_or_default();
    let code = std::str::from_utf8(&body).ok().and_then(|t| parse_xml(t).ok().and_then(|d| xml_error_code(&d))).unwrap_or_default();
    map_status(status, &code, what)
}

/// [`http::status_error`] plus S3's error codes.
fn map_status(status: u16, code: &str, what: &str) -> RemoteError {
    let detail = if code.is_empty() { what.to_string() } else { format!("{what} ({code})") };
    match (status, code) {
        (401 | 403 | 400, "SignatureDoesNotMatch" | "InvalidAccessKeyId" | "InvalidToken" | "ExpiredToken" | "AuthorizationHeaderMalformed") => {
            RemoteError::Unauthorized
        }
        (409, "ConditionalRequestConflict") => RemoteError::AlreadyExists(what.to_string()),
        (_, "InternalError" | "SlowDown" | "ServiceUnavailable" | "RequestTimeout") => RemoteError::Network(format!("{detail}: the server is busy")),
        (404 | 412, _) => http::status_error(status, what),
        (300..=399, _) => RemoteError::Protocol(format!("{detail}: HTTP {status}, redirected (check the endpoint and region)")),
        _ => match http::status_error(status, &detail) {
            RemoteError::Protocol(_) => RemoteError::Protocol(format!("{detail}: HTTP {status}")),
            e => e,
        },
    }
}

/// An `<Error>` document (also sent with status 200 by `CompleteMultipartUpload` /
/// `CopyObject`) as an error.
fn check_xml_error(doc: &roxmltree::Document<'_>, what: &str) -> Result<()> {
    if doc.root_element().tag_name().name() != "Error" {
        return Ok(());
    }
    let code = xml_error_code(doc).unwrap_or_default();
    let status = match code.as_str() {
        "AccessDenied" => 403,
        "NoSuchKey" | "NoSuchUpload" => 404,
        _ => 400,
    };
    Err(map_status(status, &code, what))
}

/// `<Error><Code>`, capped and reduced to safe characters.
fn xml_error_code(doc: &roxmltree::Document<'_>) -> Option<String> {
    let root = doc.root_element();
    if root.tag_name().name() != "Error" {
        return None;
    }
    let code = root.children().find(|n| n.tag_name().name() == "Code")?.text()?;
    Some(code.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').take(64).collect())
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 4_000_000, ..Default::default() })
        .map_err(|e| RemoteError::Protocol(format!("bad XML from the server: {e}")))
}

/// One listed object.
#[derive(Debug, Default)]
struct Listed {
    key: String,
    size: u64,
    modified: Option<i64>,
    etag: Option<String>,
}

#[derive(Debug, Default)]
struct ListPage {
    contents: Vec<Listed>,
    prefixes: Vec<String>,
    truncated: bool,
    next: Option<String>,
}

fn parse_list(bytes: &[u8]) -> Result<ListPage> {
    let text = std::str::from_utf8(bytes).map_err(|_| RemoteError::Protocol("the listing is not UTF-8".into()))?;
    let doc = parse_xml(text)?;
    check_xml_error(&doc, "listing")?;
    let root = doc.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err(RemoteError::Protocol("the listing is not a ListBucketResult".into()));
    }
    let mut page = ListPage::default();
    let text_of = |n: roxmltree::Node<'_, '_>, name: &str| n.children().find(|c| c.tag_name().name() == name).and_then(|c| c.text()).map(str::to_string);
    for n in root.children().filter(|n| n.is_element()) {
        if page.contents.len().saturating_add(page.prefixes.len()) >= MAX_LIST_ENTRIES {
            break;
        }
        match n.tag_name().name() {
            "Contents" => {
                let Some(key) = text_of(n, "Key") else { continue };
                let size = text_of(n, "Size").and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
                let modified = text_of(n, "LastModified").as_deref().and_then(parse_iso8601);
                let etag = text_of(n, "ETag").filter(|e| e.len() <= MAX_TOKEN_BYTES);
                page.contents.push(Listed { key, size, modified, etag });
            }
            "CommonPrefixes" => {
                if let Some(p) = text_of(n, "Prefix") {
                    page.prefixes.push(p);
                }
            }
            "IsTruncated" => page.truncated = n.text().is_some_and(|t| t.trim() == "true"),
            "NextContinuationToken" => page.next = n.text().map(str::to_string).filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN_BYTES),
            _ => {}
        }
    }
    Ok(page)
}

/// The first byte of `Content-Range: bytes a-b/len`.
fn content_range_start(v: &str) -> Option<u64> {
    v.trim().strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Why a streamed upload body stopped.
#[derive(Debug)]
enum StreamFail {
    Cancelled,
    Short(u64),
    Long,
    Source(std::io::Error),
}

/// Streams exactly `remaining` bytes of `inner`, reporting progress; never hands over the last
/// bytes when the source has more than announced (so the server never stores a wrong file).
struct Counting<'a, 'p> {
    inner: &'a mut dyn Read,
    remaining: u64,
    /// Bytes of the whole file done (for progress).
    done: u64,
    progress: &'p mut dyn FnMut(u64) -> bool,
    check_extra: bool,
    fail: Option<StreamFail>,
}

impl Counting<'_, '_> {
    fn error(&mut self, e: ureq::Error, size: u64, what: &str) -> RemoteError {
        match self.fail.take() {
            Some(StreamFail::Cancelled) => RemoteError::Cancelled,
            Some(StreamFail::Short(got)) => RemoteError::Protocol(format!("{what}: {got} bytes instead of the announced {size}")),
            Some(StreamFail::Long) => RemoteError::Protocol(format!("{what}: more than the announced {size} bytes")),
            Some(StreamFail::Source(e)) => RemoteError::Io(e),
            None => transport_error(e, what),
        }
    }

    fn stop(&mut self, why: StreamFail) -> std::io::Error {
        self.fail = Some(why);
        std::io::Error::other("upload stopped")
    }
}

impl Read for Counting<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = usize::try_from(self.remaining).unwrap_or(usize::MAX).min(buf.len());
        let Some(dst) = buf.get_mut(..want) else { return Ok(0) };
        let n = match self.inner.read(dst) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(e),
            Err(e) => return Err(self.stop(StreamFail::Source(e))),
        };
        if n == 0 {
            return Err(self.stop(StreamFail::Short(self.done)));
        }
        self.remaining = self.remaining.saturating_sub(n as u64);
        if self.remaining == 0 && self.check_extra {
            let mut probe = [0u8; 1];
            match self.inner.read(&mut probe) {
                Ok(0) => {}
                Ok(_) => return Err(self.stop(StreamFail::Long)),
                Err(e) => return Err(self.stop(StreamFail::Source(e))),
            }
        }
        self.done = self.done.saturating_add(n as u64);
        if !(self.progress)(self.done) {
            return Err(self.stop(StreamFail::Cancelled));
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------------------------
// Signature Version 4

/// What goes into one request's signature.
struct SignInput<'a> {
    method: &'a str,
    /// The encoded path, exactly as sent.
    path: &'a str,
    /// Decoded query pairs.
    query: &'a [(String, String)],
    /// Lower-case names; all of them are signed.
    headers: &'a [(String, String)],
    payload_hash: &'a str,
    /// `yyyymmddThhmmssZ`
    amz_date: &'a str,
    region: &'a str,
    access_key: &'a str,
    secret: &'a str,
}

impl SignInput<'_> {
    fn date(&self) -> &str {
        self.amz_date.get(..8).unwrap_or_default()
    }

    fn scope(&self) -> String {
        format!("{}/{}/s3/aws4_request", self.date(), self.region)
    }

    /// `(canonical request, signed headers)`.
    fn canonical_request(&self) -> (String, String) {
        let mut headers: Vec<(String, String)> =
            self.headers.iter().map(|(k, v)| (k.trim().to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
        headers.sort();
        let mut canon_headers = String::new();
        for (k, v) in &headers {
            let _ = writeln!(canon_headers, "{k}:{v}");
        }
        let signed = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let req = format!("{}\n{}\n{}\n{}\n{}\n{}", self.method, self.path, canonical_query(self.query), canon_headers, signed, self.payload_hash);
        (req, signed)
    }

    fn string_to_sign(&self, canonical_request: &str) -> String {
        format!("AWS4-HMAC-SHA256\n{}\n{}\n{}", self.amz_date, self.scope(), hex(&Sha256::digest(canonical_request.as_bytes())))
    }

    fn signature(&self) -> (String, String) {
        let (req, signed) = self.canonical_request();
        let sts = self.string_to_sign(&req);
        let k_date = hmac_sha256(format!("AWS4{}", self.secret).as_bytes(), self.date().as_bytes());
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        (hex(&hmac_sha256(&k_signing, sts.as_bytes())), signed)
    }

    fn authorization(&self) -> String {
        let (sig, signed) = self.signature();
        format!("AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={signed}, Signature={sig}", self.access_key, self.scope())
    }
}

/// Encoded `k=v` pairs, sorted (the canonical query string, also used on the wire).
fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query.iter().map(|(k, v)| (percent_encode(k, false), percent_encode(v, false))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// A key's path: each `/`-separated segment encoded.
fn encode_key(key: &str) -> String {
    percent_encode(key, true)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        for (d, s) in k.iter_mut().zip(Sha256::digest(key).iter()) {
            *d = *s;
        }
    } else {
        for (d, s) in k.iter_mut().zip(key.iter()) {
            *d = *s;
        }
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(data).finalize();
    Sha256::new().chain_update(&opad).chain_update(inner).finalize().into()
}

// ---------------------------------------------------------------------------------------------
// Dates (no chrono; parsing via `crate::http`)

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// Days since 1970-01-01 → (year, month, day) (proleptic Gregorian, Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe.saturating_add(era.saturating_mul(400)).saturating_add(i64::from(m <= 2));
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// `yyyymmddThhmmssZ` (the `x-amz-date` format).
fn amz_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let t = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", t / 3600, (t / 60) % 60, t % 60)
}

fn num<T: std::str::FromStr>(s: &str, range: std::ops::Range<usize>) -> Option<T> {
    let part = s.get(range)?;
    if !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// `2009-10-12T17:50:30.000Z` → seconds since the epoch.
fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.get(4..5)? != "-" || s.get(7..8)? != "-" || !matches!(s.get(10..11)?, "T" | "t") || s.get(13..14)? != ":" || s.get(16..17)? != ":" {
        return None;
    }
    http::unix_time(num(s, 0..4)?, num(s, 5..7)?, num(s, 8..10)?, num(s, 11..13)?, num(s, 14..16)?, num(s, 17..19)?)
}

#[cfg(test)]
mod tests;
