//! One-click install of an update (fork UPD item 3, FORK-PLAN §1a.77): download the release's
//! Linux tarball, check it against the release's `SHA256SUMS`, unpack it beside the running build
//! and switch the `latest` link the launcher starts. The running build is never touched: a failed
//! or interrupted update leaves everything as it was (the half-made files are removed), and the
//! previous build stays for "Roll back".
//!
//! Builds laid out by the fork's build procedure (`<builds>/<tag>/filmcraft` with a
//! `<builds>/latest` link: the portable tarball) update themselves, and so does an AppImage
//! (FORK-PLAN §1a.107 (d): the release's AppImage for this processor replaces the running file,
//! which is kept as `<file>.previous` for "Roll back"); anything else (Flatpak, .deb / .rpm /
//! pacman packages, the Windows installer or zip, a build run from `target/`) only shows the
//! prompt, saying how that kind of install is updated.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::update::{Fetch, Release, valid_tag};

/// The largest tarball accepted (a build is ~55 MB).
pub const MAX_TARBALL_BYTES: u64 = 1 << 30;
/// The largest `SHA256SUMS` accepted.
const MAX_SUMS_BYTES: u64 = 64 << 10;
/// Most entries in a tarball (a build has 4).
const MAX_ENTRIES: usize = 64;
/// Most bytes unpacked in total.
const MAX_UNPACKED_BYTES: u64 = 4 << 30;
/// The link the launcher starts.
pub const LATEST: &str = "latest";

/// Where the builds live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The folder holding every build and the `latest` link.
    pub builds: PathBuf,
    /// The running build's folder.
    pub current: PathBuf,
    /// The running AppImage (`$APPIMAGE`), when this is one: updates replace that file instead.
    pub appimage: Option<PathBuf>,
}

/// The install layout of the build at `exe`, or why it can't update itself.
pub fn layout(exe: &Path) -> Result<Layout, String> {
    let (appimage, appdir) = (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR"));
    let home = std::env::var_os("HOME");
    layout_in(
        exe,
        std::env::var_os("FLATPAK_ID").is_some(),
        appimage.as_deref().map(Path::new),
        appdir.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
}

/// [`layout`] with the environment passed in: `flatpak` = running in a Flatpak, `appimage` /
/// `appdir` = the `$APPIMAGE` / `$APPDIR` the AppImage runtime sets, `home` = `$HOME`.
pub fn layout_in(exe: &Path, flatpak: bool, appimage: Option<&Path>, appdir: Option<&Path>, home: Option<&Path>) -> Result<Layout, String> {
    if flatpak {
        return Err("this copy of Revolution is a Flatpak: install the newer .flatpak from the release page (flatpak install --user <file>)".into());
    }
    // `$APPIMAGE` is inherited by every program an AppImage starts (a terminal or editor run as an
    // AppImage): only trust it when this program runs from inside that AppImage's mount (`$APPDIR`).
    let ours = |_: &&Path| {
        let (Some(dir), Ok(exe)) = (appdir.filter(|d| !d.as_os_str().is_empty()), std::fs::canonicalize(exe)) else { return false };
        std::fs::canonicalize(dir).is_ok_and(|d| exe.starts_with(d))
    };
    if let Some(app) = appimage.filter(|p| !p.as_os_str().is_empty()).filter(ours) {
        let app = std::fs::canonicalize(app).map_err(|e| format!("can't find this AppImage: {e}"))?;
        let dir = app.parent().ok_or("can't find this AppImage's folder")?.to_path_buf();
        if !app.is_file() {
            return Err("can't find this AppImage".into());
        }
        return Ok(Layout { builds: dir.clone(), current: dir, appimage: Some(app) });
    }
    let exe = std::fs::canonicalize(exe).map_err(|e| format!("can't find this build: {e}"))?;
    if let Some(why) = packaged_install(&exe, home) {
        return Err(why.into());
    }
    let current = exe.parent().ok_or("can't find this build's folder")?.to_path_buf();
    let builds = current.parent().ok_or("can't find the builds folder")?.to_path_buf();
    let latest = builds.join(LATEST);
    let is_link = std::fs::symlink_metadata(&latest).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    if !is_link {
        return Err("this build wasn't installed by the build procedure (no `latest` link beside it): download the update from the release page".into());
    }
    Ok(Layout { builds, current, appimage: None })
}

/// Why a build installed by a package or installer can't replace itself, with how to update it
/// instead (`None`: not one of those).
fn packaged_install(exe: &Path, home: Option<&Path>) -> Option<&'static str> {
    let dir = exe.parent()?;
    if cfg!(windows) {
        return Some(if dir.join(crate::autosave::PORTABLE_MARKER).is_file() {
            "this is the portable Windows folder: download the newer .zip from the release page and extract it in its place"
        } else {
            "Revolution was installed with the Windows installer: run the newer .msi from the release page (it replaces this version)"
        });
    }
    let user_bin = home.map(|h| h.join(".local/bin"));
    if dir == Path::new("/usr/bin") || dir == Path::new("/usr/local/bin") || dir.starts_with("/opt") || user_bin.as_deref() == Some(dir) {
        return Some("Revolution was installed by your package manager: install the newer .deb or .rpm from the release page (or rebuild the PKGBUILD)");
    }
    None
}

/// Where an AppImage update keeps the file it replaced (`<file>.previous`).
pub fn previous_appimage(app: &Path) -> PathBuf {
    let mut name = app.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".previous");
    app.with_file_name(name)
}

