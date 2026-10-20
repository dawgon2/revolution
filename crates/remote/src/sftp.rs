//! SFTP backend (STORAGE-PLAN S2 step 1d): any SSH server, through `russh` + `russh-sftp`
//! built on RustCrypto only (no `ring`, `aws-lc` or OpenSSL; pinned to russh 0.52, the last
//! release that doesn't need one of them).
//!
//! - Address: `Location::url` = `sftp://host[:port]` (or `ssh://`; IPv6 as `[::1]`),
//!   `Location::user`, `Location::root` = the folder everything lives under: relative to the
//!   login folder, or absolute when it starts with `/` (`..` is refused). An empty root with a
//!   path in the URL (`sftp://host/srv/films`) uses that path, absolute.
//! - Secret: a password, or an OpenSSH / PEM private key (text starting with `-----BEGIN`).
//!   Passphrase-protected keys are not supported (yet): they are refused with a clear message.
//! - Host keys: trust on first use, against a known-hosts file the caller names (the engine
//!   passes `<data dir>/remote/known_hosts`). The first connection records the server's SHA-256
//!   fingerprint; a different key later is refused ([`RemoteError::Forbidden`]). The file holds
//!   one `host:port SHA256:…` line per server (not OpenSSH's format).
//! - Each [`Sftp`] owns a current-thread tokio runtime and one connection (reconnected when it
//!   drops). Methods block; call them from a plain thread, never from inside an async runtime
//!   (that is refused with an error instead of panicking).

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};

use crate::{Entry, Location, MAX_LIST_ENTRIES, MAX_READ_BYTES, Progress, RemoteError, RemotePath, RemoteStore, Result};

/// Connecting, the handshake, signing in and starting SFTP.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// One operation (or one batch of a long transfer).
pub const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Bytes per SFTP read / write request (every server accepts 32 KiB).
const CHUNK: u32 = 32 << 10;
/// Requests in flight at once during a transfer (1 MiB).
const IN_FLIGHT: u64 = 32;
/// The largest known-hosts file read.
pub const MAX_KNOWN_HOSTS_BYTES: u64 = 1 << 20;
/// Symbolic links resolved per listing (the rest are listed as files).
const MAX_LINK_STATS: usize = 1000;
/// The longest server message kept in an error.
const MAX_SERVER_MESSAGE: usize = 200;

const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// Where to connect: parsed from a [`Location`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Host name or IP address (IPv6 without brackets), lowercase.
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The root folder as the server sees it: "" (the login folder), `rel/path`, `/` or `/abs/path`.
    pub base: String,
}

impl Target {
    /// Parses and checks a location's URL, user and root.
    pub fn from_location(loc: &Location) -> Result<Self> {
        let url = parse_url(&loc.url)?;
        let user = if loc.user.trim().is_empty() { url.user.unwrap_or_default() } else { loc.user.trim().to_string() };
        if user.is_empty() {
            return Err(RemoteError::BadPath("an SFTP location needs a user name".into()));
        }
        if user.len() > 255 || user.chars().any(char::is_control) {
            return Err(RemoteError::BadPath("the user name is too long or contains control characters".into()));
        }
        let root = if loc.root.trim().is_empty() && !url.path.trim_matches('/').is_empty() { format!("/{}", url.path) } else { loc.root.clone() };
        Ok(Target { host: url.host, port: url.port, user, base: base_path(&root)? })
    }

    /// The name of this server in the known-hosts file: `host:port`, `[v6]:port`.
    pub fn host_key_name(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }

    /// The server path of a location path.
    fn full(&self, p: &RemotePath) -> String {
        match (self.base.as_str(), p.is_root()) {
            ("", true) => ".".into(),
            ("", false) => p.as_str().to_string(),
            (b, true) => b.to_string(),
            ("/", false) => format!("/{}", p.as_str()),
            (b, false) => format!("{b}/{}", p.as_str()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedUrl {
    host: String,
    port: u16,
    user: Option<String>,
    /// Everything after the first `/` of the authority (no leading slash).
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl> {
    let bad = |why: &str| RemoteError::BadPath(format!("SFTP address: {why}"));
    if url.chars().any(char::is_control) {
        return Err(bad("it contains a control character"));
    }
    let url = url.trim();
    if url.len() > 2048 {
        return Err(bad("too long"));
    }
    let lower = url.to_ascii_lowercase();
    let rest = if lower.starts_with("sftp://") {
        url.get(7..)
    } else if lower.starts_with("ssh://") {
        url.get(6..)
    } else {
        None
    }
    .ok_or_else(|| bad("it starts with sftp:// (for example sftp://nas.local)"))?;
    if rest.chars().any(|c| c.is_control() || c == '?' || c == '#' || c == '\\' || c == ' ') {
        return Err(bad("it contains a space, control character, ?, # or backslash"));
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) if !u.is_empty() && !u.contains('@') && !u.contains(':') => (Some(u.to_string()), h),
        Some(_) => return Err(bad("the user name part is not valid (put the password in the password field)")),
        None => (None, authority),
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or_else(|| bad("an IPv6 address misses its ]"))?;
        if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
            return Err(bad("the IPv6 address is not valid"));
        }
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or_else(|| bad("unexpected text after the IPv6 address"))?),
        };
        (h.to_ascii_lowercase(), port)
    } else {
        let (h, p) = match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        };
        // `nas.local.` and `nas.local` are the same host (one known-hosts entry)
        let h = h.strip_suffix('.').unwrap_or(h);
        if h.is_empty() || h.len() > 253 || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_') {
            return Err(bad("the host name is missing or not valid"));
        }
        (h.to_ascii_lowercase(), p)
    };
    let port = match port {
        None => 22,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 && p.len() <= 5 => n,
            _ => return Err(bad("the port is not a number from 1 to 65535")),
        },
    };
    Ok(ParsedUrl { host, port, user, path: path.to_string() })
}

/// Checks a root folder: relative to the login folder, or absolute (leading `/`).
fn base_path(root: &str) -> Result<String> {
    let root = root.trim();
    let absolute = root.starts_with('/');
    let rel = RemotePath::parse(root).map_err(|e| RemoteError::BadPath(format!("the location's folder: {e}")))?;
    Ok(match (absolute, rel.is_root()) {
        (true, true) => "/".into(),
        (true, false) => format!("/{}", rel.as_str()),
        (false, _) => rel.as_str().to_string(),
    })
}

// ---------------------------------------------------------------------------------------------
// Known hosts

/// The fingerprints recorded for `name` (`host:port`). Lines for other servers that don't parse
/// are skipped; a damaged line for this server is an error (`Err(line number)`): it must never
/// turn into silent re-trust.
fn parse_known_hosts(text: &str, name: &str) -> std::result::Result<Vec<String>, usize> {
    let mut out = Vec::new();
    for (i, line) in text.lines().take(100_000).enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        if it.next() != Some(name) {
            continue;
        }
        match (it.next(), it.next()) {
            (Some(fp), None) if valid_fingerprint(fp) => {
                if out.len() < 64 {
                    out.push(fp.to_string());
                }
            }
            _ => return Err(i.saturating_add(1)),
        }
    }
    Ok(out)
}

