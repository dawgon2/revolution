//! AI5 (AI-PLAN Phase AI5): rate an Assistant answer and share it as an example, only when the
//! user asks (FORK-PLAN §1a.84: nothing in the background, FilmCraft has no server).
//!
//! `coach.feedback` keeps 👍 / 👎 and "what should it have said?" with the answer.
//! `coach.shareExample` builds ONE text, the preview, and that exact text is what the GitHub link
//! carries or the file holds. Project / clip / bin / sequence names, the user's name, paths and
//! network addresses are taken out first (the public-snapshot rules), and the grade's settings,
//! the picture facts and a small frame are added only when ticked.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::coach_assist::{AssistAnswer, AssistContext};
use crate::commands::{CommandSpec, always, bad};
use crate::{Result, Session};

/// Where "Open on GitHub" files examples (issues; the user submits with their own account).
pub const EXAMPLES_REPO: &str = "dawgon2/revolution"; // public, so anyone can file one (FORK-PLAN §1a.103)
/// The label those issues carry.
pub const EXAMPLES_LABEL: &str = "assistant-example";
/// Longest GitHub link: longer examples are saved and the issue says to paste the file.
pub const MAX_URL_CHARS: usize = 7_500;
/// Longest "what should it have said?".
pub const MAX_CORRECTION_CHARS: usize = 2_000;
/// Pixels of the frame that may go with an example (≈ 480 × 270).
pub const FRAME_PIXELS: u64 = 480 * 270;
/// Whether this build has the owner's Send to my training inbox (`route: inbox`).
pub const INBOX: bool = cfg!(feature = "fork-inbox"); // FORK-ONLY (AI5 item 8)
/// Records kept (the same number of answers `coach.assistResult` keeps).
const MAX_RECORDS: usize = 64;
/// Shortest name taken out of the text (shorter ones would eat ordinary words).
const MIN_NAME_CHARS: usize = 3;
/// Most private names collected from one project; past it nothing is shared ([`Private::overflow`]).
pub const MAX_NAMES: usize = 200_000;
/// Names shorter than this (in characters) only count as whole words ("dan" isn't taken out of "dance").
const WHOLE_WORD_CHARS: usize = 5;

/// A 👍 or 👎.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rating {
    Up,
    Down,
}

/// What was asked, with the context the answer was written in.
#[derive(Clone, Debug, Default)]
pub struct ShareRecord {
    pub question: String,
    pub page: Option<String>,
    pub grade_facts: Option<String>,
    pub picture_facts: Option<String>,
    pub rating: Option<Rating>,
    pub correction: Option<String>,
    /// The project's private names when the question was asked (a project closed or a clip
    /// renamed since then still can't leak through the question or the facts).
    pub private: Option<std::sync::Arc<Private>>,
}

impl ShareRecord {
    pub fn new(question: &str, ctx: &AssistContext) -> ShareRecord {
        ShareRecord {
            question: question.to_string(),
            page: ctx.page.clone(),
            grade_facts: ctx.grade_facts_json.clone(),
            picture_facts: ctx.picture_facts.clone(),
            rating: None,
            correction: None,
            private: None,
        }
    }
}

/// The records of recent questions, by assistant id.
#[derive(Debug, Default)]
pub struct ShareRecords(Mutex<BTreeMap<u64, ShareRecord>>);

impl ShareRecords {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<u64, ShareRecord>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn insert(&self, id: u64, r: ShareRecord) {
        let mut m = self.lock();
        while m.len() >= MAX_RECORDS {
            let Some(oldest) = m.keys().next().copied() else { break };
            m.remove(&oldest);
        }
        m.insert(id, r);
    }

    pub fn get(&self, id: u64) -> Option<ShareRecord> {
        self.lock().get(&id).cloned()
    }

    /// Sets the rating and correction; false: unknown id.
    pub fn rate(&self, id: u64, rating: Option<Rating>, correction: Option<String>) -> bool {
        match self.lock().get_mut(&id) {
            Some(r) => {
                r.rating = rating;
                r.correction = correction;
                true
            }
            None => false,
        }
    }
}

/// What the user ticked to go along (all off by default; the frame is never pre-ticked).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    pub grade: bool,
    pub picture: bool,
    pub frame: bool,
}

