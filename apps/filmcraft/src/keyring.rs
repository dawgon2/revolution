//! Remote locations' passwords in the system keyring (STORAGE-PLAN S2 item 2): the freedesktop
//! Secret Service over D-Bus (KWallet's ksecretd, GNOME Keyring, KeePassXC), from its public
//! specification. Items carry the attributes `application = revolution`, `key = <name>`.
//!
//! The session uses the spec's `plain` algorithm (the secret crosses the private session bus
//! unencrypted, as with most clients); a locked collection asks the keyring to show its unlock
//! prompt and waits up to [`PROMPT_TIMEOUT`].

#[cfg(target_os = "linux")]
pub use imp::LazyKeyring;

/// Makes the system keyring the process's secret store (connected on first use, so start-up
/// never waits on D-Bus or wakes the keyring daemon), with `<data dir>/remote/secrets.json` as
/// the fallback the user may allow in Settings ▸ Remote locations.
pub fn install(data_dir: Option<std::path::PathBuf>) {
    use filmcraft_remote::secrets::{FileSecrets, SecretStore, WithFileFallback};
    #[cfg(target_os = "linux")]
    let primary: std::sync::Arc<dyn SecretStore> = std::sync::Arc::new(LazyKeyring::default());
    #[cfg(not(target_os = "linux"))]
    let primary: std::sync::Arc<dyn SecretStore> = std::sync::Arc::new(filmcraft_remote::secrets::MemorySecrets::default());
    let Some(dir) = data_dir else {
        filmcraft_remote::secrets::install(primary);
        return;
    };
    let file = FileSecrets::new(dir.join("remote").join("secrets.json"));
    filmcraft_remote::secrets::install(std::sync::Arc::new(WithFileFallback::new(primary, file)));
}

