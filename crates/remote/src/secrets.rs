//! Where remote locations' passwords, keys and secret keys live (STORAGE-PLAN S2 item 2): the
//! system keyring by default (the app installs a Secret Service / KWallet store at start-up), a
//! file only if the user allows it. Never in project files, settings or logs.
//!
//! [`install`] sets the process's store once; [`store`] returns it (a [`MemorySecrets`] until
//! something is installed, so a headless run without a keyring forgets secrets at exit rather
//! than writing them anywhere).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::{RemoteError, Result};

/// The longest secret stored (a private key in PEM is a few KiB).
pub const MAX_SECRET_BYTES: usize = 64 << 10;

/// A place to keep secrets by key (`revolution-remote:<location id>`).
pub trait SecretStore: Send + Sync {
    /// Shown in Settings ▸ Remote locations ("System keyring (KWallet)", "File (not encrypted)").
    fn describe(&self) -> String;
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, secret: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

fn check(key: &str, secret: Option<&str>) -> Result<()> {
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
        return Err(RemoteError::BadPath("bad secret name".into()));
    }
    if secret.is_some_and(|s| s.len() > MAX_SECRET_BYTES) {
        return Err(RemoteError::TooLarge(format!("a secret longer than {MAX_SECRET_BYTES} bytes")));
    }
    Ok(())
}

static STORE: OnceLock<Mutex<Arc<dyn SecretStore>>> = OnceLock::new();

fn slot() -> &'static Mutex<Arc<dyn SecretStore>> {
    STORE.get_or_init(|| Mutex::new(Arc::new(MemorySecrets::default())))
}

/// Makes `store` the process's secret store (the app: the system keyring, or the file store if
/// the user allowed it).
pub fn install(store: Arc<dyn SecretStore>) {
    *slot().lock().unwrap_or_else(PoisonError::into_inner) = store;
}

