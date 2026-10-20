//! The WebDAV backend against a minimal in-process WebDAV server (std TcpListener, one thread
//! per connection, `Connection: close`), backed by a `MemoryStore`. Hostile replies are
//! injected with a hook.
#![cfg(feature = "net")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use filmcraft_remote::lock::{self, Acquire, LockInfo};
use filmcraft_remote::{Kind, Location, MemoryStore, RemoteError, RemotePath, RemoteStore, WebDav};

const PREFIX: &str = "/dav";
const USER: &str = "ana";
const PASS: &str = "s3cret";
const MODIFIED: i64 = 784_111_777;
const MODIFIED_TEXT: &str = "Sun, 06 Nov 1994 08:49:37 GMT";

struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

type Hook = Box<dyn Fn(&Req) -> Option<Vec<u8>> + Send + Sync>;

struct Server {
    addr: SocketAddr,
    store: Arc<MemoryStore>,
    hook: Arc<Mutex<Option<Hook>>>,
    /// Drop the connection after this many bytes of a PUT body (u64::MAX: never).
    drop_put_after: Arc<AtomicU64>,
    /// Requests a real server would refuse or that must never be sent (folder DELETEs, chunked
    /// PUTs, DELETEs without If-Match).
    violations: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let store = Arc::new(MemoryStore::new());
        store.set_clock(MODIFIED);
        let hook: Arc<Mutex<Option<Hook>>> = Arc::new(Mutex::new(None));
        let drop_put_after = Arc::new(AtomicU64::new(u64::MAX));
        let violations = Arc::new(Mutex::new(Vec::new()));
        let (s, h, d, f) = (store.clone(), hook.clone(), drop_put_after.clone(), violations.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (s, h, d, f) = (s.clone(), h.clone(), d.clone(), f.clone());
                std::thread::spawn(move || {
                    let _ = serve(conn, &s, &h, &d, &f);
                });
            }
        });
        Server { addr, store, hook, drop_put_after, violations }
    }

    fn location(&self, root: &str) -> Location {
        Location {
            id: "t".into(),
            name: "test".into(),
            kind: Kind::WebDav,
            url: format!("http://{}{PREFIX}/", self.addr),
            user: USER.into(),
            root: root.into(),
            region: String::new(),
        }
    }

    fn client(&self) -> WebDav {
        WebDav::new(&self.location(""), PASS).unwrap()
    }

    fn set_hook(&self, f: impl Fn(&Req) -> Option<Vec<u8>> + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Box::new(f));
    }
}