/// Removes `.<tag>.AppImage.part` downloads an interrupted AppImage update left in `dir`.
fn remove_appimage_leftovers(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.take(10_000).flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(tag) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".AppImage.part")) else { continue };
        if valid_tag(tag) && std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Downloads and checks the release's AppImage, then puts it in place of `app` (the old one
/// becomes [`previous_appimage`]); returns `app`. A failure leaves the running AppImage as it was.
fn install_appimage(fetch: &dyn Fetch, rel: &Release, app: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<PathBuf, String> {
    let file = rel.appimage.as_ref().ok_or("this release has no AppImage for this computer")?;
    let sums = rel.sums.as_ref().ok_or("this release has no SHA256SUMS file, so the download can't be checked")?;
    if file.size > MAX_TARBALL_BYTES {
        return Err("the release's AppImage is too large".into());
    }
    let sums = fetch.get(&sums.url, MAX_SUMS_BYTES)?;
    let want = parse_sums(&String::from_utf8_lossy(&sums), &file.name).ok_or("SHA256SUMS doesn't list the AppImage")?;
    let dir = app.parent().ok_or("can't find this AppImage's folder")?;
    let part = dir.join(format!(".{}.AppImage.part", rel.tag));
    remove_appimage_leftovers(dir);
    let r = (|| {
        let got = fetch.download(&file.url, &part, file.size, progress)?;
        if got != file.size {
            return Err(format!("the download is {got} bytes, the release says {}", file.size));
        }
        let (sha, _) = crate::package::sha256_file(&part).map_err(|e| format!("checking the download: {e}"))?;
        if sha != want {
            return Err("the download doesn't match its checksum (SHA256SUMS): nothing was changed".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("making the AppImage executable: {e}"))?;
        }
        let prev = previous_appimage(app);
        std::fs::rename(app, &prev).map_err(|e| format!("keeping the old AppImage: {e}"))?;
        if let Err(e) = std::fs::rename(&part, app) {
            if let Err(back) = std::fs::rename(&prev, app) {
                return Err(format!("putting the new AppImage in place: {e}; the old one is now {} ({back}): rename it back", prev.display()));
            }
            return Err(format!("putting the new AppImage in place: {e}"));
        }
        Ok(app.to_path_buf())
    })();
    let _ = std::fs::remove_file(&part);
    r
}

/// The SHA-256 (lower-case hex) the `SHA256SUMS` text gives for `name` (`<hex>  <name>` or
/// `<hex> *<name>` lines).
pub fn parse_sums(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (hex, rest) = l.trim_end().split_once(char::is_whitespace)?;
        let file = rest.trim_start().trim_start_matches('*');
        (file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
    })
}

/// The program's file name in a build folder: `revolution` since the fork rebrand (FORK-PLAN
/// §1a.103), `filmcraft` in builds made before it.
pub const PROGRAMS: [&str; 2] = ["revolution", "filmcraft"];

/// The program in build folder `dir`, if it has one.
pub fn program_in(dir: &Path) -> Option<std::path::PathBuf> {
    PROGRAMS.iter().map(|p| dir.join(p)).find(|p| p.is_file())
}

/// Unpacks the gzip'd tar at `src` into `dest` (which must not exist yet). Only regular files and
/// folders under the single top folder `top` are accepted, with plain relative names: links,
/// devices, absolute names, `..` and anything outside `top` refuse the whole archive. Returns
/// the unpacked files (relative to `dest`, without `top`).
pub fn unpack_tar_gz(src: &Path, dest: &Path, top: &str) -> Result<Vec<PathBuf>, String> {
    let file = std::fs::File::open(src).map_err(|e| format!("opening the download: {e}"))?;
    let mut gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file)).take(MAX_UNPACKED_BYTES.saturating_add(1 << 20));
    std::fs::create_dir(dest).map_err(|e| format!("making {}: {e}", dest.display()))?;
    let r = unpack_tar(&mut gz, dest, top);
    if r.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    r
}