/// Names that must not leave the computer (lower-cased), with their stand-ins.
#[derive(Clone, Debug, Default)]
pub struct Private {
    names: Vec<(String, &'static str)>,
    seen: std::collections::HashSet<String>,
    /// More than [`MAX_NAMES`] names: nothing can be shared, since some would go unchecked.
    pub overflow: bool,
}

/// Folder names that say nothing about the user (left in place when a path's folders are collected).
const PLAIN_FOLDERS: [&str; 16] = [
    "home",
    "users",
    "media",
    "mnt",
    "run",
    "tmp",
    "volumes",
    "videos",
    "pictures",
    "documents",
    "desktop",
    "downloads",
    "music",
    "movies",
    "footage",
    "projects",
];

impl Private {
    /// The project's, its bins', items', clips', tracks', markers' and grade nodes' names, item
    /// metadata, media and project paths with their folders, and the user's and computer's names.
    pub fn of(s: &Session) -> Private {
        let mut p = Private::default();
        let env = |k: &str| std::env::var(k).ok();
        p.add(&s.project.name, "<project>");
        if let Some(path) = &s.path {
            p.add_path(path);
        }
        for k in ["USER", "USERNAME", "LOGNAME"] {
            if let Some(v) = env(k) {
                p.add(&v, "<user>");
            }
        }
        for k in ["HOME", "USERPROFILE"] {
            if let Some(v) = env(k) {
                p.add_path(&v);
            }
        }
        for k in ["HOSTNAME", "COMPUTERNAME"] {
            if let Some(v) = env(k) {
                p.add(&v, "<computer>");
            }
        }
        if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
            p.add(h.trim(), "<computer>");
        }
        let mut bins = vec![&s.project.root];
        while let Some(b) = bins.pop() {
            if p.overflow {
                break;
            }
            p.add(&b.name, "<bin>");
            for c in &b.children {
                if let filmcraft_project::BinEntry::Bin(sub) = c {
                    bins.push(sub);
                }
            }
        }
        // Gallery stills: an imported reference picture is named after its file
        for st in &s.project.stills {
            if p.overflow {
                break;
            }
            p.add(&st.label, "<still>");
            p.add(&st.clip_name, "<clip>");
        }
        for it in s.project.items.values() {
            if p.overflow {
                break;
            }
            p.add(&it.name, "<clip>");
            for v in it.metadata.values() {
                p.add(v, "<clip>");
            }
            match &it.kind {
                filmcraft_project::ItemKind::Media(m) => {
                    for r in std::iter::once(&m.media).chain(m.proxy.iter()) {
                        if let filmcraft_project::MediaRef::File { path } = r {
                            p.add_path(path);
                        }
                    }
                    for mk in &m.markers {
                        p.add(&mk.name, "<marker>");
                        p.add(&mk.comment, "<marker>");
                    }
                }
                filmcraft_project::ItemKind::Sequence(q) => {
                    for mk in &q.markers {
                        p.add(&mk.name, "<marker>");
                        p.add(&mk.comment, "<marker>");
                    }
                    for t in q.video_tracks.iter().chain(&q.audio_tracks) {
                        p.add(&t.name, "<track>");
                        for c in &t.items {
                            p.add(&c.name, "<clip>");
                            for n in c.grade.iter().flat_map(|g| &g.nodes) {
                                p.add(&n.label, "<node>");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        p
    }

    /// Adds a name (trimmed, compared without case; shorter than [`MIN_NAME_CHARS`]: skipped).
    pub fn add(&mut self, name: &str, stand_in: &'static str) {
        let n = name.trim().to_lowercase();
        if n.chars().count() < MIN_NAME_CHARS || self.seen.contains(&n) {
            return;
        }
        if self.names.len() >= MAX_NAMES {
            self.overflow = true;
            return;
        }
        self.seen.insert(n.clone());
        self.names.push((n, stand_in));
    }

    /// A path, its file name and stem, and its folder names (all but the plain ones).
    pub fn add_path(&mut self, path: &str) {
        self.add(path, "<path>");
        let p = Path::new(path);
        if let Some(f) = p.file_name().and_then(|f| f.to_str()) {
            self.add(f, "<file>");
        }
        if let Some(f) = p.file_stem().and_then(|f| f.to_str()) {
            self.add(f, "<file>");
        }
        for c in p.parent().into_iter().flat_map(Path::components) {
            if let std::path::Component::Normal(c) = c
                && let Some(c) = c.to_str()
                && !PLAIN_FOLDERS.contains(&c.to_lowercase().as_str())
            {
                self.add(c, "<folder>");
            }
        }
    }

    /// Adds every name of `other`.
    pub fn merge(&mut self, other: &Private) {
        self.overflow |= other.overflow;
        for (n, s) in &other.names {
            self.add(n, s);
        }
    }

    /// `text` with every private name, path, address and e-mail replaced by a stand-in. Names are
    /// found in one pass, without regard to case; overlapping finds are merged into one stand-in,
    /// and names shorter than [`WHOLE_WORD_CHARS`] only count as whole words.
    pub fn strip(&self, text: &str) -> String {
        strip_patterns(&self.strip_names(text))
    }

    fn strip_names(&self, text: &str) -> String {
        if self.names.is_empty() {
            return text.to_string();
        }
        // the lower-cased text, and for each of its bytes the original character's byte range
        let mut norm = String::with_capacity(text.len());
        let mut span: Vec<(usize, usize)> = Vec::with_capacity(text.len());
        for (i, c) in text.char_indices() {
            let end = i.saturating_add(c.len_utf8());
            for lc in c.to_lowercase() {
                norm.push(lc);
                span.extend(std::iter::repeat_n((i, end), lc.len_utf8()));
            }
        }
        let Ok(ac) = aho_corasick::AhoCorasick::new(self.names.iter().map(|(n, _)| n.as_str())) else {
            // can't search: give nothing away
            return "<withheld>".to_string();
        };
        let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let mut found: Vec<(usize, usize, &'static str)> = Vec::new();
        for m in ac.find_overlapping_iter(&norm) {
            let Some((name, stand_in)) = self.names.get(m.pattern().as_usize()) else { continue };
            if name.chars().count() < WHOLE_WORD_CHARS {
                let before = norm.get(..m.start()).and_then(|s| s.chars().next_back());
                let after = norm.get(m.end()..).and_then(|s| s.chars().next());
                if word(before) || word(after) {
                    continue;
                }
            }
            let (Some(a), Some(b)) = (span.get(m.start()), m.end().checked_sub(1).and_then(|e| span.get(e))) else { continue };
            found.push((a.0, b.1, stand_in));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut out = String::with_capacity(text.len());
        let mut at = 0usize;
        let mut i = 0usize;
        while let Some(&(start, mut end, stand_in)) = found.get(i) {
            i = i.saturating_add(1);
            if start < at {
                continue; // inside a range already replaced (its overlap was merged below)
            }
            while let Some(&(s2, e2, _)) = found.get(i) {
                if s2 >= end {
                    break;
                }
                end = end.max(e2);
                i = i.saturating_add(1);
            }
            out.push_str(text.get(at..start).unwrap_or_default());
            out.push_str(stand_in);
            at = end;
        }
        out.push_str(text.get(at..).unwrap_or_default());
        out
    }
}

/// Takes out, word by word: paths (`/…`, `~/…`, `C:\…`, `\\server`, `file:`, also after `=` or
/// `*` inside a word), URLs (`scheme://…`), IPv4 / IPv6 addresses, `*.local` / `*.lan` host
/// names and e-mail addresses.
pub fn strip_patterns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str(&strip_word(word));
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | '<' | '>' | '[' | ']' | '{' | '}' | ',' | ';') {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn strip_word(w: &str) -> String {
    // keep trailing sentence punctuation outside the stand-in
    let core = w.trim_end_matches(['.', ':', '!', '?', '*']);
    let tail = w.get(core.len()..).unwrap_or_default();
    let lower = core.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("file:") || lower.starts_with("smb:") {
        return format!("<address>{tail}");
    }
    // a path starting inside the word ("--out=/home/…", "**/home/…")
    if let Some(at) = path_start(core) {
        return format!("{}<path>{tail}", core.get(..at).unwrap_or_default());
    }
    // host:port
    let host = core
        .rsplit_once(':')
        .filter(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .map_or(lower.as_str(), |(h, _)| lower.get(..h.len()).unwrap_or(&lower));
    let stand_in = if is_ip(core) {
        Some("<address>")
    } else if core.contains('@') && core.split('@').nth(1).is_some_and(|d| d.contains('.')) {
        Some("<email>")
    } else if [".local", ".lan", ".home", ".internal"].iter().any(|s| host.ends_with(s) && host.len() > s.len()) {
        Some("<computer>")
    } else {
        None
    };
    match stand_in {
        Some(s) => format!("{s}{tail}"),
        None => w.to_string(),
    }
}

/// Where a path starts in `w`: at its start, or right after `=` / `*` / `:`-free prefix marks.
fn path_start(w: &str) -> Option<usize> {
    for (i, _) in w.char_indices() {
        let prev = w.get(..i).and_then(|s| s.chars().next_back());
        if (i == 0 || matches!(prev, Some('=' | '*')))
            && let Some(rest) = w.get(i..)
            && is_path(rest)
        {
            return Some(i);
        }
    }
    None
}

fn is_path(w: &str) -> bool {
    let b = w.as_bytes();
    let unix = w.starts_with('/') && w.get(1..).is_some_and(|r| r.contains('/') || r.chars().next().is_some_and(char::is_alphanumeric));
    let home = w.starts_with("~/");
    let windows = b.len() >= 3 && b.first().is_some_and(u8::is_ascii_alphabetic) && b.get(1) == Some(&b':') && matches!(b.get(2), Some(b'\\' | b'/'));
    let unc = w.starts_with("\\\\");
    unix || home || windows || unc
}

fn is_ip(w: &str) -> bool {
    let w = w.split('/').next().unwrap_or(w);
    // an IPv6 zone ("fe80::1%eth0") isn't part of the address
    let w = w.split('%').next().unwrap_or(w);
    let host = w.rsplit_once(':').filter(|(h, p)| h.contains('.') && p.chars().all(|c| c.is_ascii_digit())).map_or(w, |(h, _)| h);
    let quad = host.split('.').count() == 4 && host.split('.').all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()));
    // an IPv6 address: hex groups with "::" or all eight groups ("fe80::1", "2001:db8::…"); a
    // timecode (01:02:03:04) has only four
    let hex = w.chars().all(|c| c.is_ascii_hexdigit() || c == ':') && w.chars().any(|c| c.is_ascii_hexdigit());
    let v6 = hex && (w.contains("::") || w.matches(':').count() == 7);
    quad || v6
}

/// The answer's text, one line per step (the model's grounded lines, else the keyword coach's steps).
fn answer_lines(a: &AssistAnswer) -> Vec<String> {
    if let Some(g) = &a.grounded {
        return g.lines.iter().map(|l| if l.general { format!("(general advice) {}", l.text) } else { l.text.clone() }).collect();
    }
    a.coach.as_ref().map(|c| c.steps.iter().map(|s| s.text.clone()).collect()).unwrap_or_default()
}

/// The example as text: exactly what the GitHub issue body or the saved file holds.
pub fn example_text(r: &ShareRecord, a: Option<&AssistAnswer>, extras: Extras, frame: bool, private: &Private) -> String {
    let mut t = String::new();
    t.push_str("## Question\n\n");
    t.push_str(r.question.trim());
    t.push_str("\n\n## Answer\n\n");
    match a {
        None => t.push_str("(no answer yet)\n"),
        // REF: an answer about a reference picture may repeat what the picture shows: not shared
        Some(a) if a.reference.is_some() => t.push_str("(about a private reference picture: the answer isn't shared)\n"),
        Some(a) => {
            let by = if a.grounded.is_some() { "the Assistant" } else { "the keyword coach" };
            t.push_str(&format!("Answered by {by}.\n\n"));
            let lines = answer_lines(a);
            if lines.is_empty() {
                t.push_str("(empty)\n");
            }
            for l in lines {
                t.push_str(&format!("- {}\n", l.trim()));
            }
            if !a.seen.is_empty() {
                t.push_str("\nOn this frame (measured):\n");
                for l in &a.seen {
                    t.push_str(&format!("- {l}\n"));
                }
            }
            if !a.sources.is_empty() {
                t.push_str(&format!("\nGuide sections used: {}\n", a.sources.join(", ")));
            }
        }
    }
    t.push_str("\n## Rating\n\n");
    t.push_str(match r.rating {
        Some(Rating::Up) => "👍 helpful\n",
        Some(Rating::Down) => "👎 not helpful\n",
        None => "(not rated)\n",
    });
    if let Some(c) = r.correction.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        t.push_str(&format!("\nWhat it should have said:\n\n{c}\n"));
    }
    t.push_str(&format!("\n## Context\n\n- Revolution {}\n", env!("CARGO_PKG_VERSION")));
    if let Some(p) = &r.page {
        t.push_str(&format!("- Page: {p}\n"));
    }
    if extras.grade {
        t.push_str("\n## Grade settings\n\n");
        t.push_str(r.grade_facts.as_deref().unwrap_or("(no grade)"));
        t.push('\n');
    }
    if extras.picture {
        t.push_str("\n## Picture facts (measured)\n\n");
        t.push_str(r.picture_facts.as_deref().unwrap_or("(no picture)"));
        t.push('\n');
    }
    if extras.frame {
        t.push_str("\n## Frame\n\n");
        // the same words for every route, so the preview is what each one carries
        t.push_str(if frame {
            "A small frame goes with this example as an image: Save example… writes it next to the text; drag it into the issue.\n"
        } else {
            "(no frame: nothing under the playhead)\n"
        });
    }
    private.strip(&t)
}

/// `s` percent-encoded for a URL query value.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The issue title: "Assistant example: " and the question's first words.
fn issue_title(question: &str, private: &Private) -> String {
    const MAX: usize = 80;
    let q: String = private.strip(question.trim()).chars().take(MAX).collect();
    format!("Assistant example: {q}")
}

/// The pre-filled New-issue link, and whether the text was too long to go in it (then the body
/// says to paste the saved file).
pub fn issue_url(question: &str, text: &str, private: &Private) -> (String, bool) {
    let base =
        format!("https://github.com/{EXAMPLES_REPO}/issues/new?labels={}&title={}", url_encode(EXAMPLES_LABEL), url_encode(&issue_title(question, private)));
    let full = format!("{base}&body={}", url_encode(text));
    if full.len() <= MAX_URL_CHARS {
        return (full, false);
    }
    let note = "This example was too long for a link. Use Save example… in Revolution and paste (or drag) the saved file here.";
    (format!("{base}&body={}", url_encode(note)), true)
}

/// A file stem not yet used in `dir` for either the text or the frame:
/// `assistant-example-<id>[-n]`; `None` after 999 tries.
fn free_stem(dir: &Path, id: u64) -> Option<String> {
    (0u32..1000)
        .map(|n| if n == 0 { format!("assistant-example-{id}") } else { format!("assistant-example-{id}-{n}") })
        .find(|stem| !dir.join(format!("{stem}.md")).exists() && !dir.join(format!("{stem}.png")).exists())
}

/// The frame under the playhead as a small PNG.
fn frame_png(s: &Session) -> Option<Vec<u8>> {
    let (rgb, w, h) = crate::grade_tools::program_frame_rgb(s, FRAME_PIXELS)?;
    let bytes: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let img = image::RgbImage::from_raw(u32::try_from(w).ok()?, u32::try_from(h).ok()?, bytes)?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// A check value of the previewed text (and frame): a share asked with `expect` goes ahead only
/// if what it would send is still exactly what was previewed.
fn preview_hash(text: &str, frame: Option<&[u8]>) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    frame.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn id_param(p: &Value, cmd: &str) -> Result<u64> {
    p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "need `id` (from coach.assist)"))
}

fn feedback_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "coach.feedback";
    let id = id_param(p, CMD)?;
    let rating = match p.get("rating").and_then(Value::as_str) {
        None | Some("none") => None,
        Some("up") => Some(Rating::Up),
        Some("down") => Some(Rating::Down),
        Some(_) => return Err(bad(CMD, "`rating` is `up`, `down` or `none`")),
    };
    let correction =
        p.get("correction").and_then(Value::as_str).map(|c| c.chars().take(MAX_CORRECTION_CHARS).collect::<String>()).filter(|c| !c.trim().is_empty());
    if !s.assist.records.rate(id, rating, correction) {
        return Err(bad(CMD, format!("no question {id} (answers are kept for the last {MAX_RECORDS} questions)")));
    }
    Ok(json!({"id": id, "rated": true}))
}

fn share_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "coach.shareExample";
    let id = id_param(p, CMD)?;
    let record = s.assist.records.get(id).ok_or_else(|| bad(CMD, format!("no question {id}")))?;
    let tick = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
    let extras = Extras { grade: tick("grade"), picture: tick("picture"), frame: tick("frame") };
    let answer = s.assist.result(id).flatten();
    let route = p.get("route").and_then(Value::as_str).unwrap_or("preview");
    let frame = extras.frame.then(|| frame_png(s)).flatten();
    #[cfg(feature = "fork-inbox")] // FORK-ONLY (AI5 item 8: the owner's local training inbox, unstripped)
    if route == "inbox" {
        return crate::fork_inbox::send(s, p, id, extras, frame).map_err(|e| bad(CMD, e));
    }
    // the names of the project as it is now and as it was when the question was asked
    let mut private = Private::of(s);
    if let Some(then) = &record.private {
        private.merge(then);
    }
    if private.overflow {
        return Err(bad(CMD, format!("this project has more than {MAX_NAMES} names to keep private, too many to check: nothing was shared")));
    }
    let text = example_text(&record, answer.as_ref(), extras, frame.is_some(), &private);
    let hash = preview_hash(&text, frame.as_deref());
    if route != "preview"
        && let Some(want) = p.get("expect").and_then(Value::as_str)
        && want != hash
    {
        return Err(bad(CMD, "the example changed since the preview (the frame under the playhead moved?): look at the preview again"));
    }
    match route {
        "preview" => Ok(json!({"text": text, "frame": frame.is_some(), "hash": hash})),
        "github" => {
            if !tick("consent") {
                return Err(bad(CMD, "tick that you agree to share it publicly first (`consent: true`)"));
            }
            let (url, too_long) = issue_url(&record.question, &text, &private);
            Ok(json!({"text": text, "url": url, "tooLong": too_long, "frame": frame.is_some(), "hash": hash}))
        }
        "file" => {
            let folder = p.get("folder").and_then(Value::as_str).ok_or_else(|| bad(CMD, "need `folder` for `route: file`"))?;
            let dir = Path::new(folder);
            if !dir.is_dir() {
                return Err(bad(CMD, "that folder doesn't exist"));
            }
            let stem = free_stem(dir, id).ok_or_else(|| bad(CMD, "that folder already holds 1 000 examples of this answer"))?;
            // the frame first: a text that names a frame never exists without it
            let frame_path = match &frame {
                Some(png) => {
                    let fp = dir.join(format!("{stem}.png"));
                    filmcraft_format::atomic_write(&fp, png).map_err(|e| bad(CMD, format!("couldn't save the frame: {e}")))?;
                    Some(fp.display().to_string())
                }
                None => None,
            };
            let path = dir.join(format!("{stem}.md"));
            filmcraft_format::atomic_write(&path, text.as_bytes()).map_err(|e| bad(CMD, format!("couldn't save the example: {e}")))?;
            Ok(json!({"text": text, "path": path.display().to_string(), "framePath": frame_path, "hash": hash}))
        }
        _ => Err(bad(CMD, "`route` is `preview`, `github` or `file`")),
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "coach.feedback",
            label: "Rate the Assistant's Answer",
            menu: &[],
            shortcut: None,
            params: r#"{"id":int,"rating":str?,"correction":str?}"#,
            enabled: always,
            run: feedback_cmd,
            journal: false,
        },
        CommandSpec {
            id: "coach.shareExample",
            label: "Help Improve the Assistant",
            menu: &[],
            shortcut: None,
            params: r#"{"id":int,"route":str?,"grade":bool?,"picture":bool?,"frame":bool?,"consent":bool?,"folder":str?,"expect":str?,"conversation":[int]?}"#,
            enabled: always,
            run: share_cmd,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "coach_share_tests.rs"]
mod tests;