fn read_line(r: &mut BufReader<TcpStream>) -> std::io::Result<String> {
    let mut line = String::new();
    r.by_ref().take(16 << 10).read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn serve(conn: TcpStream, store: &MemoryStore, hook: &Mutex<Option<Hook>>, drop_put_after: &AtomicU64, violations: &Mutex<Vec<String>>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut out = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    let line = read_line(&mut r)?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
    let mut headers = Vec::new();
    loop {
        let h = read_line(&mut r)?;
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut req = Req { method, target, headers, body: Vec::new() };
    if req.header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        if req.method == "PUT" {
            // like nginx: uploads must announce their length
            violations.lock().unwrap().push(format!("chunked PUT {}", req.target));
            out.write_all(b"HTTP/1.1 411 Length Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return out.shutdown(std::net::Shutdown::Both);
        }
        loop {
            let size = usize::from_str_radix(read_line(&mut r)?.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
            if size == 0 {
                let _ = read_line(&mut r);
                break;
            }
            let mut chunk = vec![0u8; size];
            r.read_exact(&mut chunk)?;
            req.body.extend_from_slice(&chunk);
            read_line(&mut r)?;
        }
    } else if let Some(len) = req.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        let drop_at = drop_put_after.load(Ordering::SeqCst);
        if req.method == "PUT" && (len as u64) > drop_at {
            drop_put_after.store(u64::MAX, Ordering::SeqCst);
            let mut part = vec![0u8; drop_at as usize];
            r.read_exact(&mut part)?;
            // hang up mid-upload
            return out.shutdown(std::net::Shutdown::Both);
        }
        req.body = vec![0u8; len];
        r.read_exact(&mut req.body)?;
    }
    if let Some(raw) = hook.lock().unwrap().as_ref().and_then(|h| h(&req)) {
        out.write_all(&raw)?;
        return out.shutdown(std::net::Shutdown::Both);
    }
    let (status, extra, body) = handle(&req, store, violations);
    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(&body)?;
    out.flush()
}

fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn encode(p: &RemotePath, dir: bool) -> String {
    let mut s = String::from(PREFIX);
    for seg in p.segments() {
        s.push('/');
        for b in seg.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                s.push(char::from(b));
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
    }
    if dir || p.is_root() {
        s.push('/');
    }
    s
}

/// A request target or Destination (absolute URL or path) as a store path.
fn target_path(t: &str) -> Option<RemotePath> {
    let path = match t.strip_prefix("http://") {
        Some(rest) => &rest[rest.find('/')?..],
        None => t,
    };
    let rest = path.strip_prefix(PREFIX)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    RemotePath::parse(&decode(rest)?).ok()
}

fn is_dir(store: &MemoryStore, p: &RemotePath) -> Option<bool> {
    store.stat(p).ok().map(|e| e.is_dir)
}

fn parent_ok(store: &MemoryStore, p: &RemotePath) -> bool {
    p.parent().is_none_or(|par| is_dir(store, &par) == Some(true))
}

type Reply = (u16, Vec<(String, String)>, Vec<u8>);

fn handle(req: &Req, store: &MemoryStore, violations: &Mutex<Vec<String>>) -> Reply {
    let expected = format!("Basic {}", b64(format!("{USER}:{PASS}").as_bytes()));
    if req.header("authorization") != Some(expected.as_str()) {
        return (401, vec![("WWW-Authenticate".into(), "Basic realm=\"t\"".into())], Vec::new());
    }
    let Some(path) = target_path(&req.target) else { return (400, vec![], Vec::new()) };
    let none = |s: u16| (s, vec![], Vec::new());
    match req.method.as_str() {
        "PROPFIND" => {
            let Ok(me) = store.stat(&path) else { return none(404) };
            let mut entries = vec![me.clone()];
            if me.is_dir && req.header("depth") != Some("0") {
                entries.extend(store.list(&path).unwrap());
            }
            let mut xml = String::from(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
            for e in entries {
                let rt = if e.is_dir { "<D:collection/>" } else { "" };
                let len = if e.is_dir { String::new() } else { format!("<D:getcontentlength>{}</D:getcontentlength>", e.size) };
                let etag = e.etag.as_deref().map(|t| format!("<D:getetag>{}</D:getetag>", t.replace('"', "&quot;"))).unwrap_or_default();
                xml.push_str(&format!(
                    "<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype>{rt}</D:resourcetype>{len}<D:getlastmodified>{MODIFIED_TEXT}</D:getlastmodified>{etag}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
                    encode(&e.path, e.is_dir)
                ));
            }
            xml.push_str("</D:multistatus>");
            (207, vec![("Content-Type".into(), "application/xml".into())], xml.into_bytes())
        }
        "GET" => match is_dir(store, &path) {
            None => none(404),
            Some(true) => none(405),
            Some(false) => {
                let data = store.read_all(&path, 1 << 30).unwrap();
                let len = data.len() as u64;
                let Some(range) = req.header("range").and_then(|r| r.strip_prefix("bytes=")) else { return (200, vec![], data) };
                let (a, b) = range.split_once('-').unwrap();
                let a: u64 = a.parse().unwrap();
                let b: u64 = if b.is_empty() { u64::MAX } else { b.parse().unwrap() };
                if a >= len {
                    return none(416);
                }
                let end = b.min(len - 1);
                let body = data[a as usize..=end as usize].to_vec();
                (206, vec![("Content-Range".into(), format!("bytes {a}-{end}/{len}"))], body)
            }
        },
        "PUT" => {
            if is_dir(store, &path) == Some(true) {
                return none(405);
            }
            if !parent_ok(store, &path) {
                return none(409);
            }
            if req.header("if-none-match") == Some("*") {
                match store.create_new(&path, &req.body) {
                    Ok(()) => none(201),
                    Err(RemoteError::AlreadyExists(_)) => none(412),
                    Err(_) => none(500),
                }
            } else {
                store.write(&path, &mut &req.body[..], req.body.len() as u64, &mut |_| true).unwrap();
                none(201)
            }
        }
        "MKCOL" => {
            if is_dir(store, &path).is_some() {
                return none(405);
            }
            if !parent_ok(store, &path) {
                return none(409);
            }
            store.make_dir(&path).unwrap();
            none(201)
        }
        "MOVE" => {
            let Some(dest) = req.header("destination").and_then(target_path) else { return none(400) };
            if is_dir(store, &path).is_none() {
                return none(404);
            }
            let exists = is_dir(store, &dest).is_some();
            if exists && req.header("overwrite") == Some("F") {
                return none(412);
            }
            if !parent_ok(store, &dest) {
                return none(409);
            }
            match store.rename(&path, &dest) {
                Ok(()) => none(if exists { 204 } else { 201 }),
                Err(_) => none(500),
            }
        }
        "DELETE" => match is_dir(store, &path) {
            None => none(404),
            Some(true) => {
                violations.lock().unwrap().push(format!("DELETE of the folder {path}"));
                none(403)
            }
            Some(false) => {
                let etag = store.stat(&path).unwrap().etag;
                match req.header("if-match") {
                    Some(m) if Some(m) != etag.as_deref() => return none(412),
                    Some(_) => {}
                    None if req.target.contains(".part-") => {} // the best-effort clean-up
                    None => violations.lock().unwrap().push(format!("DELETE without If-Match {path}")),
                }
                store.delete(&path).unwrap();
                none(204)
            }
        },
        _ => none(405),
    }
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            s.push(if i <= c.len() { char::from(A[((n >> shift) & 63) as usize]) } else { '=' });
        }
    }
    s
}

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

fn put(d: &WebDav, path: &str, bytes: &[u8]) -> filmcraft_remote::Result<()> {
    d.write(&p(path), &mut &bytes[..], bytes.len() as u64, &mut |_| true)
}

fn no_temp_files(s: &Server) {
    let files = s.store.files();
    assert!(files.iter().all(|f| !f.name().contains(".part-")), "{files:?}");
}

fn raw(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

#[test]
fn round_trip() {
    let s = Server::start();
    let d = s.client();
    let mut seen = 0;
    d.write(&p("proj/a.rev"), &mut &b"hello"[..], 5, &mut |n| {
        seen = n;
        true
    })
    .unwrap();
    assert_eq!(seen, 5);
    let root = d.list(&RemotePath::root()).unwrap();
    assert_eq!(root.iter().map(|e| (e.name(), e.is_dir)).collect::<Vec<_>>(), [("proj", true)]);
    let files = d.list(&p("proj")).unwrap();
    assert_eq!(files.len(), 1);
    let e = &files[0];
    assert_eq!((e.name(), e.is_dir, e.size, e.modified), ("a.rev", false, 5, Some(MODIFIED)));
    assert!(e.etag.is_some());
    assert_eq!(d.stat(&p("proj/a.rev")).unwrap(), *e);
    assert!(d.stat(&p("proj")).unwrap().is_dir);
    assert!(d.stat(&RemotePath::root()).unwrap().is_dir);
    assert!(matches!(d.stat(&p("nope")), Err(RemoteError::NotFound(_))));
    assert!(matches!(d.list(&p("nope")), Err(RemoteError::NotFound(_))));
    assert!(matches!(d.list(&p("proj/a.rev")), Err(RemoteError::NotFound(_))));
    assert_eq!(d.read_range(&p("proj/a.rev"), 1, 3).unwrap(), b"ell");
    assert_eq!(d.read_range(&p("proj/a.rev"), 3, 99).unwrap(), b"lo");
    assert_eq!(d.read_range(&p("proj/a.rev"), 99, 3).unwrap(), b"");
    assert_eq!(d.read_range(&p("proj/a.rev"), u64::MAX, 3).unwrap(), b"");
    assert!(d.read_range(&p("proj/a.rev"), 0, u64::MAX).is_err());
    assert_eq!(d.read_all(&p("proj/a.rev"), 5).unwrap(), b"hello");
    assert!(matches!(d.read_all(&p("proj/a.rev"), 4), Err(RemoteError::TooLarge(_))));
    assert!(matches!(d.read_range(&p("proj/missing"), 0, 4), Err(RemoteError::NotFound(_))));

    // overwrite, a bigger file, ranged reads across it
    put(&d, "proj/a.rev", b"hello again").unwrap();
    assert_eq!(d.read_all(&p("proj/a.rev"), 100).unwrap(), b"hello again");
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    put(&d, "media/clip one.mov", &big).unwrap();
    assert_eq!(d.read_range(&p("media/clip one.mov"), 1_000_000, 70_000).unwrap(), &big[1_000_000..1_070_000]);

    d.rename(&p("proj/a.rev"), &p("proj/old/b.rev")).unwrap();
    assert!(matches!(d.stat(&p("proj/a.rev")), Err(RemoteError::NotFound(_))));
    assert_eq!(d.read_all(&p("proj/old/b.rev"), 100).unwrap(), b"hello again");
    assert!(matches!(d.rename(&p("proj/old"), &p("x")), Err(RemoteError::Forbidden(_))));
    assert!(matches!(d.rename(&p("proj/old/b.rev"), &p("proj")), Err(RemoteError::AlreadyExists(_))));
    assert!(matches!(d.write(&p("proj"), &mut &b"x"[..], 1, &mut |_| true), Err(RemoteError::AlreadyExists(_))));

    d.make_dir(&p("x/y/z")).unwrap();
    d.make_dir(&p("x/y/z")).unwrap();
    assert!(d.stat(&p("x/y/z")).unwrap().is_dir);
    assert!(matches!(d.make_dir(&p("proj/old/b.rev")), Err(RemoteError::AlreadyExists(_))));
    assert!(d.make_dir(&p("proj/old/b.rev/sub")).is_err());

    d.delete(&p("proj/old/b.rev")).unwrap();
    assert!(d.list(&p("proj/old")).unwrap().is_empty());
    assert!(matches!(d.delete(&p("x")), Err(RemoteError::Forbidden(_))));
    assert!(matches!(d.delete(&RemotePath::root()), Err(RemoteError::Forbidden(_))));
    assert!(matches!(d.delete(&p("gone")), Err(RemoteError::NotFound(_))));
    assert!(s.violations.lock().unwrap().is_empty(), "{:?}", s.violations.lock().unwrap());

    // create_new: parents made on demand, exactly once
    d.create_new(&p("deep/er/lock"), b"1").unwrap();
    assert!(matches!(d.create_new(&p("deep/er/lock"), b"2"), Err(RemoteError::AlreadyExists(_))));
    assert!(matches!(d.create_new(&p("deep"), b"2"), Err(RemoteError::AlreadyExists(_))));
    assert_eq!(d.read_all(&p("deep/er/lock"), 10).unwrap(), b"1");

    // cancelled, short and long uploads leave nothing behind
    assert!(matches!(d.write(&p("c/x"), &mut &b"abc"[..], 3, &mut |_| false), Err(RemoteError::Cancelled)));
    assert!(d.write(&p("c/short"), &mut &b"abc"[..], 4, &mut |_| true).is_err());
    assert!(d.write(&p("c/long"), &mut &b"abc"[..], 2, &mut |_| true).is_err());
    for f in ["c/x", "c/short", "c/long"] {
        assert!(matches!(d.stat(&p(f)), Err(RemoteError::NotFound(_))), "{f}");
    }
    no_temp_files(&s);
}

#[test]
fn location_root_is_a_prefix() {
    let s = Server::start();
    s.store.make_dir(&p("team/films")).unwrap();
    let d = WebDav::new(&s.location("team/films"), PASS).unwrap();
    put(&d, "cut.rev", b"x").unwrap();
    assert_eq!(s.store.files(), [p("team/films/cut.rev")]);
    assert_eq!(d.list(&RemotePath::root()).unwrap().iter().map(|e| e.path.clone()).collect::<Vec<_>>(), [p("cut.rev")]);
}

#[test]
fn wrong_password_is_unauthorized() {
    let s = Server::start();
    let d = WebDav::new(&s.location(""), "wrong").unwrap();
    assert!(matches!(d.list(&RemotePath::root()), Err(RemoteError::Unauthorized)));
    assert!(matches!(put(&d, "a", b"x"), Err(RemoteError::Unauthorized)));
    assert!(matches!(d.create_new(&p("a"), b"x"), Err(RemoteError::Unauthorized)));
    assert!(s.store.files().is_empty());
}

#[test]
fn locks_across_two_machines() {
    let s = Server::start();
    let (a, b) = (s.client(), s.client());
    let me = |sess: &str| LockInfo { holder: "Ana".into(), machine: format!("pc-{sess}"), session: sess.into(), since: 1_700_000_000 };
    let proj = p("films/cut.rev");
    assert_eq!(lock::acquire(&a, &proj, &me("a")).unwrap(), Acquire::Acquired);
    assert_eq!(lock::acquire(&a, &proj, &me("a")).unwrap(), Acquire::Acquired);
    assert_eq!(lock::acquire(&b, &proj, &me("b")).unwrap(), Acquire::HeldBy(me("a")));
    assert_eq!(lock::take_over(&b, &proj, &me("b")).unwrap(), Acquire::Acquired);
    assert!(!lock::still_ours(&a, &proj, &me("a")).unwrap());
    assert!(lock::release(&b, &proj, &me("b")).unwrap());
    assert_eq!(lock::read(&a, &proj).unwrap(), None);

    // racing machines: exactly one winner
    let s = Arc::new(s);
    let won: usize = (0..6)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || lock::acquire(&s.client(), &p("race/x.rev"), &me(&i.to_string())).unwrap() == Acquire::Acquired)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum();
    assert_eq!(won, 1);
}

fn multistatus(responses: &[&str]) -> Vec<u8> {
    let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
    for href in responses {
        let rt = if href.ends_with('/') { "<d:resourcetype><d:collection/></d:resourcetype>" } else { "<d:resourcetype/>" };
        xml.push_str(&format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{rt}<d:getcontentlength>3</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        ));
    }
    xml.push_str("</d:multistatus>");
    xml.into_bytes()
}