fn unpack_tar(r: &mut dyn Read, dest: &Path, top: &str) -> Result<Vec<PathBuf>, String> {
    let bad = |m: &str| format!("the download isn't a Revolution build: {m}");
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut long_name: Option<String> = None;
    for _ in 0..MAX_ENTRIES.saturating_mul(3) {
        let mut h = [0u8; 512];
        read_exact(r, &mut h).map_err(|_| bad("it ends early"))?;
        if h.iter().all(|&b| b == 0) {
            return if files.is_empty() { Err(bad("it holds no files")) } else { Ok(files) };
        }
        if !checksum_ok(&h) {
            return Err(bad("a header is damaged"));
        }
        let size = octal(h.get(124..136).unwrap_or(&[])).ok_or_else(|| bad("a size is damaged"))?;
        let kind = h.get(156).copied().unwrap_or(0);
        let padded = size.div_ceil(512).saturating_mul(512);
        match kind {
            // GNU long name: the next entry's name
            b'L' => {
                if size > 4096 {
                    return Err(bad("a name is too long"));
                }
                let mut buf = vec![0u8; usize::try_from(padded).map_err(|_| bad("a name is too long"))?];
                read_exact(r, &mut buf).map_err(|_| bad("it ends early"))?;
                buf.truncate(usize::try_from(size).unwrap_or(0));
                let n = String::from_utf8(buf).map_err(|_| bad("a name isn't text"))?;
                long_name = Some(n.trim_end_matches('\0').to_string());
                continue;
            }
            // pax headers: skipped (names stay the plain ones)
            b'x' | b'g' => {
                skip(r, padded).map_err(|_| bad("it ends early"))?;
                continue;
            }
            b'0' | 0 | b'5' => {}
            _ => return Err(bad("it holds a link or a special file")),
        }
        let name = match long_name.take() {
            Some(n) => n,
            None => header_name(&h).ok_or_else(|| bad("a name isn't text"))?,
        };
        let rel = safe_rel(&name, top).ok_or_else(|| bad(&format!("`{name}` is outside its folder")))?;
        if kind == b'5' {
            if !rel.as_os_str().is_empty() {
                std::fs::create_dir_all(dest.join(&rel)).map_err(|e| e.to_string())?;
            }
            skip(r, padded).map_err(|_| bad("it ends early"))?;
            continue;
        }
        if rel.as_os_str().is_empty() || files.len() >= MAX_ENTRIES {
            return Err(bad("too many files"));
        }
        total = total.saturating_add(size);
        if total > MAX_UNPACKED_BYTES {
            return Err(bad("it is too large"));
        }
        let path = dest.join(&rel);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("writing {}: {e}", rel.display()))?;
        let copied = std::io::copy(&mut (&mut *r).take(size), &mut out).map_err(|e| format!("unpacking {}: {e}", rel.display()))?;
        if copied != size {
            return Err(bad("it ends early"));
        }
        out.flush().and_then(|()| out.sync_all()).map_err(|e| e.to_string())?;
        skip(r, padded.saturating_sub(size)).map_err(|_| bad("it ends early"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = octal(h.get(100..108).unwrap_or(&[])).unwrap_or(0o644);
            let mode = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        }
        files.push(rel);
    }
    Err(bad("too many entries"))
}