fn valid_fingerprint(fp: &str) -> bool {
    fp.strip_prefix("SHA256:")
        .is_some_and(|b| !b.is_empty() && b.len() <= 64 && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
}

fn read_known_hosts(path: &Path) -> Result<String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_KNOWN_HOSTS_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_KNOWN_HOSTS_BYTES {
        return Err(RemoteError::TooLarge(format!("the known-hosts file {} is larger than {MAX_KNOWN_HOSTS_BYTES} bytes", path.display())));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn record_known_host(path: &Path, existing: &str, name: &str, fp: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    f.write_all(format!("{sep}{name} {fp}\n").as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Secrets

enum Auth {
    Password(String),
    Key(Arc<PrivateKey>),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Auth::Password(_) => "Password(<redacted>)",
            Auth::Key(_) => "PrivateKey(<redacted>)",
        })
    }
}

fn parse_secret(secret: &str) -> Result<Auth> {
    if !secret.trim_start().starts_with("-----BEGIN") {
        return Ok(Auth::Password(secret.to_string()));
    }
    match russh::keys::decode_secret_key(secret.trim(), None) {
        Ok(k) => Ok(Auth::Key(Arc::new(k))),
        Err(russh::keys::Error::KeyIsEncrypted) => Err(RemoteError::Forbidden(
            "the private key is protected by a passphrase, which isn't supported yet: use a key without one (ssh-keygen -p can remove it)".into(),
        )),
        // never echo the key or the parser's details about it
        Err(_) => Err(RemoteError::Forbidden("the private key could not be read (an OpenSSH or PEM private key is expected)".into())),
    }
}

// ---------------------------------------------------------------------------------------------
// Errors

fn clean_message(msg: &str) -> String {
    msg.chars().filter(|c| !c.is_control()).take(MAX_SERVER_MESSAGE).collect()
}

fn map_ssh(e: russh::Error) -> RemoteError {
    use russh::Error as E;
    match e {
        E::IO(io) => RemoteError::Network(io.to_string()),
        E::ConnectionTimeout | E::KeepaliveTimeout | E::InactivityTimeout | E::Disconnect | E::HUP | E::SendError | E::Elapsed(_) => {
            RemoteError::Network(e.to_string())
        }
        E::UnknownKey | E::WrongServerSig => RemoteError::Forbidden("the server's identity could not be verified".into()),
        E::NoAuthMethod | E::NotAuthenticated => RemoteError::Unauthorized,
        other => RemoteError::Protocol(clean_message(&other.to_string())),
    }
}

fn in_runtime_error() -> RemoteError {
    RemoteError::Protocol("the SFTP store was called from inside an async runtime (call it from a plain thread)".into())
}

fn status_code(e: &SftpError) -> Option<StatusCode> {
    match e {
        SftpError::Status(s) => Some(s.status_code),
        _ => None,
    }
}

fn map_sftp(e: SftpError, shown: &str) -> RemoteError {
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => RemoteError::NotFound(shown.to_string()),
            StatusCode::PermissionDenied => RemoteError::Forbidden(format!("{shown}: permission denied")),
            StatusCode::NoConnection | StatusCode::ConnectionLost => RemoteError::Network(format!("{}", s.status_code)),
            StatusCode::OpUnsupported => RemoteError::Forbidden(format!("{shown}: the server doesn't support this")),
            code => RemoteError::Protocol(format!("{shown}: {code}: {}", clean_message(&s.error_message))),
        },
        SftpError::Timeout => RemoteError::Network("the server stopped answering".into()),
        SftpError::IO(m) => RemoteError::Network(clean_message(&m)),
        SftpError::Limited(m) => RemoteError::TooLarge(clean_message(&m)),
        SftpError::UnexpectedPacket => RemoteError::Protocol(format!("{shown}: unexpected SFTP reply")),
        SftpError::UnexpectedBehavior(m) => {
            if ["closed", "dropped", "SendError", "RecvError"].iter().any(|k| m.contains(k)) {
                RemoteError::Network(clean_message(&m))
            } else {
                RemoteError::Protocol(clean_message(&m))
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Connection

struct Client {
    expected: Vec<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> std::result::Result<bool, Self::Error> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let ok = self.expected.is_empty() || self.expected.contains(&fp);
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(fp);
        Ok(ok)
    }
}

struct Conn {
    ssh: client::Handle<Client>,
    sftp: Arc<RawSftpSession>,
    posix_rename: bool,
}

/// Spawned SFTP requests: the ones not awaited yet are aborted when this is dropped (an early
/// error, a short read, a timeout).
struct Tasks<T>(VecDeque<(u64, tokio::task::JoinHandle<T>)>);

impl<T> Drop for Tasks<T> {
    fn drop(&mut self) {
        for (_, h) in &self.0 {
            h.abort();
        }
    }
}

/// What `rename_over` reports on failure: `keep_from` is set when the target is gone and the new
/// content only exists under `from`, so the caller must not delete it.
struct RenameFail {
    err: RemoteError,
    keep_from: bool,
}

impl From<RemoteError> for RenameFail {
    fn from(err: RemoteError) -> Self {
        RenameFail { err, keep_from: false }
    }
}

const RSA_WARNING: &str = "RSA keys work but ed25519 is safer: the RSA code FilmCraft uses has a known timing weakness (RUSTSEC-2023-0071). Create an ed25519 key (ssh-keygen -t ed25519), add it to the server and use it here.";

/// An SFTP remote location. See the module docs.
pub struct Sftp {
    conn: Mutex<Option<Conn>>,
    target: Target,
    auth: Auth,
    known_hosts: PathBuf,
    key_warning: Option<String>,
    /// Test switches (see [`Sftp::test_disable_posix_rename`], [`Sftp::test_fail_replace`]).
    no_posix_rename: AtomicBool,
    fail_replace: AtomicU8,
    /// `None` only while dropping.
    rt: Option<tokio::runtime::Runtime>,
}

impl fmt::Debug for Sftp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sftp").field("target", &self.target).field("auth", &self.auth).field("known_hosts", &self.known_hosts).finish_non_exhaustive()
    }
}

impl Drop for Sftp {
    fn drop(&mut self) {
        let conn = self.conn.get_mut().unwrap_or_else(PoisonError::into_inner).take();
        self.discard(conn);
        if let Some(rt) = self.rt.take() {
            // dropping a runtime inside another one's context panics
            if tokio::runtime::Handle::try_current().is_ok() {
                rt.shutdown_background();
            } else {
                drop(rt);
            }
        }
    }
}

