//! Project packages (STORAGE-PLAN S1 item 5): one folder (or `.revpkg` file; older `.fcpackage` files still open) holding a project
//! and everything it needs, with a manifest so the receiver can check nothing is missing or
//! damaged.
//!
//! This module is the manifest: what a package lists ([`Manifest`]), how a file's checksum is
//! taken ([`sha256_file`]), and how a received package is checked ([`verify`]). Entry paths are
//! relative, `/`-separated and must stay inside the package: an absolute path or a `..` is
//! refused, so a hostile manifest can't point the import at files elsewhere on the disk.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest's file name at the root of a package.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The `format` value of a FilmCraft package manifest.
pub const FORMAT: &str = "filmcraft-package";

/// The manifest version this build writes and reads.
pub const VERSION: u32 = 1;

/// The largest manifest read (a manifest lists paths and hashes: a few hundred bytes a file).
pub const MAX_MANIFEST_BYTES: u64 = 64 << 20;

/// The most entries a manifest may list.
pub const MAX_ENTRIES: usize = 1_000_000;

/// What a packaged file is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Project,
    Media,
    Still,
    VoiceOver,
    Font,
    Shader,
    Captions,
    Proxy,
    Other,
}

/// One packaged file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Relative to the package root, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Lower-case hex SHA-256 of the file's bytes.
    pub sha256: String,
    pub role: Role,
}

/// Something deliberately not packaged, and why (the render cache, downloaded models, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftOut {
    pub what: String,
    pub reason: String,
}

/// A package's `manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: String,
    /// [`VERSION`] when written by this build.
    pub version: u32,
    /// The FilmCraft version that made it.
    pub filmcraft: String,
    /// The project file's entry path.
    pub project: String,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub left_out: Vec<LeftOut>,
}

impl Manifest {
    pub fn new(project: &str) -> Self {
        Self {
            format: FORMAT.into(),
            version: VERSION,
            filmcraft: env!("CARGO_PKG_VERSION").into(),
            project: project.into(),
            entries: Vec::new(),
            left_out: Vec::new(),
        }
    }

    /// Reads and checks a manifest's text: the right format, a version this build reads, a
    /// bounded entry count, and every path safe. A newer version is an error that says so.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("the package manifest is too large".into());
        }
        let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("the package manifest can't be read: {e}"))?;
        if m.format != FORMAT {
            return Err("this isn't a Revolution package (unknown manifest format)".into());
        }
        if m.version > VERSION {
            return Err(format!(
                "this package was made by a newer Revolution ({}, package version {}); update Revolution to open it",
                shown(&m.filmcraft),
                m.version
            ));
        }
        if m.entries.len() > MAX_ENTRIES {
            return Err("the package manifest lists too many files".into());
        }
        for e in &m.entries {
            if safe_relative(&e.path).is_none() {
                return Err(format!("the package manifest names a file outside the package: {}", shown(&e.path)));
            }
            if !is_hex_sha256(&e.sha256) {
                return Err(format!("the package manifest has a damaged checksum for {}", shown(&e.path)));
            }
        }
        let mut seen = std::collections::HashSet::with_capacity(m.entries.len());
        for e in &m.entries {
            if !seen.insert(e.path.as_str()) {
                return Err(format!("the package manifest lists {} twice", shown(&e.path)));
            }
        }
        if m.project.contains('/') {
            return Err("the package's project file must be at the top of the package".into());
        }
        if !m.entries.iter().any(|e| e.path == m.project && e.role == Role::Project) {
            return Err("the package manifest doesn't list its project file".into());
        }
        Ok(m)
    }

    /// Pretty JSON for `manifest.json`.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }
}

/// `path` as a relative path inside a package, or `None` when it is empty, absolute, or climbs
/// out with `..`.
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains('\\') || path.contains('\0') {
        return None;
    }
    let p = Path::new(path);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// A string from a manifest, cut for an error message.
fn shown(s: &str) -> String {
    let mut out: String = s.chars().take(200).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// SHA-256 of a file (streamed, 1 MB at a time) and its size.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    sha256_file_until(path, &|| false)
}

