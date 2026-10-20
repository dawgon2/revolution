//! Project lock files (STORAGE-PLAN S2 item 4): so two machines can't overwrite each other's
//! work. Opening a project on a remote location creates `.<name>.revlock` next to it with who
//! holds it; a second machine sees the lock and opens read-only, or takes it over after a
//! warning. Built on [`RemoteStore::create_new`], so two machines racing for a free project get
//! exactly one winner.

use serde::{Deserialize, Serialize};

use crate::{RemoteError, RemotePath, RemoteStore, Result};

/// The largest lock file read (it holds a few short strings).
pub const MAX_LOCK_BYTES: u64 = 16 << 10;

/// Who holds a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockInfo {
    /// The user's name as the app shows it.
    pub holder: String,
    /// The machine's host name.
    pub machine: String,
    /// A random id of this app session: tells our own lock from another window's on the same
    /// machine, and makes release / refresh safe.
    pub session: String,
    /// When the lock was taken, seconds since the Unix epoch.
    pub since: i64,
}

/// The outcome of [`acquire`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acquire {
    /// The project is ours now.
    Acquired,
    /// Someone else holds it (open read-only, or [`take_over`] after warning the user).
    HeldBy(LockInfo),
}

/// The lock file of the project file `project`: `.<name>.revlock` in the same folder.
pub fn lock_path(project: &RemotePath) -> Result<RemotePath> {
    let parent = project.parent().ok_or_else(|| RemoteError::BadPath("the root is not a project".into()))?;
    parent.join(&format!(".{}.revlock", project.name()))
}

/// Reads the current lock (`None` when the project is free). A damaged lock file reads as held
/// by "unknown", so it is never silently ignored.
pub fn read(store: &dyn RemoteStore, project: &RemotePath) -> Result<Option<LockInfo>> {
    let path = lock_path(project)?;
    match store.read_all(&path, MAX_LOCK_BYTES) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).unwrap_or_else(|_| LockInfo {
            holder: "unknown".into(),
            machine: "unknown".into(),
            session: String::new(),
            since: 0,
        }))),
        Err(RemoteError::NotFound(_)) => Ok(None),
        Err(RemoteError::TooLarge(_)) => Ok(Some(LockInfo { holder: "unknown".into(), machine: "unknown".into(), session: String::new(), since: 0 })),
        Err(e) => Err(e),
    }
}

fn encode(me: &LockInfo) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(me).map_err(|e| RemoteError::Protocol(e.to_string()))
}

/// Takes the project's lock if it is free (or already ours: same session).
pub fn acquire(store: &dyn RemoteStore, project: &RemotePath, me: &LockInfo) -> Result<Acquire> {
    let path = lock_path(project)?;
    match store.create_new(&path, &encode(me)?) {
        Ok(()) => Ok(Acquire::Acquired),
        Err(RemoteError::AlreadyExists(_)) => match read(store, project)? {
            Some(held) if !me.session.is_empty() && held.session == me.session => Ok(Acquire::Acquired),
            Some(held) => Ok(Acquire::HeldBy(held)),
            // released between our two calls: try once more
            None => match store.create_new(&path, &encode(me)?) {
                Ok(()) => Ok(Acquire::Acquired),
                Err(RemoteError::AlreadyExists(_)) => Ok(read(store, project)?.map_or(Acquire::Acquired, Acquire::HeldBy)),
                Err(e) => Err(e),
            },
        },
        Err(e) => Err(e),
    }
}

/// Takes the lock from whoever holds it (the user confirmed the warning). If a third machine
/// grabs it in between, that one wins and is returned.
pub fn take_over(store: &dyn RemoteStore, project: &RemotePath, me: &LockInfo) -> Result<Acquire> {
    let path = lock_path(project)?;
    match store.delete(&path) {
        Ok(()) | Err(RemoteError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    acquire(store, project, me)
}

/// Releases our lock. A lock someone else took over in the meantime is left alone (returns
/// false).
pub fn release(store: &dyn RemoteStore, project: &RemotePath, me: &LockInfo) -> Result<bool> {
    match read(store, project)? {
        Some(held) if held.session == me.session && !me.session.is_empty() => match store.delete(&lock_path(project)?) {
            Ok(()) | Err(RemoteError::NotFound(_)) => Ok(true),
            Err(e) => Err(e),
        },
        _ => Ok(false),
    }
}

/// Whether we still hold the lock (checked before every save: a project taken over by another
/// machine is not overwritten, it is saved as a conflict copy instead).
pub fn still_ours(store: &dyn RemoteStore, project: &RemotePath, me: &LockInfo) -> Result<bool> {
    Ok(read(store, project)?.is_some_and(|held| !me.session.is_empty() && held.session == me.session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    fn me(session: &str) -> LockInfo {
        LockInfo { holder: "Ana".into(), machine: format!("pc-{session}"), session: session.into(), since: 1_700_000_000 }
    }

    #[test]
    fn second_machine_sees_the_lock_and_can_take_over() {
        let s = MemoryStore::new();
        let proj = RemotePath::parse("films/cut.rev").unwrap();
        assert_eq!(lock_path(&proj).unwrap().as_str(), "films/.cut.rev.revlock");
        assert_eq!(acquire(&s, &proj, &me("a")).unwrap(), Acquire::Acquired);
        assert_eq!(acquire(&s, &proj, &me("a")).unwrap(), Acquire::Acquired, "re-entrant for the same session");
        assert_eq!(acquire(&s, &proj, &me("b")).unwrap(), Acquire::HeldBy(me("a")));
        assert!(!release(&s, &proj, &me("b")).unwrap(), "b can't release a's lock");
        assert_eq!(take_over(&s, &proj, &me("b")).unwrap(), Acquire::Acquired);
        assert!(!still_ours(&s, &proj, &me("a")).unwrap());
        assert!(still_ours(&s, &proj, &me("b")).unwrap());
        assert!(!release(&s, &proj, &me("a")).unwrap(), "a's late release leaves b's lock");
        assert!(release(&s, &proj, &me("b")).unwrap());
        assert_eq!(read(&s, &proj).unwrap(), None);
    }

    #[test]
    fn damaged_or_huge_lock_reads_as_held() {
        let s = MemoryStore::new();
        let proj = RemotePath::parse("cut.rev").unwrap();
        s.create_new(&lock_path(&proj).unwrap(), b"{not json").unwrap();
        assert!(matches!(acquire(&s, &proj, &me("a")).unwrap(), Acquire::HeldBy(h) if h.holder == "unknown"));
        let big = vec![b' '; (MAX_LOCK_BYTES + 1) as usize];
        let lp = lock_path(&proj).unwrap();
        s.write(&lp, &mut &big[..], big.len() as u64, &mut |_| true).unwrap();
        assert!(matches!(read(&s, &proj).unwrap(), Some(h) if h.holder == "unknown"));
        // an empty session never matches another empty session
        assert!(matches!(acquire(&s, &proj, &me("")).unwrap(), Acquire::HeldBy(_)));
        assert!(lock_path(&RemotePath::root()).is_err());
    }

    #[test]
    fn racing_machines_get_one_winner() {
        let s = std::sync::Arc::new(MemoryStore::new());
        let proj = RemotePath::parse("p/x.rev").unwrap();
        let won: usize = (0..8)
            .map(|i| {
                let (s, proj) = (s.clone(), proj.clone());
                std::thread::spawn(move || acquire(&*s, &proj, &me(&i.to_string())).is_ok_and(|a| a == Acquire::Acquired))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| usize::from(h.join().unwrap_or(false)))
            .sum();
        assert_eq!(won, 1);
    }
}