/// The longest wait for the keyring's own unlock / confirm prompt.
#[cfg(target_os = "linux")]
pub const PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use filmcraft_remote::secrets::{MAX_SECRET_BYTES, SecretStore};
    use filmcraft_remote::{RemoteError, Result};
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.secrets";
    const SERVICE_PATH: &str = "/org/freedesktop/secrets";
    const SERVICE: &str = "org.freedesktop.Secret.Service";
    const COLLECTION: &str = "org.freedesktop.Secret.Collection";
    const ITEM: &str = "org.freedesktop.Secret.Item";
    const PROMPT: &str = "org.freedesktop.Secret.Prompt";
    const APP: &str = "revolution";

    /// (session, parameters, value, content type): the spec's `Secret` struct.
    type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

    fn err(e: impl std::fmt::Display) -> RemoteError {
        RemoteError::Protocol(format!("system keyring: {e}"))
    }

    /// The Secret Service as a [`SecretStore`].
    pub struct SecretService {
        conn: Connection,
    }

    impl SecretService {
        /// Connects to the session bus; `None` when no keyring service answers (headless
        /// sessions, minimal desktops).
        pub fn connect() -> Option<Self> {
            let conn = zbus::blocking::connection::Builder::session().ok()?.method_timeout(std::time::Duration::from_secs(5)).build().ok()?;
            let me = SecretService { conn };
            // the service must exist (it may be D-Bus activated: a ping starts it)
            me.session().ok()?;
            Some(me)
        }

        fn proxy<'a>(&'a self, path: &'a str, iface: &'a str) -> Result<Proxy<'a>> {
            Proxy::new(&self.conn, DEST, path, iface).map_err(err)
        }

        fn service(&self) -> Result<Proxy<'_>> {
            self.proxy(SERVICE_PATH, SERVICE)
        }

        fn session(&self) -> Result<OwnedObjectPath> {
            let (_out, path): (OwnedValue, OwnedObjectPath) = self.service()?.call("OpenSession", &("plain", Value::from(""))).map_err(err)?;
            Ok(path)
        }

        fn attributes(key: &str) -> HashMap<&str, &str> {
            HashMap::from([("application", APP), ("key", key)])
        }

        /// Runs the keyring's prompt (unlock / confirm) and waits for it, bounded.
        fn prompt(&self, prompt: &OwnedObjectPath) -> Result<()> {
            if prompt.as_str() == "/" {
                return Ok(());
            }
            let proxy = Proxy::new(&self.conn, DEST, prompt.clone().into_inner(), PROMPT).map_err(err)?;
            let signals = proxy.receive_signal("Completed").map_err(err)?;
            proxy.call::<_, _, ()>("Prompt", &("",)).map_err(err)?;
            let (tx, rx) = std::sync::mpsc::channel();
            // the signal iterator blocks; a prompt nobody answers leaves this thread waiting on a
            // message that never comes, which costs nothing
            std::thread::spawn(move || {
                let mut signals = signals;
                let dismissed = signals.next().and_then(|m| m.body().deserialize::<(bool, OwnedValue)>().ok()).is_none_or(|(d, _)| d);
                let _ = tx.send(dismissed);
            });
            match rx.recv_timeout(super::PROMPT_TIMEOUT) {
                Ok(false) => Ok(()),
                Ok(true) => Err(RemoteError::Cancelled),
                Err(_) => Err(err("the keyring's prompt was not answered")),
            }
        }

        fn unlock(&self, objects: &[OwnedObjectPath]) -> Result<()> {
            if objects.is_empty() {
                return Ok(());
            }
            let (_done, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self.service()?.call("Unlock", &(objects,)).map_err(err)?;
            self.prompt(&prompt)
        }

        fn find(&self, key: &str) -> Result<Vec<OwnedObjectPath>> {
            let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
                self.service()?.call("SearchItems", &(Self::attributes(key),)).map_err(err)?;
            self.unlock(&locked)?;
            unlocked.extend(locked);
            Ok(unlocked)
        }
    }

    impl SecretStore for SecretService {
        fn describe(&self) -> String {
            "System keyring".into()
        }

        fn get(&self, key: &str) -> Result<Option<String>> {
            let items = self.find(key)?;
            let Some(first) = items.first() else { return Ok(None) };
            let session = self.session()?;
            let secrets: HashMap<OwnedObjectPath, Secret> = self.service()?.call("GetSecrets", &(vec![first.clone()], &session)).map_err(err)?;
            let Some((_, _, value, _)) = secrets.get(first) else { return Ok(None) };
            if value.len() > MAX_SECRET_BYTES {
                return Err(RemoteError::TooLarge("a keyring secret".into()));
            }
            String::from_utf8(value.clone()).map(Some).map_err(|_| err("the stored secret is not text"))
        }

        fn set(&self, key: &str, secret: &str) -> Result<()> {
            if secret.len() > MAX_SECRET_BYTES {
                return Err(RemoteError::TooLarge(format!("a secret longer than {MAX_SECRET_BYTES} bytes")));
            }
            let collection: OwnedObjectPath = self.service()?.call("ReadAlias", &("default",)).map_err(err)?;
            if collection.as_str() == "/" {
                return Err(err("there is no default keyring collection"));
            }
            self.unlock(std::slice::from_ref(&collection))?;
            let session = self.session()?;
            let mut props: HashMap<&str, Value> = HashMap::new();
            props.insert("org.freedesktop.Secret.Item.Label", Value::from(format!("Revolution: {key}")));
            props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(Self::attributes(key)));
            let value: Secret = (session, Vec::new(), secret.as_bytes().to_vec(), "text/plain".into());
            let proxy = self.proxy(collection.as_str(), COLLECTION)?;
            let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy.call("CreateItem", &(props, value, true)).map_err(err)?;
            self.prompt(&prompt)
        }

        fn delete(&self, key: &str) -> Result<()> {
            for item in self.find(key)? {
                let prompt: OwnedObjectPath = self.proxy(item.as_str(), ITEM)?.call("Delete", &()).map_err(err)?;
                self.prompt(&prompt)?;
            }
            Ok(())
        }
    }

    /// [`SecretService`] connected on first use; without a keyring service every call fails
    /// with a plain-language error (nothing is written anywhere else).
    #[derive(Default)]
    pub struct LazyKeyring {
        inner: std::sync::OnceLock<Option<SecretService>>,
    }

    impl LazyKeyring {
        fn get_service(&self) -> Result<&SecretService> {
            self.inner.get_or_init(SecretService::connect).as_ref().ok_or_else(|| {
                err("no keyring service is running (install KWallet, GNOME Keyring or KeePassXC, or allow the file store in Settings ▸ Remote locations)")
            })
        }
    }

    impl SecretStore for LazyKeyring {
        fn describe(&self) -> String {
            "System keyring".into()
        }
        fn get(&self, key: &str) -> Result<Option<String>> {
            self.get_service()?.get(key)
        }
        fn set(&self, key: &str, secret: &str) -> Result<()> {
            self.get_service()?.set(key, secret)
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.get_service()?.delete(key)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Writes, reads and deletes one throw-away item in the real keyring. Ignored by default
        /// (it touches the user's keyring and may show its unlock prompt):
        /// `cargo test -p filmcraft keyring -- --ignored`.
        #[test]
        #[ignore]
        fn round_trip_in_the_session_keyring() {
            let Some(ks) = SecretService::connect() else {
                eprintln!("no Secret Service on the session bus: skipped");
                return;
            };
            let key = "revolution-remote:keyring-self-test";
            ks.set(key, "s3cret").unwrap();
            assert_eq!(ks.get(key).unwrap().as_deref(), Some("s3cret"));
            ks.set(key, "changed").unwrap();
            assert_eq!(ks.get(key).unwrap().as_deref(), Some("changed"));
            ks.delete(key).unwrap();
            assert_eq!(ks.get(key).unwrap(), None);
        }
    }
}