#[test]
fn hostile_listings_are_skipped_or_refused() {
    let s = Server::start();
    let d = s.client();
    let addr = s.addr;
    s.set_hook(move |r| {
        (r.method == "PROPFIND").then(|| {
            let body = multistatus(&[
                "/dav/",
                "/dav/ok.txt",
                &format!("http://{addr}/dav/also%20ok.txt"),
                "/dav/../etc/passwd",
                "/dav/%2e%2e/x",
                "/dav/sub/deeper.txt",
                "/dav/a%2Fb",
                "/other/x",
                "http://evil.example/dav/evil.txt",
                "/dav/bad%ZZ",
                "/dav/nul%00",
                "dav/relative",
                "/dav//double",
            ]);
            raw("207 Multi-Status", &[("Content-Length", body.len().to_string())], &body)
        })
    });
    let names: Vec<String> = d.list(&RemotePath::root()).unwrap().iter().map(|e| e.name().to_string()).collect();
    assert_eq!(names, ["ok.txt", "also ok.txt"]);

    // malformed XML, wrong root element, a body that isn't UTF-8
    for bad in [&b"<d:multistatus xmlns:d=\"DAV:\"><d:response>"[..], b"<html>hi</html>", b"\xff\xfe\x00"] {
        let bad = bad.to_vec();
        s.set_hook(move |_| Some(raw("207 Multi-Status", &[("Content-Length", bad.len().to_string())], &bad)));
        assert!(matches!(d.list(&RemotePath::root()), Err(RemoteError::Protocol(_))));
        assert!(d.stat(&p("x")).is_err());
    }

    // garbage dates and lengths: the entry is kept, the values dropped
    s.set_hook(|_| {
        let body = br#"<multistatus xmlns="DAV:"><response><href>/dav/f</href><propstat><prop><getcontentlength>lots</getcontentlength><getlastmodified>Blursday, 99 Zzz 1994 99:99:99 GMT</getlastmodified><getetag></getetag></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        Some(raw("207 Multi-Status", &[("Content-Length", body.len().to_string())], body))
    });
    let e = d.stat(&p("f")).unwrap();
    assert_eq!((e.size, e.modified, e.etag, e.is_dir), (0, None, None, false));

    // an announced huge reply is refused before reading it
    s.set_hook(|_| Some(raw("207 Multi-Status", &[("Content-Length", (100u64 << 20).to_string())], b"<x")));
    assert!(matches!(d.list(&RemotePath::root()), Err(RemoteError::TooLarge(_))));
    // an unannounced one (ends at connection close) is cut off at the cap
    s.set_hook(|r| {
        (r.method == "PROPFIND").then(|| {
            let mut body = br#"<d:multistatus xmlns:d="DAV:">"#.to_vec();
            body.resize((65 << 20) + 10, b' ');
            raw("207 Multi-Status", &[], &body)
        })
    });
    assert!(matches!(d.list(&RemotePath::root()), Err(RemoteError::TooLarge(_))));
    // other statuses
    s.set_hook(|_| Some(raw("500 Oops", &[("Content-Length", "0".into())], b"")));
    assert!(d.list(&RemotePath::root()).unwrap_err().is_transient());
    s.set_hook(|_| Some(raw("429 Slow down", &[("Content-Length", "0".into())], b"")));
    assert!(d.stat(&p("x")).unwrap_err().is_transient());
    s.set_hook(|_| Some(raw("400 Bad", &[("Content-Length", "0".into())], b"")));
    assert!(matches!(d.list(&RemotePath::root()), Err(RemoteError::Protocol(_))));
    s.set_hook(|_| Some(raw("403 No", &[("Content-Length", "0".into())], b"")));
    assert!(matches!(d.stat(&p("x")), Err(RemoteError::Forbidden(_))));
    s.set_hook(|_| Some(b"garbage that is not HTTP\r\n\r\n".to_vec()));
    assert!(d.list(&RemotePath::root()).is_err());
}

