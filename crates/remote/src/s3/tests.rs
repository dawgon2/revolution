//! S3 backend tests against a minimal in-process S3 server that checks every request's
//! Signature V4.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::lock;

const ACCESS: &str = "AKIDTESTKEY";
const SECRET: &str = "test/Secret+Key=";

// ---------------------------------------------------------------------------------------------
// Signature V4, HMAC, dates

#[test]
fn hmac_matches_rfc_4231() {
    assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    // a key longer than the block is hashed first (RFC 4231 test case 6)
    let key = [0xaau8; 131];
    assert_eq!(
        hex(&hmac_sha256(&key, b"Test Using Larger Than Block-Size Key - Hash Key First")),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn signature_matches_the_aws_get_object_example() {
    // The "GET Object" example of the S3 Signature V4 documentation.
    let headers = vec![
        ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
        ("range".to_string(), "bytes=0-9".to_string()),
        ("x-amz-content-sha256".to_string(), EMPTY_SHA256.to_string()),
        ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
    ];
    let s = SignInput {
        method: "GET",
        path: "/test.txt",
        query: &[],
        headers: &headers,
        payload_hash: EMPTY_SHA256,
        amz_date: "20130524T000000Z",
        region: "us-east-1",
        access_key: "AKIAIOSFODNN7EXAMPLE",
        secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    };
    let (req, signed) = s.canonical_request();
    assert_eq!(signed, "host;range;x-amz-content-sha256;x-amz-date");
    assert!(req.starts_with("GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nrange:bytes=0-9\n"));
    assert_eq!(s.signature().0, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    assert_eq!(
        s.authorization(),
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
         SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
         Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
}

#[test]
fn encoding_and_query_are_canonical() {
    assert_eq!(encode_key("a b/é~*"), "a%20b/%C3%A9~%2A");
    assert_eq!(percent_encode("a/b", false), "a%2Fb");
    let q = vec![("prefix".to_string(), "x/y z".to_string()), ("list-type".to_string(), "2".to_string()), ("uploads".to_string(), String::new())];
    assert_eq!(canonical_query(&q), "list-type=2&prefix=x%2Fy%20z&uploads=");
}

#[test]
fn dates_round_trip() {
    assert_eq!(amz_date(0), "19700101T000000Z");
    assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
    assert_eq!(amz_date(951_782_400), "20000229T000000Z");
    assert_eq!(parse_iso8601("2024-01-02T03:04:05.000Z"), Some(1_704_164_645));
    assert_eq!(parse_iso8601("2013-05-24T00:00:00Z"), Some(1_369_353_600));
    assert_eq!(parse_http_date("Tue, 02 Jan 2024 03:04:05 GMT"), Some(1_704_164_645));
    for bad in ["", "2024", "2024-13-02T03:04:05Z", "2024-01-02T25:04:05Z", "２０２４-01-02T03:04:05Z", "+024-01-02T03:04:05Z"] {
        assert_eq!(parse_iso8601(bad), None, "{bad:?}");
    }
    assert_eq!(parse_http_date("Tue, 02 Foo 2024 03:04:05 GMT"), None);
    assert_eq!(parse_http_date("Tue, 99999999999999999999 Jan 2024 03:04:05 GMT"), None);
    // every day of a few centuries survives the round trip
    for days in (-200_000i64..200_000).step_by(997) {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(http::unix_time(y, m, d, 0, 0, 0), Some(days * 86_400));
    }
}

/// The S3 Signature V4 documentation's examples on a bucket (no object key, with a query).
fn aws_bucket_example(query: &[(&str, &str)]) -> String {
    let headers = vec![
        ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
        ("x-amz-content-sha256".to_string(), EMPTY_SHA256.to_string()),
        ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
    ];
    let query: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    SignInput {
        method: "GET",
        path: "/",
        query: &query,
        headers: &headers,
        payload_hash: EMPTY_SHA256,
        amz_date: "20130524T000000Z",
        region: "us-east-1",
        access_key: "AKIAIOSFODNN7EXAMPLE",
        secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    }
    .signature()
    .0
}

#[test]
fn signature_matches_the_aws_examples_with_a_query() {
    // "GET Bucket Lifecycle" (`?lifecycle`, an empty value)
    assert_eq!(aws_bucket_example(&[("lifecycle", "")]), "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543");
    // "Get Bucket (List Objects)" (`?max-keys=2&prefix=J`, sorted)
    assert_eq!(aws_bucket_example(&[("prefix", "J"), ("max-keys", "2")]), "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");
}

#[test]
fn location_parsing_and_redacted_debug() {
    let mut loc = location("http://127.0.0.1:9");
    loc.root = "/bkt/a/b/".into();
    let s = S3::new(&loc, "super-secret-value").unwrap();
    assert_eq!((s.bucket.as_str(), s.prefix.as_str(), s.region.as_str()), ("bkt", "a/b", "us-east-1"));
    assert_eq!(s.key(&RemotePath::parse("x/y.mov").unwrap()), "a/b/x/y.mov");
    assert_eq!(s.target("a/b/x y"), ("127.0.0.1:9".to_string(), "/bkt/a/b/x%20y".to_string()));
    let debug = format!("{s:?}");
    assert!(!debug.contains("super-secret-value") && debug.contains("redacted"), "{debug}");
    let v = S3::new(&location("https://S3.Example.com/proxy/"), "k").unwrap().virtual_hosted();
    assert_eq!(v.target("k"), ("bkt.s3.example.com".to_string(), "/proxy/k".to_string()));
    assert_eq!(v.target(""), ("bkt.s3.example.com".to_string(), "/proxy/".to_string()));
    for (url, root) in [
        ("ftp://x", "bkt"),
        ("http://", "bkt"),
        ("http://a@b", "bkt"),
        ("http://h/%2e%2e", "bkt"),
        ("http://h", ""),
        ("http://h", "b/../x"),
        ("http://h", "b c"),
    ] {
        let mut l = location(url);
        l.root = root.into();
        assert!(S3::new(&l, "k").is_err(), "{url} {root}");
    }
    let mut l = location("http://h");
    l.user = String::new();
    assert!(S3::new(&l, "k").is_err());
    assert!(S3::new(&location("http://h"), "").is_err());
}

#[test]
fn hostile_xml_is_refused_or_skipped() {
    assert!(parse_list(b"<ListBucketResult><Contents>").is_err());
    assert!(parse_list(b"\xff\xfe").is_err());
    assert!(parse_list(b"<Other/>").is_err());
    assert!(parse_list(b"<!DOCTYPE x [<!ENTITY a \"aaaa\">]><ListBucketResult>&a;</ListBucketResult>").is_err());
    let p = parse_list(b"<ListBucketResult><Contents><Size>-5</Size></Contents><Contents><Key>k</Key><Size>99999999999999999999999</Size><LastModified>junk</LastModified></Contents></ListBucketResult>").unwrap();
    assert_eq!(p.contents.len(), 1);
    assert_eq!((p.contents[0].size, p.contents[0].modified), (0, None));
    assert!(matches!(check_xml_error(&parse_xml("<Error><Code>SlowDown</Code></Error>").unwrap(), "x"), Err(RemoteError::Network(_))));
    assert_eq!(content_range_start("bytes 5-9/10"), Some(5));
    assert_eq!(content_range_start("bytes */10"), None);
}

#[test]
fn status_mapping() {
    assert!(matches!(map_status(403, "SignatureDoesNotMatch", "/a"), RemoteError::Unauthorized));
    assert!(matches!(map_status(401, "", "/a"), RemoteError::Unauthorized));
    assert!(matches!(map_status(403, "AccessDenied", "/a"), RemoteError::Forbidden(_)));
    assert!(matches!(map_status(404, "NoSuchKey", "/a"), RemoteError::NotFound(_)));
    assert!(matches!(map_status(412, "", "/a"), RemoteError::AlreadyExists(_)));
    assert!(matches!(map_status(409, "ConditionalRequestConflict", "/a"), RemoteError::AlreadyExists(_)));
    assert!(matches!(map_status(409, "BucketNotEmpty", "/a"), RemoteError::Protocol(_)));
    assert!(map_status(503, "SlowDown", "/a").is_transient());
    assert!(matches!(map_status(301, "PermanentRedirect", "/a"), RemoteError::Protocol(_)));
}

// ---------------------------------------------------------------------------------------------
// The test server

#[derive(Default)]
struct Srv {
    /// `bucket/key` → bytes, ETag
    objects: BTreeMap<String, (Vec<u8>, String)>,
    /// upload id → (bucket/key, part → bytes)
    uploads: BTreeMap<String, (String, BTreeMap<u32, Vec<u8>>)>,
    counter: u64,
    page: usize,
    /// Close the connection half-way through this part's upload (this many times).
    drop_part: Option<(u32, usize)>,
    /// Answer the next request with these raw bytes (then close).
    raw_next: Option<Vec<u8>>,
    /// Carry out the next such request, then close the connection without replying.
    lose_reply: Option<LoseReply>,
    /// `METHOD ?query-names` of every request (to check what was retried).
    log: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoseReply {
    CreateNew,
    Complete,
}

struct TestServer {
    addr: SocketAddr,
    st: Arc<Mutex<Srv>>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let st = Arc::new(Mutex::new(Srv { page: 1000, ..Default::default() }));
        let st2 = st.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let st = st2.clone();
                std::thread::spawn(move || serve(conn, &st));
            }
        });
        TestServer { addr, st }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Srv> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn store(&self, secret: &str) -> S3 {
        let mut loc = location(&format!("http://{}", self.addr));
        loc.root = "bkt/team/films".into();
        S3::new(&loc, secret).unwrap()
    }

    fn keys(&self) -> Vec<String> {
        self.lock().objects.keys().cloned().collect()
    }
}

fn location(url: &str) -> Location {
    Location { id: "s3".into(), name: "S3".into(), kind: Kind::S3, url: url.into(), user: ACCESS.into(), root: "bkt".into(), region: String::new() }
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    fn q(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn resp(status: u16, body: impl Into<Vec<u8>>) -> Response {
    Response { status, headers: Vec::new(), body: body.into() }
}

fn err(status: u16, code: &str) -> Response {
    resp(status, format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>m</Message></Error>"))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn read_line(r: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match r.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
    }
}

fn serve(conn: TcpStream, st: &Mutex<Srv>) {
    let mut w = conn.try_clone().unwrap();
    let mut r = BufReader::new(conn);
    loop {
        let Some(line) = read_line(&mut r) else { return };
        let mut parts = line.split(' ');
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        loop {
            let Some(h) = read_line(&mut r) else { return };
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let query: Vec<(String, String)> = query
            .split('&')
            .filter(|s| !s.is_empty())
            .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
            .map(|(k, v)| (percent_decode(k), percent_decode(v)))
            .collect();
        let mut req = Request { method, path: path.to_string(), query, headers, body: Vec::new() };
        // dropped connection in the middle of an upload part
        let drop_now = {
            let mut s = st.lock().unwrap_or_else(PoisonError::into_inner);
            let names: Vec<&str> = req.query.iter().map(|(k, _)| k.as_str()).collect();
            s.log.push(format!("{} {}", req.method, names.join("&")));
            let part = req.q("partNumber").and_then(|n| n.parse::<u32>().ok());
            match s.drop_part {
                Some((n, times)) if part == Some(n) && times > 0 => {
                    s.drop_part = if times > 1 { Some((n, times - 1)) } else { None };
                    true
                }
                _ => false,
            }
        };
        if drop_now {
            let mut half = vec![0u8; 3];
            let _ = r.read_exact(&mut half);
            let _ = w.shutdown(Shutdown::Both);
            return;
        }
        let mut chunked = false;
        if let Some(len) = req.header("content-length") {
            let mut body = vec![0u8; len.parse().unwrap()];
            if r.read_exact(&mut body).is_err() {
                return;
            }
            req.body = body;
        } else if req.header("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
            chunked = true;
            loop {
                let Some(size) = read_line(&mut r) else { return };
                let n = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16).unwrap();
                let mut chunk = vec![0u8; n + 2];
                if r.read_exact(&mut chunk).is_err() {
                    return;
                }
                if n == 0 {
                    break;
                }
                req.body.extend_from_slice(&chunk[..n]);
            }
        }
        let raw = st.lock().unwrap_or_else(PoisonError::into_inner).raw_next.take();
        if let Some(raw) = raw {
            let _ = w.write_all(&raw);
            let _ = w.shutdown(Shutdown::Both);
            return;
        }
        let head = req.method == "HEAD";
        // S3 wants a Content-Length on every upload (no chunked bodies without aws-chunked)
        let out = if matches!(req.method.as_str(), "PUT" | "POST") && (chunked || req.header("content-length").is_none()) {
            err(411, "MissingContentLength")
        } else {
            route(st, &req)
        };
        let lost = {
            let mut s = st.lock().unwrap_or_else(PoisonError::into_inner);
            let kind = if req.header("if-none-match").is_some() {
                Some(LoseReply::CreateNew)
            } else if req.method == "POST" && req.q("uploadId").is_some() {
                Some(LoseReply::Complete)
            } else {
                None
            };
            let hit = kind.is_some() && s.lose_reply == kind && out.status < 300;
            if hit {
                s.lose_reply = None;
            }
            hit
        };
        if lost {
            let _ = w.shutdown(Shutdown::Both);
            return;
        }
        let mut text = format!("HTTP/1.1 {} X\r\n", out.status);
        let mut has_len = false;
        for (k, v) in &out.headers {
            has_len |= k == "content-length";
            text.push_str(&format!("{k}: {v}\r\n"));
        }
        if !has_len {
            text.push_str(&format!("content-length: {}\r\n", out.body.len()));
        }
        text.push_str("\r\n");
        let mut bytes = text.into_bytes();
        if !head {
            bytes.extend_from_slice(&out.body);
        }
        if w.write_all(&bytes).is_err() {
            return;
        }
    }
}

/// Checks the request's Signature V4 with the server's copy of the secret.
fn verify(req: &Request) -> std::result::Result<(), Response> {
    let Some(auth) = req.header("authorization") else { return Err(err(403, "AccessDenied")) };
    let rest = auth.strip_prefix("AWS4-HMAC-SHA256 ").ok_or_else(|| err(400, "AuthorizationHeaderMalformed"))?;
    let mut cred = "";
    let mut signed = "";
    let mut sig = "";
    for part in rest.split(", ") {
        let (k, v) = part.split_once('=').ok_or_else(|| err(400, "AuthorizationHeaderMalformed"))?;
        match k {
            "Credential" => cred = v,
            "SignedHeaders" => signed = v,
            "Signature" => sig = v,
            _ => {}
        }
    }
    let mut c = cred.split('/');
    let (Some(access), Some(_date), Some(region), Some("s3"), Some("aws4_request")) = (c.next(), c.next(), c.next(), c.next(), c.next()) else {
        return Err(err(400, "AuthorizationHeaderMalformed"));
    };
    if access != ACCESS {
        return Err(err(403, "InvalidAccessKeyId"));
    }
    let headers: Vec<(String, String)> = signed.split(';').map(|h| (h.to_string(), req.header(h).unwrap_or_default().to_string())).collect();
    for must in ["host", "x-amz-date", "x-amz-content-sha256"] {
        if !signed.split(';').any(|h| h == must) {
            return Err(err(400, "AuthorizationHeaderMalformed"));
        }
    }
    let payload = req.header("x-amz-content-sha256").unwrap_or_default();
    if payload != UNSIGNED_PAYLOAD && payload != hex(&Sha256::digest(&req.body)) {
        return Err(err(400, "XAmzContentSHA256Mismatch"));
    }
    // the canonical path is the decoded path encoded again
    let path = encode_key(&percent_decode(&req.path));
    let amz_date = req.header("x-amz-date").unwrap_or_default();
    let s = SignInput {
        method: &req.method,
        path: &path,
        query: &req.query,
        headers: &headers,
        payload_hash: payload,
        amz_date,
        region,
        access_key: access,
        secret: SECRET,
    };
    if s.signature().0 != sig {
        return Err(err(403, "SignatureDoesNotMatch"));
    }
    Ok(())
}

const LAST_MODIFIED_XML: &str = "2024-01-02T03:04:05.000Z";
const LAST_MODIFIED_HTTP: &str = "Tue, 02 Jan 2024 03:04:05 GMT";

fn xml_list(s: &Srv, bucket: &str, req: &Request) -> Response {
    let prefix = req.q("prefix").unwrap_or_default();
    let delim = req.q("delimiter") == Some("/");
    let token = req.q("continuation-token");
    let max: usize = req.q("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000).min(s.page);
    let full_prefix = format!("{bucket}/{prefix}");
    let mut items: Vec<(bool, String)> = Vec::new(); // (is prefix, key)
    for k in s.objects.range(full_prefix.clone()..).map(|(k, _)| k) {
        let Some(key) = k.strip_prefix(&format!("{bucket}/")) else { continue };
        if !key.starts_with(prefix) {
            break;
        }
        if let Some(t) = token
            && (key <= t || (t.ends_with('/') && key.starts_with(t)))
        {
            continue;
        }
        let rest = &key[prefix.len()..];
        let item = match rest.find('/') {
            Some(i) if delim => (true, format!("{prefix}{}", &rest[..=i])),
            _ => (false, key.to_string()),
        };
        if items.last() == Some(&item) {
            continue;
        }
        items.push(item);
        if items.len() > max {
            break;
        }
    }
    let truncated = items.len() > max;
    items.truncate(max);
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    xml.push_str(&format!("<Name>{bucket}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys>", xml_escape(prefix), items.len()));
    xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
    if truncated && let Some((_, last)) = items.last() {
        xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", xml_escape(last)));
    }
    for (is_prefix, key) in &items {
        if *is_prefix {
            xml.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", xml_escape(key)));
        } else {
            let (bytes, etag) = &s.objects[&format!("{bucket}/{key}")];
            xml.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{LAST_MODIFIED_XML}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                xml_escape(key),
                xml_escape(etag),
                bytes.len()
            ));
        }
    }
    xml.push_str("</ListBucketResult>");
    resp(200, xml)
}

fn route(st: &Mutex<Srv>, req: &Request) -> Response {
    if let Err(e) = verify(req) {
        return e;
    }
    let mut s = st.lock().unwrap_or_else(PoisonError::into_inner);
    let path = percent_decode(&req.path);
    let path = path.trim_start_matches('/');
    let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
    if bucket != "bkt" {
        return err(404, "NoSuchBucket");
    }
    let full = format!("{bucket}/{key}");
    s.counter += 1;
    let etag = format!("\"e{}\"", s.counter);
    match (req.method.as_str(), key.is_empty()) {
        ("GET", true) if req.q("list-type") == Some("2") => xml_list(&s, bucket, req),
        ("HEAD", false) => match s.objects.get(&full) {
            Some((bytes, etag)) => Response {
                status: 200,
                headers: vec![
                    ("content-length".into(), bytes.len().to_string()),
                    ("etag".into(), etag.clone()),
                    ("last-modified".into(), LAST_MODIFIED_HTTP.into()),
                ],
                body: Vec::new(),
            },
            None => resp(404, ""),
        },
        ("GET", false) => {
            let Some((bytes, etag)) = s.objects.get(&full) else { return err(404, "NoSuchKey") };
            let Some(range) = req.header("range") else {
                return Response { status: 200, headers: vec![("etag".into(), etag.clone())], body: bytes.clone() };
            };
            let (a, b) = range.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
            let a: usize = a.parse().unwrap();
            if a >= bytes.len() {
                return err(416, "InvalidRange");
            }
            let b = b.parse::<u64>().unwrap().min(bytes.len() as u64 - 1) as usize;
            Response { status: 206, headers: vec![("content-range".into(), format!("bytes {a}-{b}/{}", bytes.len()))], body: bytes[a..=b].to_vec() }
        }
        ("PUT", false) if req.q("partNumber").is_some() => {
            let n: u32 = req.q("partNumber").unwrap().parse().unwrap();
            let Some(up) = s.uploads.get_mut(req.q("uploadId").unwrap_or_default()) else { return err(404, "NoSuchUpload") };
            up.1.insert(n, req.body.clone());
            Response { status: 200, headers: vec![("etag".into(), format!("\"p{n}-{}\"", req.body.len()))], body: Vec::new() }
        }
        ("PUT", false) if req.header("x-amz-copy-source").is_some() => {
            let src = percent_decode(req.header("x-amz-copy-source").unwrap());
            let Some(obj) = s.objects.get(src.trim_start_matches('/')).cloned() else { return err(404, "NoSuchKey") };
            s.objects.insert(full, (obj.0, etag.clone()));
            resp(200, format!("<CopyObjectResult><LastModified>{LAST_MODIFIED_XML}</LastModified><ETag>{}</ETag></CopyObjectResult>", xml_escape(&etag)))
        }
        ("PUT", false) => {
            if req.header("if-none-match") == Some("*") && s.objects.contains_key(&full) {
                return err(412, "PreconditionFailed");
            }
            s.objects.insert(full, (req.body.clone(), etag.clone()));
            Response { status: 200, headers: vec![("etag".into(), etag)], body: Vec::new() }
        }
        ("POST", false) if req.q("uploads").is_some() => {
            let id = format!("up{}", s.counter);
            s.uploads.insert(id.clone(), (full, BTreeMap::new()));
            resp(
                200,
                format!(
                    "<InitiateMultipartUploadResult><Bucket>bkt</Bucket><Key>{}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>",
                    xml_escape(key)
                ),
            )
        }
        ("POST", false) if req.q("uploadId").is_some() => {
            let id = req.q("uploadId").unwrap();
            let Some((target, parts)) = s.uploads.get(id).cloned() else { return err(404, "NoSuchUpload") };
            if target != full {
                return err(400, "InvalidRequest");
            }
            let doc = roxmltree::Document::parse(std::str::from_utf8(&req.body).unwrap()).unwrap();
            let mut data = Vec::new();
            for (expect, p) in (1u32..).zip(doc.descendants().filter(|n| n.tag_name().name() == "Part")) {
                let n: u32 = p.children().find(|c| c.tag_name().name() == "PartNumber").unwrap().text().unwrap().parse().unwrap();
                let tag = p.children().find(|c| c.tag_name().name() == "ETag").unwrap().text().unwrap();
                let Some(bytes) = parts.get(&n) else { return err(400, "InvalidPart") };
                if n != expect || tag != format!("\"p{n}-{}\"", bytes.len()) {
                    return err(400, "InvalidPart");
                }
                data.extend_from_slice(bytes);
            }
            s.uploads.remove(id);
            s.objects.insert(full, (data, etag.clone()));
            resp(
                200,
                format!("<CompleteMultipartUploadResult><Key>{}</Key><ETag>{}</ETag></CompleteMultipartUploadResult>", xml_escape(key), xml_escape(&etag)),
            )
        }
        ("DELETE", false) if req.q("uploadId").is_some() => match s.uploads.remove(req.q("uploadId").unwrap()) {
            Some(_) => resp(204, ""),
            None => err(404, "NoSuchUpload"),
        },
        ("DELETE", false) => {
            s.objects.remove(&full);
            resp(204, "")
        }
        _ => err(400, "NotImplemented"),
    }
}

// ---------------------------------------------------------------------------------------------
// The backend against the server

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

fn put(store: &S3, path: &str, bytes: &[u8]) -> crate::Result<()> {
    store.write(&p(path), &mut &bytes[..], bytes.len() as u64, &mut |_| true)
}

fn names(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|e| format!("{}{}", e.name(), if e.is_dir { "/" } else { "" })).collect()
}

#[test]
fn every_method_round_trips() {
    let srv = TestServer::start();
    let s = srv.store(SECRET);
    put(&s, "proj/a.rev", b"hello").unwrap();
    assert_eq!(srv.keys(), ["bkt/team/films/proj/a.rev"]);
    assert_eq!(names(&s.list(&RemotePath::root()).unwrap()), ["proj/"]);
    let e = s.stat(&p("proj/a.rev")).unwrap();
    assert_eq!((e.size, e.is_dir, e.modified), (5, false, Some(1_704_164_645)));
    assert!(e.etag.is_some());
    assert!(s.stat(&p("proj")).unwrap().is_dir);
    assert!(s.stat(&RemotePath::root()).unwrap().is_dir);
    assert!(matches!(s.stat(&p("nope")), Err(RemoteError::NotFound(_))));
    assert!(matches!(s.list(&p("nope")), Err(RemoteError::NotFound(_))));
    let listed = s.list(&p("proj")).unwrap();
    assert_eq!(names(&listed), ["a.rev"]);
    assert_eq!((listed[0].size, listed[0].modified), (5, Some(1_704_164_645)));
    assert_eq!(s.read_range(&p("proj/a.rev"), 1, 3).unwrap(), b"ell");
    assert_eq!(s.read_range(&p("proj/a.rev"), 3, 99).unwrap(), b"lo");
    assert_eq!(s.read_range(&p("proj/a.rev"), 99, 3).unwrap(), b"");
    assert_eq!(s.read_range(&p("proj/a.rev"), 0, 0).unwrap(), b"");
    assert!(matches!(s.read_range(&p("proj/x"), 0, 1), Err(RemoteError::NotFound(_))));
    assert!(matches!(s.read_range(&p("proj/a.rev"), 0, MAX_READ_BYTES + 1), Err(RemoteError::TooLarge(_))));
    assert_eq!(s.read_all(&p("proj/a.rev"), 5).unwrap(), b"hello");
    assert!(matches!(s.read_all(&p("proj/a.rev"), 4), Err(RemoteError::TooLarge(_))));
    // overwrite, empty file, unicode names
    put(&s, "proj/a.rev", b"hello again").unwrap();
    put(&s, "proj/empty", b"").unwrap();
    put(&s, "proj/sub dir/é & <x>.mov", b"x").unwrap();
    assert_eq!(names(&s.list(&p("proj")).unwrap()), ["a.rev", "empty", "sub dir/"]);
    assert_eq!(names(&s.list(&p("proj/sub dir")).unwrap()), ["é & <x>.mov"]);
    assert_eq!(s.read_all(&p("proj/sub dir/é & <x>.mov"), 10).unwrap(), b"x");
    // rename = copy + delete
    s.rename(&p("proj/a.rev"), &p("proj/b.rev")).unwrap();
    assert!(matches!(s.stat(&p("proj/a.rev")), Err(RemoteError::NotFound(_))));
    assert_eq!(s.read_all(&p("proj/b.rev"), 100).unwrap(), b"hello again");
    assert!(matches!(s.rename(&p("proj/zzz"), &p("proj/y")), Err(RemoteError::NotFound(_))));
    assert!(matches!(s.rename(&p("proj"), &p("proj2")), Err(RemoteError::Forbidden(_))));
    // create_new
    s.create_new(&p("proj/lock"), b"1").unwrap();
    assert!(matches!(s.create_new(&p("proj/lock"), b"2"), Err(RemoteError::AlreadyExists(_))));
    assert_eq!(s.read_all(&p("proj/lock"), 10).unwrap(), b"1");
    // make_dir: a marker, idempotent; listed as a folder even when empty
    s.make_dir(&p("proj/new/deeper")).unwrap();
    s.make_dir(&p("proj/new/deeper")).unwrap();
    s.make_dir(&RemotePath::root()).unwrap();
    assert!(srv.keys().contains(&"bkt/team/films/proj/new/deeper/".to_string()));
    assert_eq!(names(&s.list(&p("proj/new")).unwrap()), ["deeper/"]);
    assert!(s.list(&p("proj/new/deeper")).unwrap().is_empty());
    assert!(s.stat(&p("proj/new/deeper")).unwrap().is_dir);
    assert!(matches!(s.make_dir(&p("proj/b.rev")), Err(RemoteError::AlreadyExists(_))));
    // delete
    s.delete(&p("proj/b.rev")).unwrap();
    assert!(matches!(s.delete(&p("proj/b.rev")), Err(RemoteError::NotFound(_))));
    assert!(matches!(s.delete(&p("proj")), Err(RemoteError::Forbidden(_))));
    assert!(matches!(s.write(&RemotePath::root(), &mut &b""[..], 0, &mut |_| true), Err(RemoteError::BadPath(_))));
    // nothing outside the prefix was touched
    assert!(srv.keys().iter().all(|k| k.starts_with("bkt/team/films/")));
}

#[test]
fn writes_send_exactly_size_bytes_and_report_progress() {
    let srv = TestServer::start();
    let s = srv.store(SECRET);
    assert!(matches!(s.write(&p("a"), &mut &b"abc"[..], 2, &mut |_| true), Err(RemoteError::Protocol(_))));
    assert!(matches!(s.write(&p("a"), &mut &b"abc"[..], 4, &mut |_| true), Err(RemoteError::Protocol(_))));
    assert!(matches!(s.write(&p("a"), &mut &b"x"[..], 0, &mut |_| true), Err(RemoteError::Protocol(_))));
    assert!(matches!(s.write(&p("a"), &mut &b"abc"[..], 3, &mut |_| false), Err(RemoteError::Cancelled)));
    assert!(srv.keys().is_empty(), "{:?}", srv.keys());
    let mut seen = Vec::new();
    s.write(&p("a"), &mut &b"abc"[..], 3, &mut |n| {
        seen.push(n);
        true
    })
    .unwrap();
    assert_eq!(seen.last(), Some(&3));
}

#[test]
fn listing_follows_continuation_tokens() {
    let srv = TestServer::start();
    srv.lock().page = 2;
    let s = srv.store(SECRET);
    for name in ["d/1", "d/2", "d/3", "d/4", "d/5", "d/s1/x", "d/s1/y", "d/s2/z", "other"] {
        put(&s, name, name.as_bytes()).unwrap();
    }
    assert_eq!(names(&s.list(&p("d")).unwrap()), ["1", "2", "3", "4", "5", "s1/", "s2/"]);
    assert_eq!(names(&s.list(&RemotePath::root()).unwrap()), ["d/", "other"]);
}

#[test]
fn wrong_secret_is_unauthorized() {
    let srv = TestServer::start();
    let s = srv.store("not the secret");
    assert!(matches!(s.list(&RemotePath::root()), Err(RemoteError::Unauthorized)));
    assert!(matches!(put(&s, "a", b"x"), Err(RemoteError::Unauthorized)));
    assert!(matches!(s.create_new(&p("a"), b"x"), Err(RemoteError::Unauthorized)));
    assert!(srv.keys().is_empty());
    let mut loc = location(&format!("http://{}", srv.addr));
    loc.user = "SOMEONEELSE".into();
    assert!(matches!(S3::new(&loc, SECRET).unwrap().list(&RemotePath::root()), Err(RemoteError::Unauthorized)));
}

#[test]
fn lock_files_work_across_two_machines() {
    let srv = TestServer::start();
    let (a, b) = (srv.store(SECRET), srv.store(SECRET));
    let me = |s: &str| lock::LockInfo { holder: "Ana".into(), machine: format!("pc-{s}"), session: s.into(), since: 1_700_000_000 };
    let proj = p("films/cut.rev");
    assert_eq!(lock::acquire(&a, &proj, &me("a")).unwrap(), lock::Acquire::Acquired);
    assert_eq!(lock::acquire(&b, &proj, &me("b")).unwrap(), lock::Acquire::HeldBy(me("a")));
    assert_eq!(lock::take_over(&b, &proj, &me("b")).unwrap(), lock::Acquire::Acquired);
    assert!(!lock::still_ours(&a, &proj, &me("a")).unwrap());
    assert!(lock::release(&b, &proj, &me("b")).unwrap());
    assert_eq!(lock::read(&a, &proj).unwrap(), None);
}

#[test]
fn multipart_upload_round_trips() {
    let srv = TestServer::start();
    let mut s = srv.store(SECRET);
    s.set_multipart_sizes(16, 8);
    let data: Vec<u8> = (0..43u8).collect();
    let mut seen = Vec::new();
    s.write(&p("big.mov"), &mut &data[..], data.len() as u64, &mut |n| {
        seen.push(n);
        true
    })
    .unwrap();
    assert_eq!(s.read_all(&p("big.mov"), 100).unwrap(), data);
    assert_eq!(seen.last(), Some(&43));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]));
    assert!(srv.lock().uploads.is_empty());
    // wrong sizes and cancel abort the upload, leaving nothing
    assert!(matches!(s.write(&p("b2"), &mut &data[..], 50, &mut |_| true), Err(RemoteError::Protocol(_))));
    assert!(matches!(s.write(&p("b2"), &mut &data[..], 40, &mut |_| true), Err(RemoteError::Protocol(_))));
    assert!(matches!(s.write(&p("b2"), &mut &data[..], 43, &mut |n| n < 20), Err(RemoteError::Cancelled)));
    assert!(srv.lock().uploads.is_empty());
    assert!(matches!(s.stat(&p("b2")), Err(RemoteError::NotFound(_))));
}