/// [`sha256_file`], giving up with an error as soon as `cancel` says so.
pub fn sha256_file_until(path: &Path, cancel: &dyn Fn() -> bool) -> std::io::Result<(String, u64)> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        if cancel() {
            return Err(std::io::Error::other("stopped"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n).unwrap_or_default());
        size = size.saturating_add(n as u64);
    }
    let hex = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((hex, size))
}

/// What [`verify`] found. A package is complete when every list is empty (`extra` files don't
/// make it incomplete; they are reported so a receiver knows about them).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub missing: Vec<String>,
    pub wrong_size: Vec<String>,
    pub damaged: Vec<String>,
    pub extra: Vec<String>,
    pub checked: usize,
}

impl Report {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty() && self.wrong_size.is_empty() && self.damaged.is_empty()
    }
}

/// The most files and folder depth looked at when listing a package for extra files.
const MAX_WALK_FILES: usize = 1_000_000;
const MAX_WALK_DEPTH: usize = 32;

/// Checks a package folder against its manifest: every listed file is there with its size and
/// checksum; files the manifest doesn't list (other than the manifest) are reported as extra.
/// `cancel` is polled between files.
pub fn verify(root: &Path, m: &Manifest, cancel: &dyn Fn() -> bool) -> Result<Report, String> {
    let mut r = Report::default();
    let mut listed = std::collections::BTreeSet::new();
    for e in &m.entries {
        if cancel() {
            return Err("stopped".into());
        }
        let Some(rel) = safe_relative(&e.path) else {
            r.damaged.push(e.path.clone());
            continue;
        };
        listed.insert(rel.clone());
        let full = root.join(&rel);
        // links are not followed: a package never needs them, and they could point anywhere
        let linked = rel.ancestors().any(|a| !a.as_os_str().is_empty() && std::fs::symlink_metadata(root.join(a)).is_ok_and(|m| m.file_type().is_symlink()));
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() && !linked => {
                if meta.len() != e.size {
                    r.wrong_size.push(e.path.clone());
                    continue;
                }
                match sha256_file_until(&full, cancel) {
                    Ok((hex, _)) if hex == e.sha256 => {}
                    Err(_) if cancel() => return Err("stopped".into()),
                    _ => r.damaged.push(e.path.clone()),
                }
            }
            _ => r.missing.push(e.path.clone()),
        }
        r.checked = r.checked.saturating_add(1);
    }
    let mut found = Vec::new();
    walk(root, root, 0, &mut found);
    for rel in found {
        if rel != Path::new(MANIFEST_FILE) && !listed.contains(&rel) {
            r.extra.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(r)
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_WALK_FILES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= MAX_WALK_FILES {
            return;
        }
        let p = e.path();
        // symlinks are not followed (a package never needs them; they could loop)
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(root, &p, depth.saturating_add(1), out);
        } else if ft.is_file()
            && let Ok(rel) = p.strip_prefix(root)
        {
            out.push(rel.to_path_buf());
        }
    }
}

/// The file extension of a one-file package (FORK-PLAN §1a.104: Revolution packages).
pub const EXT: &str = "revpkg";

/// The extension packages had before 0.1.3; still opened.
pub const LEGACY_EXT: &str = "fcpackage";

/// Whether `path` names a one-file package (`.revpkg`, or an older `.fcpackage`), any case.
pub fn is_package_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXT) || e.eq_ignore_ascii_case(LEGACY_EXT))
}