#[test]
fn hostile_range_replies() {
    let s = Server::start();
    let d = s.client();
    let data: Vec<u8> = (0..200u8).collect();
    // a 206 longer than asked: only the asked bytes are returned
    let body = data.clone();
    s.set_hook(move |_| Some(raw("206 Partial", &[("Content-Range", "bytes 10-19/200".into()), ("Content-Length", "190".into())], &body[10..])));
    assert_eq!(d.read_range(&p("f"), 10, 10).unwrap(), &data[10..20]);
    // a 206 for a different range is refused
    let body = data.clone();
    s.set_hook(move |_| Some(raw("206 Partial", &[("Content-Range", "bytes 0-9/200".into()), ("Content-Length", "10".into())], &body[..10])));
    assert!(matches!(d.read_range(&p("f"), 10, 10), Err(RemoteError::Protocol(_))));
    // no Content-Length (ends at close), no Content-Range
    let body = data.clone();
    s.set_hook(move |_| Some(raw("206 Partial", &[], &body[50..])));
    assert_eq!(d.read_range(&p("f"), 50, 5).unwrap(), &data[50..55]);
    // a server that ignores Range: skip to the offset
    let body = data.clone();
    s.set_hook(move |_| Some(raw("200 OK", &[("Content-Length", "200".into())], &body)));
    assert_eq!(d.read_range(&p("f"), 100, 5).unwrap(), &data[100..105]);
    assert_eq!(d.read_range(&p("f"), 198, 5).unwrap(), &data[198..]);
    assert_eq!(d.read_range(&p("f"), 500, 5).unwrap(), b"");
    assert!(matches!(d.read_range(&p("f"), 1 << 40, 5), Err(RemoteError::Protocol(_))));
    // a Content-Length longer than what arrives
    let body = data.clone();
    s.set_hook(move |_| Some(raw("206 Partial", &[("Content-Length", "1000".into())], &body[..20])));
    assert!(d.read_range(&p("f"), 0, 100).is_err());
    s.set_hook(|_| Some(raw("416 Range Not Satisfiable", &[("Content-Length", "0".into())], b"")));
    assert_eq!(d.read_range(&p("f"), 0, 100).unwrap(), b"");
}