impl Sftp {
    /// Connects and signs in (so a wrong address, password or a changed server key shows up
    /// right away). `secret` is the password or private key; `known_hosts` the file the
    /// servers' fingerprints are kept in (created on first use). Must be called from a plain
    /// thread, not from inside an async runtime (that is refused with an error).
    pub fn new(location: &Location, secret: &str, known_hosts: &Path) -> Result<Self> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(in_runtime_error());
        }
        let target = Target::from_location(location)?;
        let auth = parse_secret(secret)?;
        let key_warning = match &auth {
            Auth::Key(k) if k.algorithm().is_rsa() => Some(RSA_WARNING.to_string()),
            _ => None,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()?;
        let me = Sftp {
            conn: Mutex::new(None),
            target,
            auth,
            known_hosts: known_hosts.to_path_buf(),
            key_warning,
            no_posix_rename: AtomicBool::new(false),
            fail_replace: AtomicU8::new(0),
            rt: Some(rt),
        };
        let conn = me.connect()?;
        *me.lock() = Some(conn);
        Ok(me)
    }

    /// Where this store connects.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// A hint for the user about their sign-in (an RSA private key works, but ed25519 is safer).
    pub fn key_warning(&self) -> Option<&str> {
        self.key_warning.as_deref()
    }

    /// Tests: act as if the server lacked `posix-rename@openssh.com`.
    #[doc(hidden)]
    pub fn test_disable_posix_rename(&self, on: bool) {
        self.no_posix_rename.store(on, Ordering::Relaxed);
    }

    /// Tests, without posix-rename: 1 = the replacing rename fails (after the old file was moved
    /// aside), 2 = putting the old file back fails too.
    #[doc(hidden)]
    pub fn test_fail_replace(&self, level: u8) {
        self.fail_replace.store(level, Ordering::Relaxed);
    }

    fn posix_rename(&self, c: &Conn) -> bool {
        c.posix_rename && !self.no_posix_rename.load(Ordering::Relaxed)
    }

    fn lock(&self) -> MutexGuard<'_, Option<Conn>> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn rt(&self) -> Result<&tokio::runtime::Runtime> {
        self.rt.as_ref().ok_or_else(|| RemoteError::Network("the SFTP store is closing".into()))
    }

    /// Drops a connection inside our runtime (russh spawns a task when a channel closes).
    fn discard(&self, conn: Option<Conn>) {
        if conn.is_none() {
            return;
        }
        match &self.rt {
            Some(rt) => {
                let _guard = rt.enter();
                drop(conn);
            }
            None => drop(conn),
        }
    }

    /// Runs `fut` on the runtime with a time limit.
    fn block<T>(&self, limit: Duration, fut: impl Future<Output = Result<T>>) -> Result<T> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(in_runtime_error());
        }
        self.rt()?.block_on(async { tokio::time::timeout(limit, fut).await.unwrap_or_else(|_| Err(RemoteError::Network("timed out".into()))) })
    }

    fn connect(&self) -> Result<Conn> {
        let name = self.target.host_key_name();
        let known_text = read_known_hosts(&self.known_hosts)?;
        let expected = parse_known_hosts(&known_text, &name).map_err(|line| {
            RemoteError::Forbidden(format!(
                "the known-hosts entry for {name} is damaged (line {line} of {}): fix or remove that line, then connect again",
                self.known_hosts.display()
            ))
        })?;
        let seen = Arc::new(Mutex::new(None));
        let handler = Client { expected: expected.clone(), seen: seen.clone() };
        let t = &self.target;
        let auth = &self.auth;
        let result = self.block(CONNECT_TIMEOUT, async move {
            let config = Arc::new(client::Config { inactivity_timeout: None, ..Default::default() });
            let mut ssh = client::connect(config, (t.host.as_str(), t.port), handler).await.map_err(map_ssh)?;
            let res = match auth {
                Auth::Password(p) => ssh.authenticate_password(t.user.clone(), p.clone()).await,
                Auth::Key(k) => {
                    let hash = if k.algorithm().is_rsa() { ssh.best_supported_rsa_hash().await.ok().flatten().unwrap_or(Some(HashAlg::Sha256)) } else { None };
                    ssh.authenticate_publickey(t.user.clone(), PrivateKeyWithHashAlg::new(k.clone(), hash)).await
                }
            }
            .map_err(map_ssh)?;
            if !res.success() {
                return Err(RemoteError::Unauthorized);
            }
            let ch = ssh.channel_open_session().await.map_err(map_ssh)?;
            ch.request_subsystem(true, "sftp").await.map_err(map_ssh)?;
            let raw = RawSftpSession::new(ch.into_stream());
            raw.set_timeout(OP_TIMEOUT.as_secs());
            let version = raw.init().await.map_err(|e| map_sftp(e, "starting SFTP"))?;
            let posix_rename = version.extensions.get(POSIX_RENAME).is_some_and(|v| v == "1");
            Ok(Conn { ssh, sftp: Arc::new(raw), posix_rename })
        });
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(fp) = seen {
            if !expected.is_empty() && !expected.contains(&fp) {
                self.discard(result.ok());
                return Err(RemoteError::Forbidden(format!(
                    "the server's identity changed: {name} now presents key {fp}, not the one recorded in {}. This can mean someone is intercepting the connection; if the server was reinstalled, remove its line from that file",
                    self.known_hosts.display()
                )));
            }
            // Trust on first use, only once the handshake finished: signed in, or the server
            // answered the sign-in (which it can only do after a verified key exchange).
            if expected.is_empty()
                && matches!(result, Ok(_) | Err(RemoteError::Unauthorized))
                && let Err(e) = record_known_host(&self.known_hosts, &known_text, &name, &fp)
            {
                self.discard(result.ok());
                return Err(e);
            }
        }
        result
    }

    /// Runs `f` with a live connection (connecting again if it dropped). A network error drops
    /// the connection so the next call reconnects.
    fn with_conn<T>(&self, f: impl FnOnce(&Conn) -> Result<T>) -> Result<T> {
        let mut guard = self.lock();
        if guard.as_ref().is_none_or(|c| c.ssh.is_closed()) {
            self.discard(guard.take());
            *guard = Some(self.connect()?);
        }
        let Some(conn) = guard.as_ref() else {
            return Err(RemoteError::Network("not connected".into()));
        };
        let r = f(conn);
        if matches!(r, Err(RemoteError::Network(_))) {
            self.discard(guard.take());
        }
        r
    }

    fn entry(path: &RemotePath, attrs: &FileAttributes) -> Entry {
        let is_dir = attrs.is_dir();
        Entry { path: path.clone(), is_dir, size: if is_dir { 0 } else { attrs.size.unwrap_or(0) }, modified: attrs.mtime.map(i64::from), etag: None }
    }

    /// Streams `data` into the open handle `h`; returns the bytes written.
    fn upload(&self, s: &Arc<RawSftpSession>, h: &str, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        let mut total: u64 = 0;
        loop {
            let mut batch: Vec<(u64, Vec<u8>)> = Vec::new();
            let mut eof = false;
            while (batch.len() as u64) < IN_FLIGHT {
                let mut buf = vec![0u8; CHUNK as usize];
                let mut filled = 0usize;
                while filled < buf.len() {
                    let Some(rest) = buf.get_mut(filled..) else { break };
                    match data.read(rest) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => filled = filled.saturating_add(n),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                buf.truncate(filled);
                if !buf.is_empty() {
                    let offset = total;
                    total = total.saturating_add(buf.len() as u64);
                    if total > size {
                        return Err(RemoteError::Protocol(format!("more than the announced {size} bytes")));
                    }
                    batch.push((offset, buf));
                }
                if eof {
                    break;
                }
            }
            if !batch.is_empty() {
                self.block(OP_TIMEOUT, async {
                    let mut tasks = Tasks(
                        batch
                            .into_iter()
                            .map(|(offset, buf)| {
                                let (s, h) = (s.clone(), h.to_string());
                                (0, tokio::spawn(async move { s.write(h, offset, buf).await }))
                            })
                            .collect(),
                    );
                    while let Some((_, jh)) = tasks.0.pop_front() {
                        jh.await.map_err(|_| RemoteError::Protocol("an upload task failed".into()))?.map_err(|e| map_sftp(e, "upload"))?;
                    }
                    Ok(())
                })?;
                if !progress(total) {
                    return Err(RemoteError::Cancelled);
                }
            }
            if eof {
                break;
            }
        }
        if total != size {
            return Err(RemoteError::Protocol(format!("{total} bytes instead of the announced {size}")));
        }
        Ok(())
    }

    fn random_suffix() -> String {
        use std::hash::{BuildHasher, Hasher};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u32(std::process::id());
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default());
        format!("{:016x}", h.finish())
    }
}

// --- async helpers (all take the raw session) ---------------------------------------------------

async fn stat_full(s: &RawSftpSession, full: &str) -> std::result::Result<FileAttributes, SftpError> {
    s.stat(full).await.map(|a| a.attrs)
}

/// `mkdir` one folder, fine if it is already a folder.
async fn mkdir_one(s: &RawSftpSession, full: &str, shown: &str) -> Result<()> {
    match stat_full(s, full).await {
        Ok(a) if a.is_dir() => return Ok(()),
        Ok(_) => return Err(RemoteError::AlreadyExists(format!("{shown} is a file"))),
        Err(e) if status_code(&e) == Some(StatusCode::NoSuchFile) => {}
        Err(e) => return Err(map_sftp(e, shown)),
    }
    if let Err(e) = s.mkdir(full, FileAttributes::default()).await {
        // someone else made it in between?
        return match stat_full(s, full).await {
            Ok(a) if a.is_dir() => Ok(()),
            _ => Err(map_sftp(e, shown)),
        };
    }
    Ok(())
}

/// Creates the root folder (if missing) and `dir` with its parents.
async fn mkdirs(s: &RawSftpSession, t: &Target, dir: &RemotePath) -> Result<()> {
    if !t.base.is_empty() && t.base != "/" {
        let absolute = t.base.starts_with('/');
        let mut cur = String::new();
        for seg in t.base.split('/').filter(|s| !s.is_empty()) {
            cur = if cur.is_empty() && !absolute { seg.to_string() } else { format!("{cur}/{seg}") };
            mkdir_one(s, &cur, "the location's folder").await?;
        }
    }
    let mut cur = RemotePath::root();
    for seg in dir.segments() {
        cur = cur.join(seg)?;
        mkdir_one(s, &t.full(&cur), &cur.to_string()).await?;
    }
    Ok(())
}