/// The process's secret store.
pub fn store() -> Arc<dyn SecretStore> {
    slot().lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Secrets held in memory only (tests; runs without a keyring).
#[derive(Default)]
pub struct MemorySecrets {
    map: Mutex<BTreeMap<String, String>>,
}

impl SecretStore for MemorySecrets {
    fn describe(&self) -> String {
        "Memory only (forgotten when the app quits)".into()
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        check(key, None)?;
        Ok(self.map.lock().unwrap_or_else(PoisonError::into_inner).get(key).cloned())
    }
    fn set(&self, key: &str, secret: &str) -> Result<()> {
        check(key, Some(secret))?;
        self.map.lock().unwrap_or_else(PoisonError::into_inner).insert(key.into(), secret.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        check(key, None)?;
        self.map.lock().unwrap_or_else(PoisonError::into_inner).remove(key);
        Ok(())
    }
}

/// The fallback the user must allow: a JSON file readable only by the user (mode 0600 on
/// Unix), not encrypted. Kept in the app's config folder, never next to a project or a log.
pub struct FileSecrets {
    path: PathBuf,
    lock: Mutex<()>,
}

/// The largest secrets file read.
const MAX_FILE_BYTES: u64 = 16 << 20;

impl FileSecrets {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileSecrets { path: path.into(), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<BTreeMap<String, String>> {
        use std::io::Read;
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(RemoteError::TooLarge(format!("{} is larger than {MAX_FILE_BYTES} bytes", self.path.display())));
        }
        // a damaged file reads as empty rather than failing every location
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        use std::io::Write;
        let bytes = serde_json::to_vec_pretty(map).map_err(|e| RemoteError::Protocol(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            // an existing temp file keeps its old mode: set it explicitly
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(&bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for FileSecrets {
    fn describe(&self) -> String {
        format!("File, not encrypted ({})", self.path.display())
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        check(key, None)?;
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(self.load()?.get(key).cloned())
    }
    fn set(&self, key: &str, secret: &str) -> Result<()> {
        check(key, Some(secret))?;
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.load()?;
        map.insert(key.into(), secret.into());
        self.save(&map)
    }
    fn delete(&self, key: &str) -> Result<()> {
        check(key, None)?;
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.load()?;
        if map.remove(key).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }
}

static FILE_FALLBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Settings ▸ Remote locations ▸ "Keep passwords in a file when no system keyring works": the
/// app sets this from the preferences; [`WithFileFallback`] reads it on every call.
pub fn set_file_fallback_allowed(allowed: bool) {
    FILE_FALLBACK.store(allowed, std::sync::atomic::Ordering::Relaxed);
}

pub fn file_fallback_allowed() -> bool {
    FILE_FALLBACK.load(std::sync::atomic::Ordering::Relaxed)
}

/// The system keyring, and the file store only when the keyring fails and the user allowed it
/// ([`set_file_fallback_allowed`]). A secret found in neither reads as `None`.
pub struct WithFileFallback {
    primary: Arc<dyn SecretStore>,
    file: FileSecrets,
}

impl WithFileFallback {
    pub fn new(primary: Arc<dyn SecretStore>, file: FileSecrets) -> Self {
        WithFileFallback { primary, file }
    }
}

impl SecretStore for WithFileFallback {
    fn describe(&self) -> String {
        if file_fallback_allowed() { format!("{}; if it fails: {}", self.primary.describe(), self.file.describe()) } else { self.primary.describe() }
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        match self.primary.get(key) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) if file_fallback_allowed() => self.file.get(key),
            Ok(None) => Ok(None),
            Err(_) if file_fallback_allowed() => self.file.get(key),
            Err(e) => Err(e),
        }
    }
    fn set(&self, key: &str, secret: &str) -> Result<()> {
        match self.primary.set(key, secret) {
            Ok(()) => {
                // never leave an older copy in the file once the keyring has it
                let _ = self.file.delete(key);
                Ok(())
            }
            Err(_) if file_fallback_allowed() => self.file.set(key, secret),
            Err(e) => Err(e),
        }
    }
    fn delete(&self, key: &str) -> Result<()> {
        let a = self.primary.delete(key);
        let b = self.file.delete(key);
        a.or(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_file_stores_round_trip() {
        let dir = std::env::temp_dir().join(format!("rev-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = FileSecrets::new(dir.join("secrets.json"));
        let mem = MemorySecrets::default();
        for s in [&file as &dyn SecretStore, &mem] {
            assert_eq!(s.get("revolution-remote:a").unwrap(), None);
            s.set("revolution-remote:a", "hunter2").unwrap();
            assert_eq!(s.get("revolution-remote:a").unwrap().as_deref(), Some("hunter2"));
            s.delete("revolution-remote:a").unwrap();
            assert_eq!(s.get("revolution-remote:a").unwrap(), None);
            assert!(s.set("", "x").is_err());
            assert!(s.set("k\n", "x").is_err());
            assert!(s.set("k", &"x".repeat(MAX_SECRET_BYTES + 1)).is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set("k", "v").unwrap();
            let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // a damaged file reads as empty, and is replaced on the next set
        std::fs::write(file.path(), b"{oops").unwrap();
        assert_eq!(file.get("k").unwrap(), None);
        file.set("k", "v2").unwrap();
        assert_eq!(file.get("k").unwrap().as_deref(), Some("v2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Broken;
    impl SecretStore for Broken {
        fn describe(&self) -> String {
            "broken".into()
        }
        fn get(&self, _: &str) -> Result<Option<String>> {
            Err(RemoteError::Protocol("no keyring".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Err(RemoteError::Protocol("no keyring".into()))
        }
        fn delete(&self, _: &str) -> Result<()> {
            Err(RemoteError::Protocol("no keyring".into()))
        }
    }

    #[test]
    fn file_fallback_only_when_allowed() {
        let dir = std::env::temp_dir().join(format!("rev-secrets-fb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = WithFileFallback::new(Arc::new(Broken), FileSecrets::new(dir.join("s.json")));
        set_file_fallback_allowed(false);
        assert!(s.set("k", "v").is_err());
        assert!(!dir.join("s.json").exists(), "nothing written without the user's OK");
        set_file_fallback_allowed(true);
        s.set("k", "v").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        set_file_fallback_allowed(false);
        // a working keyring removes the file's older copy
        let file = FileSecrets::new(dir.join("s.json"));
        file.set("k", "old").unwrap();
        let ok = WithFileFallback::new(Arc::new(MemorySecrets::default()), FileSecrets::new(dir.join("s.json")));
        ok.set("k", "new").unwrap();
        assert_eq!(file.get("k").unwrap(), None);
        assert_eq!(ok.get("k").unwrap().as_deref(), Some("new"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_store_is_shared() {
        let mem = Arc::new(MemorySecrets::default());
        install(mem.clone());
        store().set("revolution-remote:t", "s").unwrap();
        assert_eq!(mem.get("revolution-remote:t").unwrap().as_deref(), Some("s"));
    }
}