#[test]
fn a_dropped_upload_leaves_the_old_file() {
    let s = Server::start();
    let d = s.client();
    put(&d, "proj/cut.rev", b"old").unwrap();
    s.drop_put_after.store(1000, Ordering::SeqCst);
    let big = vec![7u8; 100 << 10];
    let err = put(&d, "proj/cut.rev", &big).unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert_eq!(d.read_all(&p("proj/cut.rev"), 10).unwrap(), b"old");
    assert_eq!(s.store.files(), [p("proj/cut.rev")]);
    // and a new file that never finished doesn't exist at all
    s.drop_put_after.store(10, Ordering::SeqCst);
    assert!(put(&d, "proj/new.bin", &big).is_err());
    assert!(matches!(d.stat(&p("proj/new.bin")), Err(RemoteError::NotFound(_))));
    no_temp_files(&s);
}

#[test]
fn unreachable_server_is_a_network_error() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let loc = Location {
        id: "t".into(),
        name: "t".into(),
        kind: Kind::WebDav,
        url: format!("http://127.0.0.1:{port}/"),
        user: String::new(),
        root: String::new(),
        region: String::new(),
    };
    let d = WebDav::new(&loc, "").unwrap();
    assert!(d.list(&RemotePath::root()).unwrap_err().is_transient());
}