#[test]
fn an_interrupted_multipart_upload_resumes() {
    let srv = TestServer::start();
    let mut s = srv.store(SECRET);
    s.set_multipart_sizes(16, 8);
    let data: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(7)).collect();
    srv.lock().drop_part = Some((3, PART_ATTEMPTS as usize));
    let fail = s.write_resumable(&p("clip.mov"), &mut &data[..], 40, &mut |_| true, None).unwrap_err();
    assert!(fail.error.is_transient(), "{:?}", fail.error);
    let state = fail.resume.expect("a resumable state");
    assert_eq!((state.parts.len(), state.bytes_done()), (2, 16));
    assert!(matches!(s.stat(&p("clip.mov")), Err(RemoteError::NotFound(_))), "nothing at the key yet");
    // the state survives serde (an app restart)
    let state: MultipartState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    // a state for another file or a damaged one is refused
    assert!(s.write_resumable(&p("other"), &mut &data[16..], 40, &mut |_| true, Some(state.clone())).is_err());
    let mut bad = state.clone();
    bad.parts[1].number = 7;
    assert!(s.write_resumable(&p("clip.mov"), &mut &data[16..], 40, &mut |_| true, Some(bad)).is_err());
    let mut seen = Vec::new();
    s.write_resumable(
        &p("clip.mov"),
        &mut &data[16..],
        40,
        &mut |n| {
            seen.push(n);
            true
        },
        Some(state),
    )
    .unwrap();
    assert_eq!(s.read_all(&p("clip.mov"), 100).unwrap(), data);
    assert!(seen.first().is_some_and(|n| *n > 16) && seen.last() == Some(&40));
    assert!(srv.lock().uploads.is_empty());
    // through the trait, a dropped connection aborts the upload (nothing left behind)
    srv.lock().drop_part = Some((2, PART_ATTEMPTS as usize));
    assert!(put(&s, "clip2.mov", &data).unwrap_err().is_transient());
    assert!(srv.lock().uploads.is_empty());
    assert!(matches!(s.stat(&p("clip2.mov")), Err(RemoteError::NotFound(_))));
}