async fn mkparents(s: &RawSftpSession, t: &Target, path: &RemotePath) -> Result<()> {
    match path.parent() {
        Some(parent) => mkdirs(s, t, &parent).await,
        None => Ok(()),
    }
}

fn ssh_string(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let len = u32::try_from(s.len()).map_err(|_| RemoteError::BadPath("path too long".into()))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// A sibling name for the old file while it is being replaced: `dir/.<name>.old-<random>`.
fn backup_name(to: &str) -> String {
    let (dir, name) = match to.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, to),
    };
    let suffix = Sftp::random_suffix();
    let file = if name.len() > 200 { format!(".old-{suffix}") } else { format!(".{name}.old-{suffix}") };
    match dir {
        Some(d) => format!("{d}/{file}"),
        None => file,
    }
}

fn simulated_failure() -> SftpError {
    SftpError::Status(russh_sftp::protocol::Status { id: 0, status_code: StatusCode::Failure, error_message: "simulated".into(), language_tag: String::new() })
}

/// Renames a file over whatever is at `to`. With `posix-rename@openssh.com` that is one atomic
/// step. Without it (plain SFTP rename never replaces), the old file is first moved aside to a
/// backup name, then `from` → `to`; on failure the backup is put back, and it is deleted only
/// after success. If even putting it back fails, nothing is deleted (`keep_from`) and the error
/// names both files. `fail` is the test switch of [`Sftp::test_fail_replace`].
async fn rename_over(s: &RawSftpSession, posix: bool, fail: u8, from: &str, to: &str, shown: &str) -> std::result::Result<(), RenameFail> {
    if posix {
        let mut data = Vec::new();
        ssh_string(&mut data, from)?;
        ssh_string(&mut data, to)?;
        return match s.extended(POSIX_RENAME, data).await {
            Ok(Packet::Status(st)) if st.status_code == StatusCode::Ok => Ok(()),
            Ok(Packet::Status(st)) => Err(map_sftp(SftpError::Status(st), shown).into()),
            Ok(_) => Err(RemoteError::Protocol(format!("{shown}: unexpected reply to rename")).into()),
            Err(e) => Err(map_sftp(e, shown).into()),
        };
    }
    match s.rename(from, to).await {
        Ok(_) => Ok(()),
        Err(e) if status_code(&e) == Some(StatusCode::Failure) && stat_full(s, to).await.is_ok() => {
            let backup = backup_name(to);
            s.rename(to, backup.as_str()).await.map_err(|e| map_sftp(e, shown))?;
            let replaced = if fail >= 1 { Err(simulated_failure()) } else { s.rename(from, to).await.map(|_| ()) };
            match replaced {
                Ok(()) => {
                    let _ = s.remove(backup.as_str()).await;
                    Ok(())
                }
                Err(e) => {
                    let restored = if fail >= 2 { Err(simulated_failure()) } else { s.rename(backup.as_str(), to).await.map(|_| ()) };
                    match restored {
                        Ok(()) => Err(map_sftp(e, shown).into()),
                        Err(_) => Err(RenameFail {
                            err: RemoteError::Protocol(format!(
                                "{shown} could not be replaced and the previous version could not be put back: it is kept on the server as {backup}, the new one as {from}"
                            )),
                            keep_from: true,
                        }),
                    }
                }
            }
        }
        Err(e) => Err(map_sftp(e, shown).into()),
    }
}

async fn close_quiet(s: &RawSftpSession, h: &str) {
    let _ = s.close(h).await;
}