#[test]
fn awkward_names_round_trip() {
    let s = Server::start();
    let d = s.client();
    let dir = "odd ?#% dir ünï";
    let name = "clip #1 ?100% 🎬 ünï cødé.mov";
    let path = format!("{dir}/{name}");
    let data: Vec<u8> = (0..70_000u32).map(|i| (i % 253) as u8).collect();
    put(&d, &path, &data).unwrap();
    assert_eq!(s.store.files(), [p(&path)]);
    assert_eq!(d.list(&RemotePath::root()).unwrap().iter().map(|e| e.name().to_string()).collect::<Vec<_>>(), [dir]);
    let listed = d.list(&p(dir)).unwrap();
    assert_eq!(listed.iter().map(|e| (e.name().to_string(), e.size)).collect::<Vec<_>>(), [(name.to_string(), data.len() as u64)]);
    assert_eq!(d.stat(&p(&path)).unwrap().size, data.len() as u64);
    assert_eq!(d.read_range(&p(&path), 65_000, 100).unwrap(), &data[65_000..65_100]);
    let other = format!("{dir}/a?b#c%d e é.rev");
    d.rename(&p(&path), &p(&other)).unwrap();
    d.create_new(&p(&format!("{dir}/.l?ck #%.revlock")), b"1").unwrap();
    d.delete(&p(&other)).unwrap();
    assert_eq!(s.store.files(), [p(&format!("{dir}/.l?ck #%.revlock"))]);
    // every upload above announced its length (chunked PUTs are refused and recorded)
    assert!(s.violations.lock().unwrap().is_empty(), "{:?}", s.violations.lock().unwrap());
}

#[test]
fn delete_is_conditional_on_what_was_stated() {
    let s = Server::start();
    let d = s.client();
    put(&d, "f", b"x").unwrap();
    // the file changes between our stat and our DELETE
    s.set_hook(|r| (r.method == "DELETE").then(|| raw("412 Precondition Failed", &[("Content-Length", "0".into())], b"")));
    let err = d.delete(&p("f")).unwrap_err();
    assert!(matches!(&err, RemoteError::Protocol(m) if m.contains("changed")), "{err:?}");
    *s.hook.lock().unwrap() = None;
    d.delete(&p("f")).unwrap();
    assert!(s.store.files().is_empty());
    assert!(s.violations.lock().unwrap().is_empty(), "{:?}", s.violations.lock().unwrap());
}

#[test]
fn credentials_in_the_address_are_refused() {
    let s = Server::start();
    let mut loc = s.location("");
    loc.url = format!("http://{USER}:{PASS}@{}/dav/", s.addr);
    let err = WebDav::new(&loc, PASS).unwrap_err();
    assert!(matches!(&err, RemoteError::BadPath(m) if m.contains("User") && !m.contains(PASS)), "{err:?}");
}