fn read_exact(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<()> {
    r.read_exact(buf)
}

fn skip(r: &mut dyn Read, n: u64) -> std::io::Result<()> {
    let got = std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    if got == n { Ok(()) } else { Err(std::io::ErrorKind::UnexpectedEof.into()) }
}

/// A tar number field: octal digits, NUL / space padded.
fn octal(f: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(f).ok()?.trim_matches(|c| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn checksum_ok(h: &[u8; 512]) -> bool {
    let Some(want) = octal(h.get(148..156).unwrap_or(&[])) else { return false };
    let sum: u64 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { u64::from(b) }).sum();
    sum == want
}

fn header_name(h: &[u8; 512]) -> Option<String> {
    let field = |r: std::ops::Range<usize>| {
        let f = h.get(r).unwrap_or(&[]);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        std::str::from_utf8(f.get(..end).unwrap_or(&[])).ok().map(str::to_string)
    };
    let name = field(0..100)?;
    // POSIX ustar only: GNU tar's "ustar  " header keeps times where POSIX keeps the prefix
    let ustar = h.get(257..263) == Some(b"ustar\0");
    let prefix = if ustar { field(345..500)? } else { String::new() };
    Some(if prefix.is_empty() { name } else { format!("{prefix}/{name}") })
}

/// `top/sub/file` → `sub/file`; `None` for anything else (absolute, `..`, another top folder).
fn safe_rel(name: &str, top: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut comps = p.components();
    match comps.next()? {
        Component::Normal(c) if c == top => {}
        _ => return None,
    }
    let mut out = PathBuf::new();
    for c in comps {
        match c {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Points `<builds>/latest` at `to` in one step (a new link renamed over the old one).
#[cfg(unix)]
pub fn switch_latest(builds: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = builds.join(".latest.new");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(to, &tmp)?;
    std::fs::rename(&tmp, builds.join(LATEST)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(not(unix))]
pub fn switch_latest(_builds: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("updating in place needs Linux"))
}

/// The folder a release unpacks into: `<builds>/<tag without the channel prefix>`, or with
/// `-release` added when a build of that name is already there.
pub fn target_dir(builds: &Path, tag: &str, prefix: &str) -> PathBuf {
    let plain = tag.strip_prefix(prefix).unwrap_or(tag);
    let first = builds.join(plain);
    if std::fs::symlink_metadata(&first).is_err() { first } else { builds.join(format!("{plain}-release")) }
}

/// Removes what an interrupted update left in the builds folder (`.<tag>.part` downloads and
/// `.<tag>.unpack` folders); nothing else is touched.
pub fn remove_leftovers(builds: &Path) {
    let Ok(rd) = std::fs::read_dir(builds) else { return };
    for e in rd.take(10_000).flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(tag) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".part").or_else(|| n.strip_suffix(".unpack"))) else { continue };
        if !valid_tag(tag) {
            continue;
        }
        let p = e.path();
        let is_dir = std::fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
        let _ = if is_dir { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

/// Downloads, checks and unpacks `rel` beside the running build and switches `latest` to it.
/// Returns the new build's folder. On any failure nothing changes (half-made files are removed).
pub fn install(fetch: &dyn Fetch, rel: &Release, prefix: &str, layout: &Layout, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<PathBuf, String> {
    if !valid_tag(&rel.tag) {
        return Err("the release has a strange name".into());
    }
    if let Some(app) = &layout.appimage {
        return install_appimage(fetch, rel, app, progress);
    }
    let tarball = rel.tarball.as_ref().ok_or("this release has no Linux build for this computer")?;
    let sums = rel.sums.as_ref().ok_or("this release has no SHA256SUMS file, so the download can't be checked")?;
    if tarball.size > MAX_TARBALL_BYTES {
        return Err("the release's Linux build is too large".into());
    }
    let sums = fetch.get(&sums.url, MAX_SUMS_BYTES)?;
    let want = parse_sums(&String::from_utf8_lossy(&sums), &tarball.name).ok_or("SHA256SUMS doesn't list the Linux build")?;
    let part = layout.builds.join(format!(".{}.part", rel.tag));
    let unpack = layout.builds.join(format!(".{}.unpack", rel.tag));
    remove_leftovers(&layout.builds);
    let r = (|| {
        let got = fetch.download(&tarball.url, &part, tarball.size, progress)?;
        if got != tarball.size {
            return Err(format!("the download is {got} bytes, the release says {}", tarball.size));
        }
        let (sha, _) = crate::package::sha256_file(&part).map_err(|e| format!("checking the download: {e}"))?;
        if sha != want {
            return Err("the download doesn't match its checksum (SHA256SUMS): nothing was changed".into());
        }
        let suffix = format!("-linux-{}.tar.gz", crate::update::linux_arch());
        let top = tarball.name.strip_suffix(suffix.as_str()).unwrap_or(&tarball.name).to_string();
        let files = unpack_tar_gz(&part, &unpack, &top)?;
        if !files.iter().any(|f| PROGRAMS.iter().any(|p| f == Path::new(p))) {
            return Err("the download has no `revolution` program".into());
        }
        let dest = target_dir(&layout.builds, &rel.tag, prefix);
        std::fs::rename(&unpack, &dest).map_err(|e| format!("moving the new build into place: {e}"))?;
        if let Err(e) = switch_latest(&layout.builds, &dest) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(format!("switching to the new build: {e}"));
        }
        Ok(dest)
    })();
    let _ = std::fs::remove_file(&part);
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&unpack);
    }
    r
}

#[cfg(test)]
#[path = "update_install_tests.rs"]
mod tests;
