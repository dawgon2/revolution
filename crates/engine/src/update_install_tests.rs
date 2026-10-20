use super::*;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use sha2::Digest;

use crate::update::{Asset, Channel, Current, Updater};

const CH: Channel = Channel { repo: "dawgon2/filmcraft-cachyos", tag_prefix: "fork-" };
const URL: &str = "https://github.com/dawgon2/filmcraft-cachyos/releases/download/fork-next-1/";

/// One tar entry: (name, kind, mode, contents).
fn tar(entries: &[(&str, u8, u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, kind, mode, data) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[156] = *kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn gz(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

fn good_tarball() -> Vec<u8> {
    gz(&tar(&[
        ("filmcraft-fork-next-1/", b'5', 0o755, b""),
        ("filmcraft-fork-next-1/filmcraft", b'0', 0o755, b"#!/bin/sh\necho new\n"),
        ("filmcraft-fork-next-1/filmcraft-cli", b'0', 0o755, b"cli"),
        ("filmcraft-fork-next-1/NOTES.md", b'0', 0o644, b"# next-1\n"),
    ]))
}

fn sha(b: &[u8]) -> String {
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn release(tarball: &[u8]) -> Release {
    Release {
        tag: "fork-next-1".into(),
        name: "next".into(),
        notes: String::new(),
        published: 2_000_000_000,
        tarball: Some(Asset {
            name: "filmcraft-fork-next-1-linux-x86_64.tar.gz".into(),
            url: format!("{URL}filmcraft-fork-next-1-linux-x86_64.tar.gz"),
            size: tarball.len() as u64,
        }),
        appimage: None,
        sums: Some(Asset { name: "SHA256SUMS".into(), url: format!("{URL}SHA256SUMS"), size: 100 }),
    }
}

/// A mock release server: url → body; `cut` stops a download after that many bytes (an
/// interrupted connection).
struct Server {
    files: HashMap<String, Vec<u8>>,
    cut: Option<usize>,
}

impl Server {
    fn new(tarball: &[u8], sums_for: &[u8]) -> Self {
        let sums = format!("{}  filmcraft-fork-next-1-linux-x86_64.tar.gz\n{}  other.tar.gz\n", sha(sums_for), "0".repeat(64));
        let files =
            HashMap::from([(format!("{URL}filmcraft-fork-next-1-linux-x86_64.tar.gz"), tarball.to_vec()), (format!("{URL}SHA256SUMS"), sums.into_bytes())]);
        Self { files, cut: None }
    }
}

impl Fetch for Server {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
        let b = self.files.get(url).cloned().ok_or_else(|| format!("{url}: 404"))?;
        if b.len() as u64 > max_bytes {
            return Err("too large".into());
        }
        Ok(b)
    }

    fn download(&self, url: &str, dest: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<u64, String> {
        let b = self.get(url, max_bytes)?;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(dest).map_err(|e| e.to_string())?;
        let n = self.cut.unwrap_or(b.len()).min(b.len());
        f.write_all(&b[..n]).unwrap();
        progress(n as u64, max_bytes);
        if self.cut.is_some() {
            drop(f);
            let _ = std::fs::remove_file(dest);
            return Err("connection reset".into());
        }
        Ok(n as u64)
    }
}

/// A builds folder with the running build `storage-1` and `latest` → it.
fn builds(name: &str) -> Layout {
    let root = crate::media_test_util::tmp_dir(&format!("update-install-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let current = root.join("storage-1");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("filmcraft"), b"old").unwrap();
    std::os::unix::fs::symlink(&current, root.join(LATEST)).unwrap();
    Layout { builds: root, current, appimage: None }
}

fn latest(l: &Layout) -> PathBuf {
    std::fs::read_link(l.builds.join(LATEST)).unwrap()
}

/// Nothing but the running build and the link is left.
fn untouched(l: &Layout) {
    assert_eq!(latest(l), l.current);
    let mut names: Vec<String> = std::fs::read_dir(&l.builds).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["latest", "storage-1"]);
    assert_eq!(std::fs::read(l.current.join("filmcraft")).unwrap(), b"old");
}

#[test]
fn sums_lines_are_read_in_both_styles() {
    let h = "a".repeat(64);
    assert_eq!(parse_sums(&format!("{h}  x.tar.gz\n"), "x.tar.gz"), Some(h.clone()));
    assert_eq!(parse_sums(&format!("{}\t*x.tar.gz\r\n", h.to_uppercase()), "x.tar.gz"), Some(h.clone()));
    assert_eq!(parse_sums(&format!("{h}  y.tar.gz\n"), "x.tar.gz"), None);
    assert_eq!(parse_sums("zz  x.tar.gz", "x.tar.gz"), None);
    assert_eq!(parse_sums("", "x.tar.gz"), None);
}

#[test]
fn only_builds_with_a_latest_link_update_themselves() {
    let l = builds("layout");
    assert_eq!(
        layout(&l.current.join("filmcraft")).unwrap(),
        Layout { builds: std::fs::canonicalize(&l.builds).unwrap(), current: std::fs::canonicalize(&l.current).unwrap(), appimage: None }
    );
    std::fs::remove_file(l.builds.join(LATEST)).unwrap();
    assert!(layout(&l.current.join("filmcraft")).unwrap_err().contains("release page"));
    assert!(layout(Path::new("/nonexistent/filmcraft")).is_err());
}

/// UPD accept (mock release server): a good release unpacks beside the running build, `latest`
/// points at it, the programs stay executable, and Roll back points `latest` back.
#[test]
fn a_good_update_installs_beside_the_running_build_and_rolls_back() {
    use std::os::unix::fs::PermissionsExt;
    let l = builds("good");
    let t = good_tarball();
    let mut s = crate::Session::new(Arc::new(crate::FsServices));
    s.updater = Updater::new(Some(CH), Arc::new(Server::new(&t, &t)), Current { tag: Some("fork-storage-1".into()), built: Some(1) });
    s.updater.set_layout(Ok(l.clone()));
    s.updater.status.latest = Some(release(&t));
    s.updater.status.state = "available".into();
    let v = s.execute("app.update.install", json!({"wait": true})).unwrap();
    assert_eq!(v["install"], "installed", "{v}");
    let new = l.builds.join("next-1");
    assert_eq!(latest(&l), new);
    assert_eq!(v["installed"], new.join("filmcraft").to_string_lossy().as_ref());
    assert_eq!(std::fs::read(new.join("filmcraft")).unwrap(), b"#!/bin/sh\necho new\n");
    assert_eq!(std::fs::metadata(new.join("filmcraft")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::metadata(new.join("NOTES.md")).unwrap().permissions().mode() & 0o777, 0o644);
    assert_eq!(std::fs::read(l.current.join("filmcraft")).unwrap(), b"old", "the running build is kept");
    assert_eq!(s.prefs.update.previous, l.current.to_string_lossy());
    assert!(!l.builds.join(".fork-next-1.part").exists() && !l.builds.join(".fork-next-1.unpack").exists());
    let v = s.execute("app.update.rollback", json!({})).unwrap();
    assert_eq!(v["rolledBack"], l.current.join("filmcraft").to_string_lossy().as_ref());
    assert_eq!(latest(&l), l.current);
    // a build of the same name already there → beside it, never over it
    assert_eq!(target_dir(&l.builds, "fork-next-1", "fork-"), l.builds.join("next-1-release"));
}

/// UPD accept (mock release server): a bad checksum, an interrupted download, a truncated or
/// hostile archive, or a release without SHA256SUMS leave everything as it was.
#[test]
fn failed_updates_leave_the_running_build_untouched() {
    let t = good_tarball();
    let hostile = [
        // a link inside the archive
        gz(&tar(&[("filmcraft-fork-next-1/filmcraft", b'2', 0o777, b"")])),
        // a name climbing out of the folder
        gz(&tar(&[("filmcraft-fork-next-1/../../evil", b'0', 0o644, b"x")])),
        // another top folder
        gz(&tar(&[("elsewhere/filmcraft", b'0', 0o755, b"x")])),
        // no program
        gz(&tar(&[("filmcraft-fork-next-1/NOTES.md", b'0', 0o644, b"x")])),
        // empty archive
        gz(&tar(&[])),
        // not gzip at all
        b"PK\x03\x04 not a tarball".to_vec(),
    ];
    let mut cases: Vec<(&str, Server, Release)> = vec![
        ("bad checksum", Server::new(&t, b"something else"), release(&t)),
        ("interrupted", Server { cut: Some(t.len() / 2), ..Server::new(&t, &t) }, release(&t)),
        ("truncated", Server::new(&t[..t.len() - 40], &t[..t.len() - 40]), release(&t[..t.len() - 40])),
        ("size differs", Server::new(&t, &t), release(&t[..10])),
        ("no sums", Server::new(&t, &t), Release { sums: None, ..release(&t) }),
        ("no tarball", Server::new(&t, &t), Release { tarball: None, ..release(&t) }),
        ("bad tag", Server::new(&t, &t), Release { tag: "fork-../x".into(), ..release(&t) }),
    ];
    for h in &hostile {
        cases.push(("hostile archive", Server::new(h, h), release(h)));
    }
    for (i, (what, server, rel)) in cases.into_iter().enumerate() {
        let l = builds(&format!("fail-{i}"));
        let r = install(&server, &rel, "fork-", &l, &mut |_, _| true);
        assert!(r.is_err(), "{what}: {r:?}");
        untouched(&l);
    }
}

#[test]
fn install_needs_an_offered_release_and_a_managed_build() {
    let mut u = Updater::new(Some(CH), Arc::new(Server::new(b"", b"")), Current::default());
    assert!(u.start_install().unwrap_err().contains("check first"));
    u.status.latest = Some(release(b"x"));
    assert!(u.start_install().is_err(), "not installed by the build procedure");
    let mut s = crate::Session::new(Arc::new(crate::FsServices));
    assert!(s.execute("app.update.rollback", json!({})).is_err());
}

#[test]
fn tar_numbers_and_names_are_checked() {
    assert_eq!(octal(b"0000644\0"), Some(0o644));
    assert_eq!(octal(b"   \0"), Some(0));
    assert_eq!(octal(b"99"), None);
    assert_eq!(safe_rel("top/a/b", "top"), Some(PathBuf::from("a/b")));
    assert_eq!(safe_rel("top/./a", "top"), Some(PathBuf::from("a")));
    assert_eq!(safe_rel("top", "top"), Some(PathBuf::new()));
    for bad in ["/top/a", "top/../a", "other/a", "../top/a", ""] {
        assert_eq!(safe_rel(bad, "top"), None, "{bad}");
    }
    // a damaged header checksum refuses the archive
    let mut t = tar(&[("top/a", b'0', 0o644, b"x")]);
    t[0] = b'X';
    let dir = crate::media_test_util::tmp_dir("update-install-damaged");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(unpack_tar(&mut &t[..], &dir, "top").unwrap_err().contains("damaged"));
}

/// GNU headers (`ustar  \0`, times where POSIX keeps the prefix) and GNU long names unpack.
#[test]
fn gnu_headers_and_long_names_unpack() {
    let long = format!("top/{}/f", "d".repeat(150));
    let mut t = tar(&[("././@LongLink", b'L', 0o644, long.as_bytes()), ("top/short", b'0', 0o644, b"long"), ("top/g", b'0', 0o644, b"gnu")]);
    // turn the third header into a GNU one with times in the prefix area
    let h = 512 * 4;
    t[h + 257..h + 265].copy_from_slice(b"ustar  \0");
    t[h + 345..h + 357].copy_from_slice(b"14710000000\0");
    t[h + 148..h + 156].copy_from_slice(b"        ");
    let sum: u32 = t[h..h + 512].iter().map(|&b| u32::from(b)).sum();
    t[h + 148..h + 155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let dir = crate::media_test_util::tmp_dir("update-install-gnu").join("out");
    let src = dir.with_extension("tar.gz");
    std::fs::write(&src, gz(&t)).unwrap();
    let files = unpack_tar_gz(&src, &dir, "top").unwrap();
    assert_eq!(files, [PathBuf::from(format!("{}/f", "d".repeat(150))), PathBuf::from("g")]);
    assert_eq!(std::fs::read(dir.join("g")).unwrap(), b"gnu");
    // a duplicate name is refused (never overwrites)
    let dup = gz(&tar(&[("top/a", b'0', 0o644, b"1"), ("top/a", b'0', 0o644, b"2")]));
    let d2 = dir.with_extension("dup");
    std::fs::write(&src, dup).unwrap();
    assert!(unpack_tar_gz(&src, &d2, "top").is_err());
    assert!(!d2.exists());
}

/// An installed update isn't installed again; leftovers of an interrupted one are cleared.
#[test]
fn an_installed_update_isnt_installed_twice_and_leftovers_go() {
    let l = builds("twice");
    std::fs::write(l.builds.join(".fork-old-1.part"), b"half").unwrap();
    std::fs::create_dir(l.builds.join(".fork-old-1.unpack")).unwrap();
    std::fs::write(l.builds.join(".keep-me"), b"").unwrap();
    let t = good_tarball();
    let mut u = Updater::new(Some(CH), Arc::new(Server::new(&t, &t)), Current::default());
    u.set_layout(Ok(l.clone()));
    u.status.latest = Some(release(&t));
    u.start_install().unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while u.poll_install().is_none() && u.status.install == "installing" && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(u.status.install, "installed", "{}", u.status.message);
    assert!(u.start_install().unwrap_err().contains("restart"));
    assert!(!l.builds.join(".fork-old-1.part").exists() && !l.builds.join(".fork-old-1.unpack").exists());
    assert!(l.builds.join(".keep-me").exists());
}

/// PKG (FORK-PLAN §1a.107 (d)): an AppImage at `<dir>/Revolution.AppImage` holding "old".
fn appimage_dir(name: &str) -> (PathBuf, Layout) {
    let root = crate::media_test_util::tmp_dir(&format!("update-appimage-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let app = root.join("Revolution.AppImage");
    std::fs::write(&app, b"old").unwrap();
    let mnt = root.join("mnt");
    std::fs::create_dir_all(mnt.join("usr/bin")).unwrap();
    std::fs::write(mnt.join("usr/bin/revolution"), b"").unwrap();
    let l = layout_in(&mnt.join("usr/bin/revolution"), false, Some(&app), Some(&mnt), None).unwrap();
    // a leftover download of an interrupted update goes with the next one
    std::fs::write(root.join(".fork-old-1.AppImage.part"), b"x").unwrap();
    (std::fs::canonicalize(&app).unwrap(), l)
}

/// A release offering `body` as this processor's AppImage, and a server whose SHA256SUMS lists
/// `sums_for` for it.
fn appimage_release(body: &[u8], sums_for: &[u8]) -> (Release, Server) {
    let name = format!("fork-next-1-linux-{}.AppImage", crate::update::linux_arch());
    let rel = Release { appimage: Some(Asset { name: name.clone(), url: format!("{URL}{name}"), size: body.len() as u64 }), tarball: None, ..release(b"") };
    let sums = format!("{}  {name}\n", sha(sums_for));
    let files = HashMap::from([(format!("{URL}{name}"), body.to_vec()), (format!("{URL}SHA256SUMS"), sums.into_bytes())]);
    (rel, Server { files, cut: None })
}

#[test]
fn an_appimage_updates_in_place_keeps_the_old_one_and_rolls_back() {
    use std::os::unix::fs::PermissionsExt;
    let (app, l) = appimage_dir("good");
    assert_eq!(l.appimage.as_deref(), Some(app.as_path()));
    let (rel, server) = appimage_release(b"new appimage", b"new appimage");
    let mut s = crate::Session::new(Arc::new(crate::FsServices));
    s.updater = Updater::new(Some(CH), Arc::new(server), Current { tag: Some("fork-storage-1".into()), built: Some(1) });
    s.updater.set_layout(Ok(l));
    s.updater.status.latest = Some(rel);
    s.updater.status.state = "available".into();
    let v = s.execute("app.update.install", json!({"wait": true})).unwrap();
    assert_eq!(v["install"], "installed", "{v}");
    assert_eq!(v["installed"], app.to_string_lossy().as_ref());
    assert_eq!(std::fs::read(&app).unwrap(), b"new appimage");
    assert_eq!(std::fs::metadata(&app).unwrap().permissions().mode() & 0o777, 0o755);
    let prev = previous_appimage(&app);
    assert_eq!(std::fs::read(&prev).unwrap(), b"old");
    assert_eq!(s.prefs.update.previous, prev.to_string_lossy());
    assert!(!app.with_file_name(".fork-next-1.AppImage.part").exists());
    assert!(!app.with_file_name(".fork-old-1.AppImage.part").exists(), "old leftovers are removed");
    let v = s.execute("app.update.rollback", json!({})).unwrap();
    assert_eq!(v["rolledBack"], app.to_string_lossy().as_ref());
    assert_eq!(std::fs::read(&app).unwrap(), b"old");
    assert!(!prev.exists());
    assert!(s.execute("app.update.rollback", json!({})).is_err(), "nothing left to go back to");
}

#[test]
fn a_failed_appimage_update_leaves_the_running_one_untouched() {
    let body = b"new appimage".as_slice();
    let (good, _) = appimage_release(body, body);
    let cases: Vec<(&str, Release, Server)> = vec![
        ("bad checksum", good.clone(), appimage_release(body, b"other").1),
        ("interrupted", good.clone(), Server { cut: Some(4), ..appimage_release(body, body).1 }),
        ("size differs", Release { appimage: good.appimage.clone().map(|a| Asset { size: 3, ..a }), ..good.clone() }, appimage_release(body, body).1),
        ("no AppImage", Release { appimage: None, ..good.clone() }, appimage_release(body, body).1),
        ("no sums", Release { sums: None, ..good.clone() }, appimage_release(body, body).1),
        (
            "too large",
            Release { appimage: good.appimage.clone().map(|a| Asset { size: MAX_TARBALL_BYTES + 1, ..a }), ..good.clone() },
            appimage_release(body, body).1,
        ),
    ];
    for (i, (what, rel, server)) in cases.into_iter().enumerate() {
        let (app, l) = appimage_dir(&format!("fail-{i}"));
        let r = install(&server, &rel, "fork-", &l, &mut |_, _| true);
        assert!(r.is_err(), "{what}: {r:?}");
        assert_eq!(std::fs::read(&app).unwrap(), b"old", "{what}");
        let mut names: Vec<_> = std::fs::read_dir(app.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let left = names.iter().filter(|n| *n != "mnt" && !n.starts_with(".fork-old-1")).count();
        assert_eq!(left, 1, "{what}: only the AppImage is left: {names:?}");
    }
}

#[test]
fn packages_and_installers_say_how_they_update() {
    assert!(layout_in(Path::new("/x"), true, None, None, None).unwrap_err().contains(".flatpak"));
    let home = Some(Path::new("/home/u"));
    for exe in ["/usr/bin/revolution", "/usr/local/bin/revolution", "/opt/revolution/revolution", "/home/u/.local/bin/revolution"] {
        assert!(packaged_install(Path::new(exe), home).is_some_and(|w| w.contains("package manager")), "{exe}");
    }
    assert_eq!(packaged_install(Path::new("/home/u/Apps/revolution-0.1.3/revolution"), home), None);
    // an empty $APPIMAGE is no AppImage; a missing one is an error, not a builds-folder update
    assert!(layout_in(Path::new("/nonexistent/revolution"), false, Some(Path::new("")), Some(Path::new("/")), None).is_err());
    assert!(layout_in(Path::new("/"), false, Some(Path::new("/nonexistent/R.AppImage")), Some(Path::new("/")), None).unwrap_err().contains("AppImage"));
}

/// `$APPIMAGE` inherited from another program's AppImage (a terminal run as one) must never make
/// the update replace that program's file.
#[test]
fn an_inherited_appimage_variable_is_not_ours() {
    let l = builds("inherited");
    let other = l.builds.join("Terminal.AppImage");
    std::fs::write(&other, b"someone else").unwrap();
    let exe = l.current.join("filmcraft");
    for appdir in [None, Some(Path::new("/tmp/.mount_TermXYZ")), Some(Path::new(""))] {
        let got = layout_in(&exe, false, Some(&other), appdir, None).unwrap();
        assert_eq!(got.appimage, None, "{appdir:?}");
    }
}

#[test]
fn release_files_are_named_for_this_processor() {
    let arch = crate::update::linux_arch();
    assert_eq!(crate::update::tarball_name("revolution-0.2"), format!("revolution-0.2-linux-{arch}.tar.gz"));
    assert_eq!(crate::update::appimage_name("revolution-0.2"), format!("revolution-0.2-linux-{arch}.AppImage"));
}