fn raw_reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nconnection: close\r\ncontent-length: {}\r\n{headers}\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

#[test]
fn hostile_replies_never_panic() {
    let srv = TestServer::start();
    let s = srv.store(SECRET);
    // keys with `..`, outside the prefix, with empty segments, control characters: skipped
    let xml = "<ListBucketResult><IsTruncated>false</IsTruncated>\
        <Contents><Key>team/films/../secret</Key><Size>1</Size></Contents>\
        <Contents><Key>team/films/ok.mov</Key><Size>7</Size></Contents>\
        <Contents><Key>other/x</Key><Size>1</Size></Contents>\
        <Contents><Key>team/filmsx</Key><Size>1</Size></Contents>\
        <Contents><Key>team/films//x</Key><Size>1</Size></Contents>\
        <Contents><Key>team/films/a/../../b</Key><Size>1</Size></Contents>\
        <Contents><Key>team/films/bad&#9;name</Key><Size>1</Size></Contents>\
        <Contents><Key>team/films/deep/x</Key><Size>1</Size></Contents>\
        <CommonPrefixes><Prefix>team/films/../</Prefix></CommonPrefixes>\
        <CommonPrefixes><Prefix>team/films/dir/</Prefix></CommonPrefixes>\
        <CommonPrefixes><Prefix>elsewhere/</Prefix></CommonPrefixes>\
        </ListBucketResult>";
    srv.lock().raw_next = Some(raw_reply("200 OK", "", xml.as_bytes()));
    assert_eq!(names(&s.list(&RemotePath::root()).unwrap()), ["dir/", "ok.mov"]);
    // malformed XML
    srv.lock().raw_next = Some(raw_reply("200 OK", "", b"<ListBucketResult><Contents>"));
    let r = s.list(&RemotePath::root());
    assert!(matches!(r, Err(RemoteError::Protocol(_))), "{r:?}");
    // a truncated listing that never moves on
    srv.lock().raw_next = Some(raw_reply("200 OK", "", b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>"));
    assert!(matches!(s.list(&RemotePath::root()), Err(RemoteError::Protocol(_))));
    // a huge listing
    let mut huge = b"<ListBucketResult>".to_vec();
    huge.resize((MAX_LIST_REPLY_BYTES + 10) as usize, b' ');
    srv.lock().raw_next = Some(raw_reply("200 OK", "", &huge));
    assert!(matches!(s.list(&RemotePath::root()), Err(RemoteError::TooLarge(_))));
    // a range reply longer than asked
    put(&s, "f", b"0123456789").unwrap();
    srv.lock().raw_next = Some(raw_reply("206 Partial Content", "content-range: bytes 2-3/10\r\n", b"23456789"));
    assert!(matches!(s.read_range(&p("f"), 2, 2), Err(RemoteError::Protocol(_))));
    // a range reply for the wrong bytes
    srv.lock().raw_next = Some(raw_reply("206 Partial Content", "content-range: bytes 0-1/10\r\n", b"01"));
    assert!(matches!(s.read_range(&p("f"), 2, 2), Err(RemoteError::Protocol(_))));
    // a server that ignores ranges
    srv.lock().raw_next = Some(raw_reply("200 OK", "", b"0123456789"));
    assert_eq!(s.read_range(&p("f"), 2, 3).unwrap(), b"234");
    // a body shorter than its content-length, garbage, an error inside a 200
    srv.lock().raw_next = Some(b"HTTP/1.1 206 X\r\ncontent-length: 100\r\n\r\nabc".to_vec());
    assert!(s.read_range(&p("f"), 0, 100).unwrap_err().is_transient());
    srv.lock().raw_next = Some(b"\x00\xffgarbage\r\n\r\n".to_vec());
    assert!(s.stat(&p("f")).is_err());
    srv.lock().raw_next = Some(raw_reply("500 Oops", "", b"<Error><Code>InternalError</Code></Error>"));
    assert!(s.stat(&p("f")).unwrap_err().is_transient());
    srv.lock().raw_next = Some(raw_reply("400 Bad", "", &vec![b'<'; 1 << 20]));
    assert!(matches!(s.stat(&p("f")), Err(RemoteError::Protocol(_))));
    srv.lock().raw_next = Some(raw_reply("301 Moved", "location: http://elsewhere/\r\n", b""));
    assert!(matches!(s.stat(&p("f")), Err(RemoteError::Protocol(_))));
    // an upload id that is missing or absurd
    let mut m = srv.store(SECRET);
    m.set_multipart_sizes(4, 4);
    srv.lock().raw_next = Some(raw_reply("200 OK", "", b"<InitiateMultipartUploadResult></InitiateMultipartUploadResult>"));
    assert!(matches!(put(&m, "g", b"0123456789"), Err(RemoteError::Protocol(_))));
    // the server still works afterwards
    assert_eq!(s.read_all(&p("f"), 100).unwrap(), b"0123456789");
}

#[test]
fn a_part_that_fails_once_is_retried_from_memory() {
    let srv = TestServer::start();
    let mut s = srv.store(SECRET);
    s.set_multipart_sizes(16, 8);
    let data: Vec<u8> = (0..40u8).collect();
    srv.lock().drop_part = Some((2, PART_ATTEMPTS as usize - 1));
    put(&s, "c.mov", &data).unwrap();
    assert_eq!(s.read_all(&p("c.mov"), 100).unwrap(), data);
    let log = srv.lock().log.clone();
    assert_eq!(log.iter().filter(|l| l.starts_with("POST uploads")).count(), 1, "{log:?}");
}

#[test]
fn lost_replies_are_checked_not_blindly_resent() {
    let srv = TestServer::start();
    let mut s = srv.store(SECRET);
    // create_new: the server stored our lock but the reply was lost
    srv.lock().lose_reply = Some(LoseReply::CreateNew);
    s.create_new(&p("lock"), b"mine").unwrap();
    assert_eq!(s.read_all(&p("lock"), 10).unwrap(), b"mine");
    // … while someone else's lock still wins
    srv.lock().lose_reply = Some(LoseReply::CreateNew);
    s.create_new(&p("lock2"), b"theirs").unwrap();
    assert!(matches!(s.create_new(&p("lock2"), b"mine"), Err(RemoteError::AlreadyExists(_))));
    // CompleteMultipartUpload: completed, reply lost → a HEAD confirms it, no second POST
    s.set_multipart_sizes(16, 8);
    let data: Vec<u8> = (0..40u8).collect();
    srv.lock().log.clear();
    srv.lock().lose_reply = Some(LoseReply::Complete);
    put(&s, "m.mov", &data).unwrap();
    assert_eq!(s.read_all(&p("m.mov"), 100).unwrap(), data);
    let log = srv.lock().log.clone();
    assert_eq!(log.iter().filter(|l| l.starts_with("POST uploadId")).count(), 1, "{log:?}");
    assert_eq!(log.iter().filter(|l| l.starts_with("POST uploads")).count(), 1, "{log:?}");
}

#[test]
fn folders_are_not_written_over() {
    let srv = TestServer::start();
    let s = srv.store(SECRET);
    put(&s, "proj/a", b"1").unwrap();
    s.make_dir(&p("empty")).unwrap();
    for dir in ["proj", "empty"] {
        assert!(matches!(put(&s, dir, b"x"), Err(RemoteError::AlreadyExists(_))), "{dir}");
        assert!(matches!(s.create_new(&p(dir), b"x"), Err(RemoteError::AlreadyExists(_))), "{dir}");
        assert!(matches!(s.rename(&p("proj/a"), &p(dir)), Err(RemoteError::AlreadyExists(_))), "{dir}");
    }
    assert_eq!(s.read_all(&p("proj/a"), 10).unwrap(), b"1");
    assert!(srv.keys().iter().all(|k| k.ends_with("proj/a") || k.ends_with("empty/")), "{:?}", srv.keys());
}

#[test]
fn rename_encodes_the_copy_source() {
    let srv = TestServer::start();
    let s = srv.store(SECRET);
    put(&s, "a b/ç & é+%.mov", b"v").unwrap();
    s.rename(&p("a b/ç & é+%.mov"), &p("x y/ü?#.mov")).unwrap();
    assert_eq!(s.read_all(&p("x y/ü?#.mov"), 10).unwrap(), b"v");
    assert!(matches!(s.stat(&p("a b/ç & é+%.mov")), Err(RemoteError::NotFound(_))));
    assert_eq!(srv.keys(), ["bkt/team/films/x y/ü?#.mov"]);
}

#[test]
fn the_test_server_refuses_chunked_uploads() {
    let srv = TestServer::start();
    for raw in
        [&b"PUT /bkt/x HTTP/1.1\r\nhost: h\r\ntransfer-encoding: chunked\r\n\r\n1\r\nx\r\n0\r\n\r\n"[..], b"POST /bkt/x?uploads HTTP/1.1\r\nhost: h\r\n\r\n"]
    {
        let mut c = TcpStream::connect(srv.addr).unwrap();
        c.write_all(raw).unwrap();
        let mut line = String::new();
        BufReader::new(&c).read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 411"), "{line}");
    }
    assert!(srv.keys().is_empty());
}
