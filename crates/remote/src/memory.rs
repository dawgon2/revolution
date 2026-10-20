//! An in-memory [`RemoteStore`]: for tests, and the reference for the trait's semantics.
//! It can simulate a dropped connection part-way through a transfer.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{Entry, MAX_LIST_ENTRIES, MAX_READ_BYTES, Progress, RemoteError, RemotePath, RemoteStore, Result};

#[derive(Clone, Debug)]
enum Node {
    Dir,
    File { bytes: Vec<u8>, modified: i64, version: u64 },
}

#[derive(Debug, Default)]
struct State {
    /// Path → node; the root folder is implicit.
    nodes: BTreeMap<RemotePath, Node>,
    clock: i64,
    version: u64,
    /// Fail the next transfer with a network error after this many bytes.
    drop_after: Option<u64>,
}

/// A remote location held in memory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: Mutex<State>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes the next read or write fail with [`RemoteError::Network`] once `bytes` have moved
    /// (a dropped connection). Cleared after it fires.
    pub fn drop_connection_after(&self, bytes: u64) {
        self.lock().drop_after = Some(bytes);
    }

    /// Sets the clock used for modification times (seconds since the epoch).
    pub fn set_clock(&self, secs: i64) {
        self.lock().clock = secs;
    }

    /// Every file path, sorted (for tests).
    pub fn files(&self) -> Vec<RemotePath> {
        self.lock().nodes.iter().filter(|(_, n)| matches!(n, Node::File { .. })).map(|(p, _)| p.clone()).collect()
    }
}

fn is_dir(st: &State, p: &RemotePath) -> bool {
    p.is_root() || matches!(st.nodes.get(p), Some(Node::Dir))
}

fn make_parents(st: &mut State, path: &RemotePath) -> Result<()> {
    let mut cur = RemotePath::root();
    let segs: Vec<&str> = path.segments().collect();
    for seg in segs.iter().take(segs.len().saturating_sub(1)) {
        cur = cur.join(seg)?;
        match st.nodes.get(&cur) {
            Some(Node::Dir) => {}
            Some(Node::File { .. }) => return Err(RemoteError::AlreadyExists(format!("{cur} is a file"))),
            None => {
                st.nodes.insert(cur.clone(), Node::Dir);
            }
        }
    }
    Ok(())
}

fn put_file(st: &mut State, path: &RemotePath, bytes: Vec<u8>) {
    st.version = st.version.wrapping_add(1);
    let node = Node::File { bytes, modified: st.clock, version: st.version };
    st.nodes.insert(path.clone(), node);
}

fn entry(path: &RemotePath, node: &Node) -> Entry {
    match node {
        Node::Dir => Entry { path: path.clone(), is_dir: true, size: 0, modified: None, etag: None },
        Node::File { bytes, modified, version } => {
            Entry { path: path.clone(), is_dir: false, size: bytes.len() as u64, modified: Some(*modified), etag: Some(format!("\"{version}\"")) }
        }
    }
}

impl RemoteStore for MemoryStore {
    fn list(&self, dir: &RemotePath) -> Result<Vec<Entry>> {
        let st = self.lock();
        if !is_dir(&st, dir) {
            return Err(RemoteError::NotFound(dir.to_string()));
        }
        let out: Vec<Entry> =
            st.nodes.iter().filter(|(p, _)| p.parent().as_ref() == Some(dir) && !p.is_root()).take(MAX_LIST_ENTRIES).map(|(p, n)| entry(p, n)).collect();
        Ok(out)
    }

    fn stat(&self, path: &RemotePath) -> Result<Entry> {
        let st = self.lock();
        if path.is_root() {
            return Ok(entry(path, &Node::Dir));
        }
        st.nodes.get(path).map(|n| entry(path, n)).ok_or_else(|| RemoteError::NotFound(path.to_string()))
    }

    fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(RemoteError::TooLarge(format!("a read of {len} bytes")));
        }
        let mut st = self.lock();
        let Some(Node::File { bytes, .. }) = st.nodes.get(path) else {
            return Err(RemoteError::NotFound(path.to_string()));
        };
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
        let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX)).min(bytes.len());
        let out = bytes.get(start..end).unwrap_or_default().to_vec();
        if st.drop_after.take().is_some_and(|limit| (out.len() as u64) > limit) {
            return Err(RemoteError::Network("connection dropped (simulated)".into()));
        }
        Ok(out)
    }

    fn write(&self, path: &RemotePath, data: &mut dyn std::io::Read, size: u64, progress: Progress) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let drop_after = self.lock().drop_after.take();
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            let n = data.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
            if buf.len() as u64 > size {
                return Err(RemoteError::Protocol(format!("more than the announced {size} bytes")));
            }
            if drop_after.is_some_and(|d| buf.len() as u64 > d) {
                // all-or-nothing: nothing lands under `path`
                return Err(RemoteError::Network("connection dropped (simulated)".into()));
            }
            if !progress(buf.len() as u64) {
                return Err(RemoteError::Cancelled);
            }
        }
        if buf.len() as u64 != size {
            return Err(RemoteError::Protocol(format!("{} bytes instead of the announced {size}", buf.len())));
        }
        let mut st = self.lock();
        if is_dir(&st, path) {
            return Err(RemoteError::AlreadyExists(format!("{path} is a folder")));
        }
        make_parents(&mut st, path)?;
        put_file(&mut st, path, buf);
        Ok(())
    }

    fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> Result<()> {
        if path.is_root() {
            return Err(RemoteError::BadPath("can't write the root".into()));
        }
        let mut st = self.lock();
        if st.nodes.contains_key(path) {
            return Err(RemoteError::AlreadyExists(path.to_string()));
        }
        make_parents(&mut st, path)?;
        put_file(&mut st, path, bytes.to_vec());
        Ok(())
    }

    fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        let mut st = self.lock();
        match st.nodes.get(from) {
            Some(Node::File { .. }) => {}
            Some(Node::Dir) => return Err(RemoteError::Forbidden(format!("{from} is a folder"))),
            None => return Err(RemoteError::NotFound(from.to_string())),
        }
        if is_dir(&st, to) {
            return Err(RemoteError::AlreadyExists(format!("{to} is a folder")));
        }
        make_parents(&mut st, to)?;
        let node = st.nodes.remove(from).ok_or_else(|| RemoteError::NotFound(from.to_string()))?;
        st.nodes.insert(to.clone(), node);
        Ok(())
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        let mut st = self.lock();
        match st.nodes.get(path) {
            Some(Node::File { .. }) => {
                st.nodes.remove(path);
                Ok(())
            }
            Some(Node::Dir) => Err(RemoteError::Forbidden(format!("{path} is a folder"))),
            None => Err(RemoteError::NotFound(path.to_string())),
        }
    }

    fn make_dir(&self, dir: &RemotePath) -> Result<()> {
        if dir.is_root() {
            return Ok(());
        }
        let mut st = self.lock();
        make_parents(&mut st, dir)?;
        match st.nodes.get(dir) {
            Some(Node::Dir) => Ok(()),
            Some(Node::File { .. }) => Err(RemoteError::AlreadyExists(format!("{dir} is a file"))),
            None => {
                st.nodes.insert(dir.clone(), Node::Dir);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> RemotePath {
        RemotePath::parse(s).unwrap()
    }

    fn put(store: &MemoryStore, path: &str, bytes: &[u8]) -> Result<()> {
        store.write(&p(path), &mut &bytes[..], bytes.len() as u64, &mut |_| true)
    }

    #[test]
    fn write_list_read_rename_delete() {
        let s = MemoryStore::new();
        put(&s, "proj/a.rev", b"hello").unwrap();
        let names: Vec<String> = s.list(&RemotePath::root()).unwrap().iter().map(|e| e.name().to_string()).collect();
        assert_eq!(names, ["proj"]);
        let e = s.stat(&p("proj/a.rev")).unwrap();
        assert_eq!((e.size, e.is_dir), (5, false));
        assert_eq!(s.read_range(&p("proj/a.rev"), 1, 3).unwrap(), b"ell");
        assert_eq!(s.read_range(&p("proj/a.rev"), 99, 3).unwrap(), b"");
        assert_eq!(s.read_all(&p("proj/a.rev"), 5).unwrap(), b"hello");
        assert!(matches!(s.read_all(&p("proj/a.rev"), 4), Err(RemoteError::TooLarge(_))));
        s.rename(&p("proj/a.rev"), &p("proj/b.rev")).unwrap();
        assert!(matches!(s.stat(&p("proj/a.rev")), Err(RemoteError::NotFound(_))));
        s.delete(&p("proj/b.rev")).unwrap();
        assert!(s.list(&p("proj")).unwrap().is_empty());
        assert!(matches!(s.delete(&p("proj")), Err(RemoteError::Forbidden(_))));
    }

    #[test]
    fn create_new_has_exactly_one_winner() {
        let s = std::sync::Arc::new(MemoryStore::new());
        let wins: usize = (0..8)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || s.create_new(&p("x/lock"), format!("{i}").as_bytes()).is_ok())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| usize::from(h.join().unwrap_or(false)))
            .sum();
        assert_eq!(wins, 1);
    }

    #[test]
    fn a_dropped_write_leaves_nothing() {
        let s = MemoryStore::new();
        put(&s, "a", b"old").unwrap();
        s.drop_connection_after(10);
        let big = vec![7u8; 200 << 10];
        let err = put(&s, "a", &big).unwrap_err();
        assert!(err.is_transient());
        assert_eq!(s.read_all(&p("a"), 10).unwrap(), b"old");
        // the drop fires once
        put(&s, "a", &big).unwrap();
    }

    #[test]
    fn size_mismatch_and_hostile_reads_are_refused() {
        let s = MemoryStore::new();
        assert!(s.write(&p("a"), &mut &b"abc"[..], 2, &mut |_| true).is_err());
        assert!(s.write(&p("a"), &mut &b"abc"[..], 4, &mut |_| true).is_err());
        assert!(s.stat(&p("a")).is_err());
        put(&s, "a", b"abc").unwrap();
        assert!(s.read_range(&p("a"), u64::MAX, u64::MAX).is_err());
        assert_eq!(s.read_range(&p("a"), u64::MAX, 4).unwrap(), b"");
        assert!(put(&s, "a/b", b"x").is_err(), "a file can't be a folder");
        assert!(matches!(s.write(&p("a"), &mut &b"x"[..], 1, &mut |_| false), Err(RemoteError::Cancelled)));
    }
}