impl RemoteStore for Sftp {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let t = &self.target;
        let shown = dir.to_string();
        self.with_conn(|c| {
            let s = &*c.sftp;
            // every request gets its own time limit: a big folder may take many of them
            let h = self.block(OP_TIMEOUT, async { s.opendir(t.full(dir)).await.map(|h| h.handle).map_err(|e| map_sftp(e, &shown)) })?;
            let mut out: Vec<Entry> = Vec::new();
            let mut links: Vec<usize> = Vec::new();
            let mut seen: usize = 0;
            let read = (|| -> Result<()> {
                for _ in 0..MAX_LIST_ENTRIES {
                    let batch = self.block(OP_TIMEOUT, async {
                        match s.readdir(h.as_str()).await {
                            Ok(name) => Ok(Some(name.files)),
                            Err(e) if status_code(&e) == Some(StatusCode::Eof) => Ok(None),
                            Err(e) => Err(map_sftp(e, &shown)),
                        }
                    })?;
                    let Some(files) = batch else { return Ok(()) };
                    for f in files {
                        seen = seen.saturating_add(1);
                        if out.len() >= MAX_LIST_ENTRIES || seen > MAX_LIST_ENTRIES.saturating_mul(2) {
                            return Ok(());
                        }
                        if f.filename == "." || f.filename == ".." {
                            continue;
                        }
                        let Ok(path) = dir.join(&f.filename) else { continue };
                        if f.attrs.is_symlink() && links.len() < MAX_LINK_STATS {
                            links.push(out.len());
                        }
                        out.push(Self::entry(&path, &f.attrs));
                    }
                }
                Ok(())
            })();
            let _ = self.block(OP_TIMEOUT, async {
                close_quiet(s, &h).await;
                Ok(())
            });
            read?;
            for i in links {
                let Some(e) = out.get_mut(i) else { continue };
                let full = t.full(&e.path);
                match self.block(OP_TIMEOUT, async { Ok(stat_full(s, &full).await.ok()) }) {
                    Ok(Some(a)) => *e = Self::entry(&e.path, &a),
                    Ok(None) => {}
                    Err(err) => return Err(err),
                }
            }
            Ok(out)
        })
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry> {
        let t = &self.target;
        self.with_conn(|c| {
            self.block(OP_TIMEOUT, async { stat_full(&c.sftp, &t.full(path)).await.map(|a| Self::entry(path, &a)).map_err(|e| map_sftp(e, &path.to_string())) })
        })
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(RemoteError::TooLarge(format!("a read of {len} bytes")));
        }
        let t = &self.target;
        let shown = path.to_string();
        self.with_conn(|c| {
            let s = &c.sftp;
            let h = self.block(OP_TIMEOUT, async {
                s.open(t.full(path), OpenFlags::READ, FileAttributes::default()).await.map(|h| h.handle).map_err(|e| map_sftp(e, &shown))
            })?;
            let mut out: Vec<u8> = Vec::with_capacity(usize::try_from(len.min(4 << 20)).unwrap_or(0));
            let result = (|| -> Result<()> {
                let (mut cur, mut remaining) = (offset, len);
                while remaining > 0 {
                    let (got, done) = self.block(OP_TIMEOUT, async {
                        let mut tasks = Tasks(VecDeque::new());
                        let mut planned: u64 = 0;
                        while (tasks.0.len() as u64) < IN_FLIGHT && planned < remaining {
                            // SFTP offsets are signed on most servers: past that is past every file's end
                            let Some(off) = cur.checked_add(planned).filter(|o| i64::try_from(*o).is_ok()) else { break };
                            let want = remaining.saturating_sub(planned).min(u64::from(CHUNK));
                            let want32 = u32::try_from(want).unwrap_or(CHUNK);
                            let (s2, h2) = (s.clone(), h.clone());
                            tasks.0.push_back((want, tokio::spawn(async move { s2.read(h2, off, want32).await })));
                            planned = planned.saturating_add(want);
                        }
                        let mut got: u64 = 0;
                        let mut done = tasks.0.is_empty();
                        // an early return or break drops `tasks`, aborting the rest
                        while let Some((want, jh)) = tasks.0.pop_front() {
                            match jh.await.map_err(|_| RemoteError::Protocol("a download task failed".into()))? {
                                Ok(data) => {
                                    let n = usize::try_from(want).unwrap_or(0).min(data.data.len());
                                    out.extend_from_slice(data.data.get(..n).unwrap_or_default());
                                    got = got.saturating_add(n as u64);
                                    if (n as u64) < want {
                                        // short read: carry on from here (or stop at the end)
                                        done = n == 0;
                                        break;
                                    }
                                }
                                Err(e) if status_code(&e) == Some(StatusCode::Eof) => {
                                    done = true;
                                    break;
                                }
                                Err(e) => return Err(map_sftp(e, &shown)),
                            }
                        }
                        Ok((got, done))
                    })?;
                    remaining = remaining.saturating_sub(got);
                    match cur.checked_add(got) {
                        Some(n) => cur = n,
                        None => break,
                    }
                    if done {
                        break;
                    }
                }
                Ok(())
            })();
            let result = match result {
                // reading a folder: the server opens it but can't read it
                Err(RemoteError::Protocol(m)) => {
                    let is_dir = self.block(OP_TIMEOUT, async { Ok(stat_full(s, &t.full(path)).await.is_ok_and(|a| a.is_dir())) }).unwrap_or(false);
                    if is_dir { Err(RemoteError::NotFound(format!("{shown} is a folder"))) } else { Err(RemoteError::Protocol(m)) }
                }
                r => r,
            };
            let _ = self.block(OP_TIMEOUT, async {
                close_quiet(s, &h).await;
                Ok(())
            });
            result.map(|()| out)
        })
    }

    fn write(&self, path: &RemotePath, data: &mut dyn Read, size: u64, progress: Progress) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let parent = path.parent().unwrap_or_default();
        let suffix = Self::random_suffix();
        let temp = parent.join(&format!(".{}.part-{suffix}", path.name())).or_else(|_| parent.join(&format!(".part-{suffix}")))?;
        let t = &self.target;
        let (target_full, temp_full) = (t.full(path), t.full(&temp));
        let shown = path.to_string();
        self.with_conn(|c| {
            let s = &c.sftp;
            let h = self.block(OP_TIMEOUT, async {
                match stat_full(s, &target_full).await {
                    Ok(a) if a.is_dir() => return Err(RemoteError::AlreadyExists(format!("{shown} is a folder"))),
                    Ok(_) => {}
                    Err(e) if status_code(&e) == Some(StatusCode::NoSuchFile) => {}
                    Err(e) => return Err(map_sftp(e, &shown)),
                }
                mkparents(s, t, path).await?;
                let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::EXCLUDE;
                s.open(temp_full.as_str(), flags, FileAttributes::default()).await.map(|h| h.handle).map_err(|e| map_sftp(e, &shown))
            })?;
            let mut result = self.upload(s, &h, data, size, progress);
            let closed = self.block(OP_TIMEOUT, async { s.close(h.as_str()).await.map(|_| ()).map_err(|e| map_sftp(e, &shown)) });
            if result.is_ok() {
                result = closed;
            }
            let mut keep_temp = false;
            if result.is_ok() {
                let (posix, fail) = (self.posix_rename(c), self.fail_replace.load(Ordering::Relaxed));
                result = match self.block(OP_TIMEOUT, async { Ok(rename_over(s, posix, fail, &temp_full, &target_full, &shown).await) }) {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(f)) => {
                        keep_temp = f.keep_from;
                        Err(f.err)
                    }
                    // timed out mid-replace: the target may be gone, so the upload must stay
                    Err(e) => {
                        keep_temp = !posix;
                        Err(e)
                    }
                };
            }
            if result.is_err() && !keep_temp {
                // best effort: never leave the partial upload behind (but never delete the only
                // copy of the new content when the target is gone)
                let _ = self.block(OP_TIMEOUT, async {
                    let _ = s.remove(temp_full.as_str()).await;
                    Ok(())
                });
            }
            result
        })
    }

    /// `open(CREATE | EXCLUDE)` (O_EXCL on the server, so exactly one racer wins), then the
    /// bytes are written in place: not through a temporary file, since a rename can't refuse to
    /// replace. A connection dropped mid-write can leave a partial file; for lock files that
    /// reads as "held by unknown" (`lock::read` treats a garbled lock as held), never as free.
    fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let t = &self.target;
        let full = t.full(path);
        let shown = path.to_string();
        self.with_conn(|c| {
            let s = &c.sftp;
            let h = self.block(OP_TIMEOUT, async {
                mkparents(s, t, path).await?;
                let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE;
                match s.open(full.as_str(), flags, FileAttributes::default()).await {
                    Ok(h) => Ok(h.handle),
                    // O_EXCL failing reports a generic failure: tell "exists" from the rest
                    Err(e) if !matches!(status_code(&e), Some(StatusCode::NoSuchFile | StatusCode::PermissionDenied) | None) => {
                        if stat_full(s, &full).await.is_ok() { Err(RemoteError::AlreadyExists(shown.clone())) } else { Err(map_sftp(e, &shown)) }
                    }
                    Err(e) => Err(map_sftp(e, &shown)),
                }
            })?;
            let mut result = self.upload(s, &h, &mut &bytes[..], bytes.len() as u64, &mut |_| true);
            let closed = self.block(OP_TIMEOUT, async { s.close(h.as_str()).await.map(|_| ()).map_err(|e| map_sftp(e, &shown)) });
            if result.is_ok() {
                result = closed;
            }
            if result.is_err() {
                let _ = self.block(OP_TIMEOUT, async {
                    let _ = s.remove(full.as_str()).await;
                    Ok(())
                });
            }
            result
        })
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        if from.is_root() || to.is_root() {
            return Err(RemoteError::Forbidden("can't move the root".into()));
        }
        let t = &self.target;
        let (from_full, to_full) = (t.full(from), t.full(to));
        self.with_conn(|c| {
            let s = &*c.sftp;
            let (posix, fail) = (self.posix_rename(c), self.fail_replace.load(Ordering::Relaxed));
            self.block(OP_TIMEOUT, async {
                match stat_full(s, &from_full).await {
                    Ok(a) if a.is_dir() => return Err(RemoteError::Forbidden(format!("{from} is a folder"))),
                    Ok(_) => {}
                    Err(e) => return Err(map_sftp(e, &from.to_string())),
                }
                if stat_full(s, &to_full).await.is_ok_and(|a| a.is_dir()) {
                    return Err(RemoteError::AlreadyExists(format!("{to} is a folder")));
                }
                mkparents(s, t, to).await?;
                rename_over(s, posix, fail, &from_full, &to_full, &to.to_string()).await.map_err(|f| f.err)
            })
        })
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::Forbidden("the root is a folder".into()));
        }
        let full = self.target.full(path);
        let shown = path.to_string();
        self.with_conn(|c| {
            let s = &*c.sftp;
            self.block(OP_TIMEOUT, async {
                match s.lstat(full.as_str()).await {
                    Ok(a) if a.attrs.is_dir() => return Err(RemoteError::Forbidden(format!("{shown} is a folder"))),
                    Ok(_) => {}
                    Err(e) => return Err(map_sftp(e, &shown)),
                }
                s.remove(full.as_str()).await.map(|_| ()).map_err(|e| map_sftp(e, &shown))
            })
        })
    }

    fn make_dir(&self, dir: &RemotePath) -> Result<()> {
        let t = &self.target;
        self.with_conn(|c| self.block(OP_TIMEOUT, mkdirs(&c.sftp, t, dir)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(url: &str, user: &str, root: &str) -> Location {
        Location { id: "t".into(), name: "t".into(), kind: crate::Kind::Sftp, url: url.into(), user: user.into(), root: root.into(), region: String::new() }
    }

    #[test]
    fn parses_addresses() {
        let t = Target::from_location(&loc("sftp://NAS.local", "ana", "films")).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.user.as_str(), t.base.as_str()), ("nas.local", 22, "ana", "films"));
        assert_eq!(t.host_key_name(), "nas.local:22");
        let t = Target::from_location(&loc("ssh://bob@10.0.0.2:2222/", "", "/srv/media/")).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.user.as_str(), t.base.as_str()), ("10.0.0.2", 2222, "bob", "/srv/media"));
        let t = Target::from_location(&loc("sftp://[::1]:2200/srv/x", "a", "")).unwrap();
        assert_eq!((t.host.as_str(), t.base.as_str()), ("::1", "/srv/x"));
        assert_eq!(t.host_key_name(), "[::1]:2200");
        let t = Target::from_location(&loc("sftp://h", "a", "/")).unwrap();
        assert_eq!(t.full(&RemotePath::parse("a/b").unwrap()), "/a/b");
        assert_eq!(t.full(&RemotePath::root()), "/");
        let t = Target::from_location(&loc("sftp://h", "a", "")).unwrap();
        assert_eq!(t.full(&RemotePath::root()), ".");
        assert_eq!(t.full(&RemotePath::parse("x").unwrap()), "x");
        let t = Target::from_location(&loc("sftp://h", "a", "/srv")).unwrap();
        assert_eq!(t.full(&RemotePath::parse("x/y").unwrap()), "/srv/x/y");
    }

    #[test]
    fn refuses_hostile_addresses() {
        for bad in [
            "",
            "http://h",
            "sftp://",
            "sftp://:22",
            "sftp://h:0",
            "sftp://h:65536",
            "sftp://h:-1",
            "sftp://h:22:33",
            "sftp://h:00000022",
            "sftp://[::1",
            "sftp://[zz]",
            "sftp://[::1]x",
            "sftp://a b",
            "sftp://h/x?y",
            "sftp://h\n",
            "sftp://u:pw@h",
            "sftp://@h",
            "sftp://a@b@h",
            "sftp://h%2f",
            "sftp://h\\x",
        ] {
            assert!(Target::from_location(&loc(bad, "a", "")).is_err(), "{bad:?} accepted");
        }
        assert!(Target::from_location(&loc(&format!("sftp://{}", "a".repeat(3000)), "a", "")).is_err());
        assert!(Target::from_location(&loc("sftp://h", "", "")).is_err(), "no user");
        assert!(Target::from_location(&loc("sftp://h", "a\nb", "")).is_err());
        for bad_root in ["../etc", "/srv/../etc", "a//b", "a/./b", "a\\b", "/x/\u{0}"] {
            assert!(Target::from_location(&loc("sftp://h", "a", bad_root)).is_err(), "{bad_root:?} accepted");
        }
        assert!(Target::from_location(&loc("sftp://h/../etc", "a", "")).is_err());
    }

    #[test]
    fn parses_known_hosts_defensively() {
        let fp = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
        // other servers' broken lines are skipped
        let text = format!(
            "# comment\n\nh:22 {fp}\nother:22 {fp} extra\nother:22 MD5:aa\nh:2222 SHA256:bad!chars\n\u{0}\u{ffff}garbage\n   h:22    SHA256:second  \n"
        );
        assert_eq!(parse_known_hosts(&text, "h:22"), Ok(vec![fp.to_string(), "SHA256:second".to_string()]));
        assert_eq!(parse_known_hosts("", "h:22"), Ok(vec![]));
        assert_eq!(parse_known_hosts(&"x".repeat(10_000), "h:22"), Ok(vec![]));
        assert!(!valid_fingerprint(&format!("SHA256:{}", "a".repeat(65))));
        // a damaged line for this server is an error with its line number, never re-trust
        for bad in [format!("h:22 {fp} extra"), "h:22 MD5:aa".into(), "h:22 SHA256:".into(), "h:22 SHA256:bad!chars".into(), "h:22".into()] {
            assert_eq!(parse_known_hosts(&format!("# c\n{bad}\nh:22 {fp}\n"), "h:22"), Err(2), "{bad:?}");
        }
        // too many lines for one host are capped
        let many = format!("h:22 {fp}\n").repeat(1000);
        assert_eq!(parse_known_hosts(&many, "h:22").map(|v| v.len()), Ok(64));
        // a trailing dot names the same host
        let t = Target::from_location(&loc("sftp://NAS.local.:22", "a", "")).unwrap();
        assert_eq!(t.host_key_name(), "nas.local:22");
        assert!(Target::from_location(&loc("sftp://.", "a", "")).is_err());
    }

    #[test]
    fn known_hosts_file_round_trip_and_size_cap() {
        let dir = std::env::temp_dir().join(format!("fc-kh-{}-{}", std::process::id(), Sftp::random_suffix()));
        let path = dir.join("remote/known_hosts");
        assert_eq!(read_known_hosts(&path).unwrap(), "");
        record_known_host(&path, "", "h:22", "SHA256:abc").unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"x:1 SHA256:no-newline").unwrap();
        let text = read_known_hosts(&path).unwrap();
        record_known_host(&path, &text, "g:22", "SHA256:def").unwrap();
        let text = read_known_hosts(&path).unwrap();
        assert_eq!(parse_known_hosts(&text, "h:22"), Ok(vec!["SHA256:abc".to_string()]));
        assert_eq!(parse_known_hosts(&text, "g:22"), Ok(vec!["SHA256:def".to_string()]));
        std::fs::write(&path, vec![b'a'; (MAX_KNOWN_HOSTS_BYTES + 1) as usize]).unwrap();
        assert!(matches!(read_known_hosts(&path), Err(RemoteError::TooLarge(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_never_show() {
        let a = parse_secret("hunter2").unwrap();
        assert!(!format!("{a:?}").contains("hunter2"));
        let e = parse_secret("-----BEGIN OPENSSH PRIVATE KEY-----\nc2VjcmV0LWtleS1ieXRlcw==\n-----END OPENSSH PRIVATE KEY-----\n").unwrap_err();
        assert!(!e.to_string().contains("c2VjcmV0"));
        assert!(matches!(e, RemoteError::Forbidden(_)));
        let e = parse_secret("-----BEGIN").unwrap_err();
        assert!(matches!(e, RemoteError::Forbidden(_)));
    }

    #[test]
    fn errors_are_classified() {
        let st = |code| {
            SftpError::Status(russh_sftp::protocol::Status { id: 1, status_code: code, error_message: "m\u{7}".repeat(500), language_tag: String::new() })
        };
        assert!(matches!(map_sftp(st(StatusCode::NoSuchFile), "/a"), RemoteError::NotFound(_)));
        assert!(matches!(map_sftp(st(StatusCode::PermissionDenied), "/a"), RemoteError::Forbidden(_)));
        assert!(map_sftp(st(StatusCode::ConnectionLost), "/a").is_transient());
        assert!(map_sftp(SftpError::Timeout, "/a").is_transient());
        let RemoteError::Protocol(m) = map_sftp(st(StatusCode::Failure), "/a") else { panic!() };
        assert!(m.len() < 400 && !m.contains('\u{7}'));
        assert!(map_ssh(russh::Error::HUP).is_transient());
        assert!(matches!(map_ssh(russh::Error::UnknownKey), RemoteError::Forbidden(_)));
        let mut d = Vec::new();
        ssh_string(&mut d, "ab").unwrap();
        assert_eq!(d, [0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn sftp_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<Sftp>();
    }

    fn other_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn new_inside_an_async_runtime_is_an_error_not_a_panic() {
        let kh = std::env::temp_dir().join(format!("fc-kh-rt-{}", Sftp::random_suffix()));
        let err = other_runtime().block_on(async { Sftp::new(&loc("sftp://127.0.0.1:1", "a", ""), "pw", &kh).map(|_| ()) }).unwrap_err();
        assert!(err.to_string().contains("async runtime"), "{err}");
        // and from a task spawned on it
        let kh2 = kh.clone();
        let res =
            other_runtime().block_on(async move { tokio::spawn(async move { Sftp::new(&loc("sftp://127.0.0.1:1", "a", ""), "pw", &kh2).is_err() }).await });
        assert!(res.unwrap());
    }

    #[test]
    fn dropped_tasks_are_aborted() {
        let rt = other_runtime();
        let abort = rt.block_on(async {
            let jh = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            });
            let abort = jh.abort_handle();
            drop(Tasks(VecDeque::from([(0u64, jh)])));
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            abort
        });
        assert!(abort.is_finished());
    }

    #[test]
    fn backup_names_stay_in_the_folder() {
        assert!(backup_name("/srv/a/x.rev").starts_with("/srv/a/.x.rev.old-"));
        assert!(backup_name("x.rev").starts_with(".x.rev.old-"));
        let long = format!("d/{}", "n".repeat(250));
        let b = backup_name(&long);
        assert!(b.starts_with("d/.old-") && b.len() < 40, "{b}");
    }

    #[test]
    fn unreachable_server_is_a_network_error() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let kh = std::env::temp_dir().join(format!("fc-kh-unreach-{}", Sftp::random_suffix()));
        let err = Sftp::new(&loc(&format!("sftp://127.0.0.1:{port}"), "a", ""), "pw", &kh).unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    // --- against a real OpenSSH server, run as this user ------------------------------------

    struct Sshd {
        child: std::process::Child,
        dir: PathBuf,
    }

    impl Drop for Sshd {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn keygen(path: &Path, kind: &str) -> bool {
        let mut cmd = std::process::Command::new("ssh-keygen");
        cmd.args(["-q", "-t", kind, "-N", "", "-C", "filmcraft-test"]);
        if kind == "rsa" {
            cmd.args(["-b", "2048"]);
        }
        cmd.arg("-f").arg(path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
    }

    fn user_name() -> String {
        std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| {
            let out = std::process::Command::new("id").arg("-un").output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        })
    }

    /// The sshd binary: on PATH, or the usual system places.
    fn find_sshd() -> Option<PathBuf> {
        let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).map(|d| d.join("sshd")).collect::<Vec<_>>()).unwrap_or_default();
        on_path.into_iter().chain(["/usr/bin/sshd", "/usr/sbin/sshd"].map(PathBuf::from)).find(|p| p.is_file())
    }

    fn free_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    /// Starts sshd on a free port; `None` (skip) when OpenSSH isn't installed.
    fn start_sshd() -> Option<(Sshd, u16)> {
        let Some(sshd) = find_sshd() else {
            eprintln!("skipping: no sshd binary (PATH, /usr/bin, /usr/sbin)");
            return None;
        };
        if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
            eprintln!("skipping: ssh-keygen is missing");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("fc-sftp-test-{}-{}", std::process::id(), Sftp::random_suffix()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        for (k, kind) in [("host_key", "ed25519"), ("user_key", "ed25519"), ("other_key", "ed25519"), ("rsa_key", "rsa")] {
            assert!(keygen(&dir.join(k), kind), "ssh-keygen failed");
        }
        let mut authorized = std::fs::read_to_string(dir.join("user_key.pub")).unwrap();
        authorized.push_str(&std::fs::read_to_string(dir.join("rsa_key.pub")).unwrap());
        std::fs::write(dir.join("authorized_keys"), authorized).unwrap();
        let mut dir = Some(dir);
        // the port is free when we look, but someone may grab it before sshd binds: retry once
        for attempt in 0..2 {
            let d = dir.take()?;
            let port = free_port();
            let cfg = format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nAuthorizedKeysFile {d}/authorized_keys\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nPidFile {d}/sshd.pid\nSubsystem sftp internal-sftp\n",
                d = d.display()
            );
            std::fs::write(d.join("sshd_config"), cfg).unwrap();
            let log = std::fs::File::create(d.join("sshd.log")).unwrap();
            let child = std::process::Command::new(&sshd)
                .args(["-D", "-e", "-f"])
                .arg(d.join("sshd_config"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap();
            let mut guard = Sshd { child, dir: d };
            let start = std::time::Instant::now();
            loop {
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return Some((guard, port));
                }
                if guard.child.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(5) {
                    let log = std::fs::read_to_string(guard.dir.join("sshd.log")).unwrap_or_default();
                    if attempt == 0 && log.contains("ind") {
                        // "Bind to port … failed" / "Cannot bind": try another port
                        let _ = guard.child.kill();
                        let _ = guard.child.wait();
                        dir = Some(std::mem::take(&mut guard.dir));
                        break;
                    }
                    panic!("sshd did not start:\n{log}");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        panic!("sshd could not bind a port twice");
    }

    fn p(s: &str) -> RemotePath {
        RemotePath::parse(s).unwrap()
    }

    fn put(s: &Sftp, path: &str, bytes: &[u8]) -> Result<()> {
        s.write(&p(path), &mut &bytes[..], bytes.len() as u64, &mut |_| true)
    }

    #[test]
    fn round_trip_against_openssh() {
        let Some((sshd, port)) = start_sshd() else { return };
        let dir = sshd.dir.clone();
        let key = std::fs::read_to_string(dir.join("user_key")).unwrap();
        let kh = dir.join("client/known_hosts");
        // a root that doesn't exist yet: created on the first write
        let root = format!("{}/data/root/inner", dir.display());
        let location = loc(&format!("sftp://127.0.0.1:{port}"), &user_name(), &root);
        let a = Sftp::new(&location, &key, &kh).unwrap();
        let recorded = std::fs::read_to_string(&kh).unwrap();
        assert!(recorded.starts_with(&format!("127.0.0.1:{port} SHA256:")), "{recorded}");
        assert!(!format!("{a:?}").contains("PRIVATE"));

        // folders, writes (several chunks), listing, stat, ranges
        a.make_dir(&p("proj/media")).unwrap();
        a.make_dir(&p("proj/media")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut seen_progress = 0;
        a.write(&p("proj/a.rev"), &mut &big[..], big.len() as u64, &mut |n| {
            seen_progress = n;
            true
        })
        .unwrap();
        assert_eq!(seen_progress, big.len() as u64);
        let names = |dir: &str| -> Vec<String> {
            let mut v: Vec<String> = a.list(&p(dir)).unwrap().iter().map(|e| e.name().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(names(""), ["proj"]);
        assert_eq!(names("proj"), ["a.rev", "media"], "no temporary files left");
        let listed = a.list(&p("proj")).unwrap();
        assert!(listed.iter().any(|e| e.name() == "media" && e.is_dir));
        let e = a.stat(&p("proj/a.rev")).unwrap();
        assert_eq!((e.size, e.is_dir), (big.len() as u64, false));
        assert!(e.modified.is_some());
        assert!(a.stat(&RemotePath::root()).unwrap().is_dir);
        assert_eq!(a.read_range(&p("proj/a.rev"), 1, 3).unwrap(), &big[1..4]);
        assert_eq!(a.read_range(&p("proj/a.rev"), 99_999, 100_000).unwrap(), &big[99_999..199_999]);
        assert_eq!(a.read_range(&p("proj/a.rev"), 0, 1 << 20).unwrap(), big);
        assert_eq!(a.read_range(&p("proj/a.rev"), 10_000_000, 4).unwrap(), b"");
        assert_eq!(a.read_range(&p("proj/a.rev"), u64::MAX - 1, 4).unwrap(), b"");
        assert!(matches!(a.read_range(&p("proj/a.rev"), 0, MAX_READ_BYTES + 1), Err(RemoteError::TooLarge(_))));
        assert!(matches!(a.read_all(&p("proj/a.rev"), 10), Err(RemoteError::TooLarge(_))));
        assert!(matches!(a.read_range(&p("proj/missing"), 0, 4), Err(RemoteError::NotFound(_))));
        assert!(matches!(a.read_range(&p("proj/media"), 0, 4), Err(RemoteError::NotFound(_))));
        assert!(matches!(a.stat(&p("nope")), Err(RemoteError::NotFound(_))));
        assert!(matches!(a.list(&p("nope")), Err(RemoteError::NotFound(_))));

        // replace, cancel, size mismatch: the old content survives, no temporaries
        put(&a, "proj/a.rev", b"hello").unwrap();
        assert_eq!(a.read_all(&p("proj/a.rev"), 100).unwrap(), b"hello");
        assert!(matches!(a.write(&p("proj/a.rev"), &mut &big[..], big.len() as u64, &mut |_| false), Err(RemoteError::Cancelled)));
        assert!(a.write(&p("proj/a.rev"), &mut &b"abc"[..], 2, &mut |_| true).is_err());
        assert!(a.write(&p("proj/a.rev"), &mut &b"abc"[..], 4, &mut |_| true).is_err());
        assert_eq!(a.read_all(&p("proj/a.rev"), 100).unwrap(), b"hello");
        assert_eq!(names("proj"), ["a.rev", "media"]);
        assert!(matches!(put(&a, "proj/media", b"x"), Err(RemoteError::AlreadyExists(_))));
        assert!(put(&a, "proj/a.rev/x", b"x").is_err(), "a file can't be a folder");
        put(&a, "deep/er/new.bin", b"").unwrap();
        assert_eq!(a.stat(&p("deep/er/new.bin")).unwrap().size, 0);

        // create_new, rename (over a file), delete
        a.create_new(&p("proj/x/lock"), b"1").unwrap();
        assert!(matches!(a.create_new(&p("proj/x/lock"), b"2"), Err(RemoteError::AlreadyExists(_))));
        assert_eq!(a.read_all(&p("proj/x/lock"), 10).unwrap(), b"1");
        a.rename(&p("proj/a.rev"), &p("proj/b.rev")).unwrap();
        assert!(matches!(a.stat(&p("proj/a.rev")), Err(RemoteError::NotFound(_))));
        put(&a, "proj/c.rev", b"c").unwrap();
        a.rename(&p("proj/c.rev"), &p("proj/b.rev")).unwrap();
        assert_eq!(a.read_all(&p("proj/b.rev"), 10).unwrap(), b"c");
        a.rename(&p("proj/b.rev"), &p("other/moved.rev")).unwrap();
        assert!(matches!(a.rename(&p("proj/media"), &p("proj/m2")), Err(RemoteError::Forbidden(_))));
        assert!(matches!(a.rename(&p("other/moved.rev"), &p("proj/media")), Err(RemoteError::AlreadyExists(_))));
        assert!(matches!(a.rename(&p("nope"), &p("x")), Err(RemoteError::NotFound(_))));
        a.delete(&p("other/moved.rev")).unwrap();
        assert!(matches!(a.delete(&p("other/moved.rev")), Err(RemoteError::NotFound(_))));
        assert!(matches!(a.delete(&p("proj")), Err(RemoteError::Forbidden(_))));
        assert!(a.list(&p("other")).unwrap().is_empty());

        // lock files across two machines (two connections)
        let b = Sftp::new(&location, &key, &kh).unwrap();
        let me = |s: &str| crate::lock::LockInfo { holder: "Ana".into(), machine: s.into(), session: s.into(), since: 1 };
        let proj = p("proj/film.rev");
        assert_eq!(crate::lock::acquire(&a, &proj, &me("a")).unwrap(), crate::lock::Acquire::Acquired);
        assert_eq!(crate::lock::acquire(&b, &proj, &me("b")).unwrap(), crate::lock::Acquire::HeldBy(me("a")));
        assert_eq!(crate::lock::take_over(&b, &proj, &me("b")).unwrap(), crate::lock::Acquire::Acquired);
        assert!(!crate::lock::still_ours(&a, &proj, &me("a")).unwrap());
        assert!(crate::lock::release(&b, &proj, &me("b")).unwrap());
        drop(b);

        // a changed host key is refused; wrong key and password sign-ins are refused
        let kh2 = dir.join("client/known_hosts2");
        std::fs::write(&kh2, format!("127.0.0.1:{port} SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU\n")).unwrap();
        let err = Sftp::new(&location, &key, &kh2).unwrap_err();
        assert!(matches!(&err, RemoteError::Forbidden(m) if m.contains("identity changed")), "{err:?}");
        let other = std::fs::read_to_string(dir.join("other_key")).unwrap();
        let err = Sftp::new(&location, &other, &kh).unwrap_err();
        assert!(matches!(err, RemoteError::Unauthorized), "{err:?}");
        let err = Sftp::new(&location, "not-the-password", &kh).unwrap_err();
        assert!(matches!(err, RemoteError::Unauthorized), "{err:?}");
        assert!(!err.to_string().contains("not-the-password"));

        // a damaged entry for this server is refused, not silently re-trusted
        let kh3 = dir.join("client/known_hosts3");
        std::fs::write(&kh3, format!("127.0.0.1:{port} SHA256:\n")).unwrap();
        let err = Sftp::new(&location, &key, &kh3).unwrap_err();
        assert!(matches!(&err, RemoteError::Forbidden(m) if m.contains("damaged")), "{err:?}");
        assert_eq!(std::fs::read_to_string(&kh3).unwrap(), format!("127.0.0.1:{port} SHA256:\n"));
        // an unauthorized first contact still records the (verified) key; an unreachable one doesn't
        let kh4 = dir.join("client/known_hosts4");
        assert!(matches!(Sftp::new(&location, "pw", &kh4), Err(RemoteError::Unauthorized)));
        assert!(std::fs::read_to_string(&kh4).unwrap().starts_with(&format!("127.0.0.1:{port} SHA256:")));

        // RSA keys work, with a hint
        assert_eq!(a.key_warning(), None);
        let rsa = std::fs::read_to_string(dir.join("rsa_key")).unwrap();
        let r = Sftp::new(&location, &rsa, &kh).unwrap();
        assert!(r.key_warning().is_some_and(|w| w.contains("ed25519")));
        assert!(r.stat(&RemotePath::root()).unwrap().is_dir);
        // used (and dropped) inside an async runtime: an error and no panic
        other_runtime().block_on(async move {
            assert!(r.stat(&RemotePath::root()).is_err());
            drop(r);
        });

        // without posix-rename: replacing goes through a backup that is put back on failure
        a.test_disable_posix_rename(true);
        put(&a, "fb/t.rev", b"old").unwrap();
        put(&a, "fb/t.rev", b"new").unwrap();
        assert_eq!(a.read_all(&p("fb/t.rev"), 10).unwrap(), b"new");
        put(&a, "fb/u.rev", b"u").unwrap();
        a.rename(&p("fb/u.rev"), &p("fb/t.rev")).unwrap();
        assert_eq!(a.read_all(&p("fb/t.rev"), 10).unwrap(), b"u");
        assert_eq!(names("fb"), ["t.rev"], "no backups or temporaries left");
        a.test_fail_replace(1);
        assert!(put(&a, "fb/t.rev", b"lost?").is_err());
        assert_eq!(a.read_all(&p("fb/t.rev"), 10).unwrap(), b"u", "old content put back");
        assert_eq!(names("fb"), ["t.rev"]);
        a.test_fail_replace(2);
        let err = put(&a, "fb/t.rev", b"keep me").unwrap_err();
        assert!(err.to_string().contains("kept on the server"), "{err}");
        let left = names("fb");
        assert!(matches!(a.stat(&p("fb/t.rev")), Err(RemoteError::NotFound(_))));
        let backup = left.iter().find(|n| n.contains(".old-")).unwrap();
        let temp = left.iter().find(|n| n.contains(".part-")).unwrap();
        assert_eq!(a.read_all(&p(&format!("fb/{backup}")), 10).unwrap(), b"u", "the backup survives");
        assert_eq!(a.read_all(&p(&format!("fb/{temp}")), 10).unwrap(), b"keep me", "the new content is not deleted");
        a.test_fail_replace(0);
        a.test_disable_posix_rename(false);

        // the files really are under the root on disk
        assert!(dir.join("data/root/inner/proj/x/lock").is_file());
        drop(a);
        drop(sshd);
    }
}
