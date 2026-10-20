//! Remote locations (STORAGE-PLAN S2): projects and media on a server, without mounting anything.
//!
//! Every backend (WebDAV, S3-compatible, SFTP) implements [`RemoteStore`]. Paths are
//! [`RemotePath`]s: relative to the location's root and checked, so nothing a server or a
//! project file says can reach outside the location. Lock files ([`lock`]) are built on the
//! one primitive every backend can do atomically: [`RemoteStore::create_new`].

#[cfg(feature = "net")]
pub(crate) mod http;
pub mod lock;
pub mod memory;
mod path;
#[cfg(feature = "net")]
pub mod s3;
pub mod secrets;
#[cfg(feature = "sftp")]
pub mod sftp;
#[cfg(feature = "net")]
pub mod webdav;

pub use memory::MemoryStore;
pub use path::RemotePath;
#[cfg(feature = "net")]
pub use s3::S3;
#[cfg(feature = "sftp")]
pub use sftp::Sftp;
#[cfg(feature = "net")]
pub use webdav::WebDav;

use serde::{Deserialize, Serialize};

/// The largest single read (a project file, a manifest, a lock file, one read-ahead block).
/// Media is streamed in ranges of at most this many bytes.
pub const MAX_READ_BYTES: u64 = 256 << 20;

/// The most entries one listing returns (a hostile server can't make us allocate without bound).
pub const MAX_LIST_ENTRIES: usize = 200_000;

/// What can go wrong talking to a remote location.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("the server refused the sign-in (check the user name and password or key)")]
    Unauthorized,
    #[error("not allowed: {0}")]
    Forbidden(String),
    #[error("bad path: {0}")]
    BadPath(String),
    #[error("too large: {0}")]
    TooLarge(String),
    #[error("could not reach the server: {0}")]
    Network(String),
    #[error("the server sent something unexpected: {0}")]
    Protocol(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl RemoteError {
    /// Whether retrying (or resuming) may succeed: a dropped connection, a timeout.
    pub fn is_transient(&self) -> bool {
        matches!(self, RemoteError::Network(_))
    }
}

pub type Result<T> = std::result::Result<T, RemoteError>;

/// One file or folder on a remote location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: RemotePath,
    pub is_dir: bool,
    /// Bytes (0 for folders).
    pub size: u64,
    /// Last modification, seconds since the Unix epoch, when the server reports it.
    pub modified: Option<i64>,
    /// The server's version tag (WebDAV / S3 ETag), when it has one.
    pub etag: Option<String>,
}

impl Entry {
    /// The last path segment ("" for the root).
    pub fn name(&self) -> &str {
        self.path.name()
    }
}

/// Progress callback for long transfers: bytes done so far; returning false cancels.
pub type Progress<'a> = &'a mut dyn FnMut(u64) -> bool;

/// A remote location's storage. Every method is blocking and bounded (timeouts and size caps
/// are the backend's job); callers run them off the UI thread.
pub trait RemoteStore: Send + Sync {
    /// The folder's direct children (not recursive), at most [`MAX_LIST_ENTRIES`].
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>>;

    /// One file or folder.
    fn stat(&self, path: &RemotePath) -> Result<Entry>;

    /// Up to `len` bytes of the file from `offset` (fewer at its end; empty past it).
    /// `len` above [`MAX_READ_BYTES`] is refused.
    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// Writes (creates or replaces) the file with `size` bytes from `data`, creating missing
    /// parent folders. A failed write never leaves a truncated file under `path`: backends
    /// upload to a temporary name and rename, or use an all-or-nothing upload.
    fn write(&self, path: &RemotePath, data: &mut dyn std::io::Read, size: u64, progress: Progress) -> Result<()>;

    /// Writes the small file only if nothing exists at `path`, atomically (two machines racing:
    /// exactly one wins, the other gets [`RemoteError::AlreadyExists`]). Lock files use this.
    fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> Result<()>;

    /// Moves a file, replacing what is at `to`.
    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()>;

    /// Deletes a file (not a folder).
    fn delete(&self, path: &RemotePath) -> Result<()>;

    /// Creates the folder and its missing parents (no error if it exists).
    fn make_dir(&self, dir: &RemotePath) -> Result<()>;

    /// Reads a whole small file (at most `max_bytes`; larger fails with [`RemoteError::TooLarge`]).
    fn read_all(&self, path: &RemotePath, max_bytes: u64) -> Result<Vec<u8>> {
        let cap = max_bytes.min(MAX_READ_BYTES);
        let bytes = self.read_range(path, 0, cap.saturating_add(1).min(MAX_READ_BYTES))?;
        if bytes.len() as u64 > cap {
            return Err(RemoteError::TooLarge(format!("{path} is larger than {cap} bytes")));
        }
        Ok(bytes)
    }
}

/// Which protocol a location speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// Nextcloud, ownCloud, Synology, Apache / nginx WebDAV.
    WebDav,
    /// Any SSH server.
    Sftp,
    /// Amazon S3 and compatible servers (MinIO, Garage).
    S3,
}

/// A saved remote location (Settings ▸ Remote locations). Serialised into the app's settings,
/// so it never holds a secret: the password / key / secret key lives in the system keyring
/// under [`Location::secret_key`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Stable id (used in commands, `remote:` media links and the keyring entry).
    pub id: String,
    /// What the user called it.
    pub name: String,
    pub kind: Kind,
    /// `https://host/remote.php/dav/files/me/`, `sftp://host:22`, `https://s3.host` …
    pub url: String,
    /// User name / access key id ("" when none).
    #[serde(default)]
    pub user: String,
    /// Folder (or S3 bucket + prefix: `bucket/some/prefix`) everything lives under.
    #[serde(default)]
    pub root: String,
    /// S3 region ("" → `us-east-1`); ignored by the other kinds.
    #[serde(default)]
    pub region: String,
}

impl Location {
    /// The keyring entry name of this location's secret.
    pub fn secret_key(&self) -> String {
        format!("revolution-remote:{}", self.id)
    }
}