/// Writes the package folder `dir` as one `.revpkg` file: a zip with every file **stored**
/// (camera media doesn't compress, and stored entries can be read back without inflating), the
/// manifest first. Written to `out.part` and renamed; a failure removes it.
pub fn zip_folder(dir: &Path, out: &Path, cancel: &dyn Fn() -> bool) -> Result<u64, String> {
    let part = out.with_extension(format!("{EXT}.part"));
    let r = (|| -> Result<u64, String> {
        let file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut z = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
        let mut files = Vec::new();
        walk(dir, dir, 0, &mut files);
        files.sort_by_key(|f| (f != Path::new(MANIFEST_FILE), f.clone()));
        let mut bytes = 0u64;
        for rel in files {
            if cancel() {
                return Err("stopped".into());
            }
            let name = rel.to_string_lossy().replace('\\', "/");
            z.start_file(name.as_str(), opts).map_err(|e| format!("{name}: {e}"))?;
            let mut f = std::fs::File::open(dir.join(&rel)).map_err(|e| format!("{name}: {e}"))?;
            bytes = bytes.saturating_add(std::io::copy(&mut f, &mut z).map_err(|e| format!("{name}: {e}"))?);
        }
        let w = z.finish().map_err(|e| format!("{}: {e}", part.display()))?;
        let file = w.into_inner().map_err(|e| format!("{}: {e}", part.display()))?;
        file.sync_all().map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(bytes)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// The manifest inside a `.revpkg` / `.fcpackage` file (read and checked; nothing else is read).
pub fn read_zip_manifest(file: &Path) -> Result<Manifest, String> {
    let f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("not a Revolution package: {e}"))?;
    let entry = z.by_name(MANIFEST_FILE).map_err(|_| "not a Revolution package (no manifest.json)".to_string())?;
    let mut bytes = Vec::new();
    entry.take(MAX_MANIFEST_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| format!("the package manifest can't be read: {e}"))?;
    Manifest::parse(&bytes)
}

/// Unpacks a `.revpkg` / `.fcpackage` into `to`: only the manifest and the files it lists come out (each at
/// most its listed size plus one byte, so [`verify`] then sees any difference), at safe paths
/// inside `to`. Check the result with [`verify`].
pub fn unzip_package(file: &Path, to: &Path, m: &Manifest, cancel: &dyn Fn() -> bool) -> Result<u64, String> {
    let f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("not a Revolution package: {e}"))?;
    if z.len() > MAX_ENTRIES.saturating_add(1) {
        return Err("the package has too many files".into());
    }
    let sizes: std::collections::HashMap<&str, u64> = m.entries.iter().map(|e| (e.path.as_str(), e.size)).collect();
    let mut bytes = 0u64;
    for i in 0..z.len() {
        if cancel() {
            return Err("stopped".into());
        }
        let mut e = z.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
        if e.is_dir() {
            continue;
        }
        let name = e.name().to_string();
        let limit = if name == MANIFEST_FILE {
            MAX_MANIFEST_BYTES
        } else if let Some(size) = sizes.get(name.as_str()) {
            *size
        } else {
            continue;
        };
        let Some(rel) = safe_relative(&name) else { continue };
        let out = to.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let n = std::io::copy(&mut (&mut e).take(limit.saturating_add(1)), &mut w).map_err(|e| format!("{name}: {e}"))?;
        bytes = bytes.saturating_add(n);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fc-package-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn entry(root: &Path, rel: &str, bytes: &[u8], role: Role) -> Entry {
        let full = root.join(rel);
        if let Some(p) = full.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(&full, bytes);
        let (sha256, size) = sha256_file(&full).unwrap_or_default();
        Entry { path: rel.into(), size, sha256, role }
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        let d = tmp("vectors");
        let f = d.join("abc");
        let _ = std::fs::write(&f, b"abc");
        let (hex, size) = sha256_file(&f).unwrap_or_default();
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(size, 3);
        let _ = std::fs::write(&f, b"");
        assert_eq!(sha256_file(&f).unwrap_or_default().0, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn round_trip_and_verify_a_complete_package() {
        let d = tmp("ok");
        let mut m = Manifest::new("Edit.fcproj");
        m.entries.push(entry(&d, "Edit.fcproj", b"{}", Role::Project));
        m.entries.push(entry(&d, "media/a b.mov", &[7u8; 5000], Role::Media));
        m.left_out.push(LeftOut { what: "render cache".into(), reason: "rebuilt on the receiving computer".into() });
        let back = Manifest::parse(&m.to_json());
        assert_eq!(back.as_ref().ok(), Some(&m));
        let r = verify(&d, &m, &|| false).unwrap_or_default();
        assert!(r.is_complete(), "{r:?}");
        assert_eq!(r.checked, 2);
        assert!(r.extra.is_empty(), "{r:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn verify_reports_missing_resized_damaged_and_extra_files() {
        let d = tmp("bad");
        let mut m = Manifest::new("p.fcproj");
        m.entries.push(entry(&d, "p.fcproj", b"{}", Role::Project));
        m.entries.push(entry(&d, "media/gone.mov", b"x", Role::Media));
        m.entries.push(entry(&d, "media/short.mov", b"12345", Role::Media));
        m.entries.push(entry(&d, "media/flip.mov", b"abcde", Role::Media));
        let _ = std::fs::remove_file(d.join("media/gone.mov"));
        let _ = std::fs::write(d.join("media/short.mov"), b"123");
        let _ = std::fs::write(d.join("media/flip.mov"), b"abcdf");
        let _ = std::fs::write(d.join("media/stray.txt"), b"?");
        let _ = std::fs::write(d.join(MANIFEST_FILE), m.to_json());
        let r = verify(&d, &m, &|| false).unwrap_or_default();
        assert_eq!(r.missing, vec!["media/gone.mov"]);
        assert_eq!(r.wrong_size, vec!["media/short.mov"]);
        assert_eq!(r.damaged, vec!["media/flip.mov"]);
        assert_eq!(r.extra, vec!["media/stray.txt"]);
        assert!(!r.is_complete());
        assert!(verify(&d, &m, &|| true).is_err(), "stopped");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn hostile_manifests_are_refused() {
        let good = "a".repeat(64);
        let make = |path: &str, sha: &str| {
            format!(
                r#"{{"format":"filmcraft-package","version":1,"filmcraft":"x","project":"{path}","entries":[{{"path":"{path}","size":1,"sha256":"{sha}","role":"project"}}]}}"#
            )
        };
        assert!(Manifest::parse(make("p.fcproj", &good).as_bytes()).is_ok());
        for bad in ["../p.fcproj", "/etc/passwd", "a/../../p", "", "a\\\\..\\\\p"] {
            assert!(Manifest::parse(make(bad, &good).as_bytes()).is_err(), "{bad}");
        }
        assert!(Manifest::parse(make("p.fcproj", "zz").as_bytes()).is_err());
        assert!(Manifest::parse(make("p.fcproj", &"A".repeat(64)).as_bytes()).is_err());
        assert!(Manifest::parse(b"").is_err());
        assert!(Manifest::parse(b"[1,2").is_err());
        assert!(Manifest::parse(br#"{"format":"other","version":1,"filmcraft":"x","project":"p","entries":[]}"#).is_err());
        assert!(Manifest::parse(br#"{"format":"filmcraft-package","version":1,"filmcraft":"x","project":"p","entries":[]}"#).is_err(), "no project entry");
        let newer = make("p.fcproj", &good).replace("\"version\":1", "\"version\":99");
        let e = Manifest::parse(newer.as_bytes()).err().unwrap_or_default();
        assert!(e.contains("newer Revolution"), "{e}");
    }

    #[test]
    fn safe_relative_paths() {
        assert_eq!(safe_relative("media/a.mov"), Some(PathBuf::from("media/a.mov")));
        assert_eq!(safe_relative("./media/a.mov"), Some(PathBuf::from("media/a.mov")));
        for bad in ["", ".", "..", "../x", "/x", "a/../../x", "a\\b", "a\0b"] {
            assert_eq!(safe_relative(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn duplicates_nested_projects_and_long_strings_are_refused() {
        let h = "a".repeat(64);
        let dup = format!(
            r#"{{"format":"filmcraft-package","version":1,"filmcraft":"x","project":"p.fcproj","entries":[{{"path":"p.fcproj","size":1,"sha256":"{h}","role":"project"}},{{"path":"m.mov","size":1,"sha256":"{h}","role":"media"}},{{"path":"m.mov","size":1,"sha256":"{h}","role":"media"}}]}}"#
        );
        assert!(Manifest::parse(dup.as_bytes()).err().unwrap_or_default().contains("twice"));
        let nested = format!(
            r#"{{"format":"filmcraft-package","version":1,"filmcraft":"x","project":"sub/p.fcproj","entries":[{{"path":"sub/p.fcproj","size":1,"sha256":"{h}","role":"project"}}]}}"#
        );
        assert!(Manifest::parse(nested.as_bytes()).is_err());
        let long = format!(r#"{{"format":"filmcraft-package","version":9,"filmcraft":"{}","project":"p","entries":[]}}"#, "v".repeat(100_000));
        assert!(Manifest::parse(long.as_bytes()).err().unwrap_or_default().len() < 1000);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_entries_count_as_missing() {
        let d = tmp("links");
        let outside = tmp("links-outside");
        let mut m = Manifest::new("p.fcproj");
        m.entries.push(entry(&d, "p.fcproj", b"{}", Role::Project));
        let real = entry(&outside, "real.mov", b"payload", Role::Media);
        let _ = std::os::unix::fs::symlink(outside.join("real.mov"), d.join("link.mov"));
        m.entries.push(Entry { path: "link.mov".into(), ..real.clone() });
        let _ = std::os::unix::fs::symlink(&outside, d.join("dir"));
        m.entries.push(Entry { path: "dir/real.mov".into(), ..real });
        let r = verify(&d, &m, &|| false).unwrap_or_default();
        assert_eq!(r.missing, vec!["link.mov", "dir/real.mov"], "{r:?}");
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn a_folder_zips_into_one_file_and_unpacks_checked() {
        let d = tmp("zip");
        let mut m = Manifest::new("p.fcproj");
        m.entries.push(entry(&d, "p.fcproj", b"{}", Role::Project));
        m.entries.push(entry(&d, "media/a.mov", &[9u8; 3000], Role::Media));
        let _ = std::fs::write(d.join(MANIFEST_FILE), m.to_json());
        let out = tmp("zip-out");
        let _ = std::fs::create_dir_all(&out);
        let file = out.join("p.revpkg");
        assert!(zip_folder(&d, &file, &|| false).is_ok());
        assert!(!out.join("p.revpkg.part").exists());
        assert_eq!(read_zip_manifest(&file).ok().as_ref(), Some(&m));
        let to = out.join("unpacked");
        assert!(unzip_package(&file, &to, &m, &|| false).is_ok());
        let r = verify(&to, &m, &|| false).unwrap_or_default();
        assert!(r.is_complete() && r.extra.is_empty(), "{r:?}");
        // not a zip / no manifest
        let _ = std::fs::write(out.join("junk.revpkg"), b"nope");
        assert!(read_zip_manifest(&out.join("junk.revpkg")).is_err());
        assert!(is_package_file(Path::new("/a/B.REVPKG")) && is_package_file(Path::new("old.fcpackage")) && !is_package_file(Path::new("x.zip")));
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(out);
    }

    #[test]
    fn unpacking_skips_unlisted_and_unsafe_entries() {
        use std::io::Write;
        let out = tmp("zip-evil");
        let _ = std::fs::create_dir_all(&out);
        let file = out.join("evil.revpkg");
        let mut m = Manifest::new("p.fcproj");
        m.entries.push(Entry { path: "p.fcproj".into(), size: 2, sha256: "0".repeat(64), role: Role::Project });
        if let Ok(f) = std::fs::File::create(&file) {
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, body) in [(MANIFEST_FILE, m.to_json()), ("p.fcproj", b"{}".to_vec()), ("../escape.txt", b"x".to_vec()), ("extra.bin", b"y".to_vec())] {
                let _ = z.start_file(name, o);
                let _ = z.write_all(&body);
            }
            let _ = z.finish();
        }
        let to = out.join("to");
        assert!(unzip_package(&file, &to, &m, &|| false).is_ok());
        assert!(to.join("p.fcproj").is_file());
        assert!(!out.join("escape.txt").exists() && !to.join("extra.bin").exists());
        let _ = std::fs::remove_dir_all(out);
    }
}
