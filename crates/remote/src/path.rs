//! Checked remote paths.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{RemoteError, Result};

/// The longest path accepted, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// The longest single segment, in bytes (what common file systems allow).
pub const MAX_SEGMENT_BYTES: usize = 255;
/// The deepest path accepted.
pub const MAX_DEPTH: usize = 64;

/// A path inside a remote location: relative to its root, `/`-separated, no empty, `.` or `..`
/// segments, no backslashes or control characters. The root is the empty path. Leading and
/// trailing slashes are dropped when parsing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RemotePath(String);

impl RemotePath {
    /// The location's root.
    pub fn root() -> Self {
        RemotePath(String::new())
    }

    /// Parses and checks a path (see the type's docs).
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > MAX_PATH_BYTES {
            return Err(RemoteError::BadPath(format!("longer than {MAX_PATH_BYTES} bytes")));
        }
        let trimmed = s.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(Self::root());
        }
        let mut depth = 0usize;
        for seg in trimmed.split('/') {
            check_segment(seg)?;
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(RemoteError::BadPath(format!("deeper than {MAX_DEPTH} folders")));
            }
        }
        Ok(RemotePath(trimmed.to_string()))
    }

    /// `self/name` (one segment).
    pub fn join(&self, name: &str) -> Result<Self> {
        check_segment(name)?;
        if self.depth() >= MAX_DEPTH {
            return Err(RemoteError::BadPath(format!("deeper than {MAX_DEPTH} folders")));
        }
        let s = if self.0.is_empty() { name.to_string() } else { format!("{}/{name}", self.0) };
        if s.len() > MAX_PATH_BYTES {
            return Err(RemoteError::BadPath(format!("longer than {MAX_PATH_BYTES} bytes")));
        }
        Ok(RemotePath(s))
    }

    /// The containing folder (`None` for the root).
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => RemotePath(self.0.get(..i).unwrap_or_default().to_string()),
            None => Self::root(),
        })
    }

    /// The last segment ("" for the root).
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The segments, root first.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }

    pub fn depth(&self) -> usize {
        self.segments().count()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `self` is `other` or inside it.
    pub fn starts_with(&self, other: &RemotePath) -> bool {
        other.0.is_empty() || self.0 == other.0 || self.0.strip_prefix(other.0.as_str()).is_some_and(|rest| rest.starts_with('/'))
    }
}

fn check_segment(seg: &str) -> Result<()> {
    if seg.is_empty() || seg == "." || seg == ".." {
        return Err(RemoteError::BadPath(format!("segment {seg:?} is not allowed")));
    }
    if seg.len() > MAX_SEGMENT_BYTES {
        return Err(RemoteError::BadPath(format!("a name is longer than {MAX_SEGMENT_BYTES} bytes")));
    }
    if seg.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err(RemoteError::BadPath(format!("{seg:?} contains a slash, backslash or control character")));
    }
    Ok(())
}

impl TryFrom<String> for RemotePath {
    type Error = RemoteError;
    fn try_from(s: String) -> Result<Self> {
        RemotePath::parse(&s)
    }
}

impl From<RemotePath> for String {
    fn from(p: RemotePath) -> String {
        p.0
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        assert_eq!(RemotePath::parse("/a/b/").unwrap().as_str(), "a/b");
        assert!(RemotePath::parse("").unwrap().is_root());
        assert!(RemotePath::parse("///").unwrap().is_root());
        assert_eq!(RemotePath::parse("a/b c/é.mov").unwrap().name(), "é.mov");
    }

    #[test]
    fn refuses_hostile_paths() {
        for bad in ["a/../b", "..", "a/./b", "a//b", "a\\b", "a/\u{0}b", "a/\nb", "x/\u{7f}"] {
            assert!(RemotePath::parse(bad).is_err(), "{bad:?} accepted");
        }
        assert!(RemotePath::parse(&"a".repeat(MAX_SEGMENT_BYTES + 1)).is_err());
        assert!(RemotePath::parse(&"a/".repeat(MAX_DEPTH + 1)).is_err());
        assert!(RemotePath::parse(&"ab/".repeat(MAX_PATH_BYTES)).is_err());
        assert!(RemotePath::root().join("..").is_err());
        assert!(RemotePath::root().join("a/b").is_err());
    }

    #[test]
    fn parent_join_and_prefix() {
        let p = RemotePath::parse("a/b/c").unwrap();
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(RemotePath::parse("a").unwrap().parent().unwrap(), RemotePath::root());
        assert_eq!(RemotePath::root().parent(), None);
        assert_eq!(p.parent().unwrap().join("c").unwrap(), p);
        assert!(p.starts_with(&RemotePath::parse("a/b").unwrap()));
        assert!(!p.starts_with(&RemotePath::parse("a/bc").unwrap()));
        assert!(p.starts_with(&RemotePath::root()));
        assert_eq!(p.to_string(), "/a/b/c");
    }

    #[test]
    fn serde_checks_on_the_way_in() {
        let p: RemotePath = serde_json::from_str("\"a/b\"").unwrap();
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"a/b\"");
        assert!(serde_json::from_str::<RemotePath>("\"../etc/passwd\"").is_err());
    }
}
