//! Colour stills and the wipe / split comparison (`stills.*`, `compare.*`; GRADE-PLAN G1.5).
//!
//! A still is a frame grabbed from the Program monitor plus the grade that made it. Its metadata
//! lives in the project ([`filmcraft_project::Still`]); its picture is a PNG of at most 1920 px
//! width in the stills folder (next to the project file, or a per-session temp folder for an
//! unsaved project). A missing picture is re-rendered from the still's sequence time when the
//! sequence still exists, so a still always keeps working for its grade.
//!
//! The comparison state ([`CompareState`]) is editor state (not undoable, not saved): which still
//! the Program monitor draws against the live frame, and how.

use std::path::{Path, PathBuf};

use filmcraft_project::still::{MAX_STILL_LABEL, MAX_STILL_WIDTH, MAX_STILLS};
use filmcraft_project::{ClipId, Still};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, has_seq, str_p};
use crate::grade::{edit_grades, grade_targets};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// The widest picture file stills decode (stills are stored at most [`MAX_STILL_WIDTH`] wide).
const MAX_DECODE_DIM: u32 = 4096;
/// The largest picture file read, in bytes.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// The widest / tallest reference picture `stills.import` decodes (a 6.2K camera JPEG fits).
const MAX_IMPORT_DIM: u32 = 16_384;
/// The most bytes decoding one reference picture may allocate.
const MAX_IMPORT_ALLOC: u64 = 512 << 20;

/// How the Program monitor compares the live frame with a still.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompareMode {
    /// No comparison.
    #[default]
    Off,
    /// The still covers one side of a divider line, the live frame the other.
    Wipe,
    /// Side by side: the still's half and the live frame's matching half.
    Split,
}

/// The comparison state (`compare.set` / `compare.get`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompareState {
    /// The still compared against.
    pub still: Option<u64>,
    pub mode: CompareMode,
    /// Divider position, 0…1 from the left (or top when `vertical`).
    pub position: f32,
    /// Divide top / bottom instead of left / right.
    pub vertical: bool,
}

impl Default for CompareState {
    fn default() -> Self {
        Self { still: None, mode: CompareMode::Off, position: 0.5, vertical: false }
    }
}

impl CompareState {
    /// Whether a comparison is showing.
    pub fn active(&self) -> bool {
        self.mode != CompareMode::Off && self.still.is_some()
    }
}

/// A still's picture, decoded: straight sRGB RGBA8.
pub struct StillPicture {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: Run,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("stills.grab", "Grab Still", r#"{"clip":id?,"label":str?}"#, has_seq, grab, true),
        spec("stills.import", "Import Reference Still", r#"{"path":path,"label":str?}"#, always, import, true),
        spec("stills.list", "List Stills", "{}", always, list, false),
        spec("stills.apply", "Apply Still Grade", r#"{"still":id,"clips":[id]?}"#, has_seq, apply, true),
        spec("stills.delete", "Delete Still", r#"{"still":id}"#, always, delete, true),
        spec("stills.label", "Label Still", r#"{"still":id,"label":str}"#, always, label, true),
        spec(
            "compare.set",
            "Set Still Comparison",
            r#"{"still":id|null?,"mode":"off"|"wipe"|"split"?,"position":0..1?,"vertical":bool?}"#,
            always,
            compare_set,
            false,
        ),
        spec("compare.get", "Get Still Comparison", "{}", always, compare_get, false),
    ]
}

fn clean_label(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).take(MAX_STILL_LABEL).collect()
}

fn still_id(p: &Value, cmd: &str) -> Result<u64> {
    p.get("still").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "need `still` (a still id)"))
}

fn find(s: &Session, id: u64, cmd: &str) -> Result<Still> {
    s.project.stills.iter().find(|x| x.id == id).cloned().ok_or_else(|| bad(cmd, format!("no still {id}")))
}

impl Session {
    /// Folders a still's picture may live in, best first: where the saved project keeps its
    /// stills, then wherever pictures were last written, then the session's temp folder.
    pub fn stills_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = Vec::new();
        let all = self
            .path
            .as_deref()
            .map(|p| crate::project_tools::stills_dir_for(&self.project, p))
            .into_iter()
            .chain(self.state.stills_at.clone())
            .chain(self.state.stills_tmp.clone());
        for d in all {
            if !v.contains(&d) {
                v.push(d);
            }
        }
        v
    }

    /// The folder new pictures go to (made when missing); `None` on the web, which has no files
    /// or when it cannot be made.
    fn stills_write_dir(&mut self) -> Option<PathBuf> {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        let dir = match self.path.as_deref().map(|p| crate::project_tools::stills_dir_for(&self.project, p)) {
            Some(d) => d,
            None => match self.state.stills_tmp.clone() {
                Some(d) => d,
                None => {
                    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                    let base = self.media_cache_dir().map(|c| c.join("Stills")).unwrap_or_else(|| crate::temp_dir().join("FilmCraft Stills"));
                    let d = base.join(format!("untitled-{}-{nanos:x}", std::process::id()));
                    self.state.stills_tmp = Some(d.clone());
                    d
                }
            },
        };
        std::fs::create_dir_all(&dir).ok()?;
        self.state.stills_at = Some(dir.clone());
        Some(dir)
    }

    /// Keep the stills' pictures with the saved project: an unsaved project's temp folder is
    /// moved to the project's stills folder, and a Save As copies the previous folder. Called
    /// after the project's path changes (Save, Save As, Scratch Disks).
    pub fn stills_follow_path(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if cfg!(target_arch = "wasm32") {
            return;
        }
        let to = crate::project_tools::stills_dir_for(&self.project, &path);
        let Some(from) = self.state.stills_at.clone() else { return };
        if from == to {
            return;
        }
        let ours = self.state.stills_tmp.as_ref() == Some(&from);
        if std::fs::create_dir_all(&to).is_err() {
            return;
        }
        let mut all_moved = true;
        if let Ok(rd) = std::fs::read_dir(&from) {
            for e in rd.flatten() {
                let name = e.file_name();
                if !name.to_string_lossy().starts_with("still-") {
                    continue;
                }
                let dest = to.join(&name);
                let ok = if ours {
                    std::fs::rename(e.path(), &dest).is_ok() || (std::fs::copy(e.path(), &dest).is_ok() && std::fs::remove_file(e.path()).is_ok())
                } else {
                    std::fs::copy(e.path(), &dest).is_ok()
                };
                all_moved &= ok;
            }
        }
        if ours && all_moved {
            let _ = std::fs::remove_dir(&from);
            self.state.stills_tmp = None;
        }
        self.state.stills_at = Some(to);
    }

    /// Delete the temp folder of an unsaved project's stills (when the project closes).
    pub fn stills_discard_temp(&mut self) {
        if let Some(t) = self.state.stills_tmp.take()
            && !cfg!(target_arch = "wasm32")
        {
            let _ = std::fs::remove_dir_all(t);
        }
        self.state.stills_at = None;
    }

    /// A still's picture: its stored file, else a re-render of its sequence time (without its
    /// grade if the clip's has changed since). `None` for an unknown still or when neither works.
    pub fn still_picture(&self, id: u64) -> Option<StillPicture> {
        let st = self.project.stills.iter().find(|x| x.id == id)?;
        for d in self.stills_dirs() {
            if let Some(pic) = read_picture(&d.join(st.file_name())) {
                return Some(pic);
            }
        }
        let seq = st.sequence.filter(|q| self.project.sequence(*q).is_some())?;
        let scale = fit_scale(self.project.sequence(seq)?.settings.width);
        let provider = self.media.provider(self.project.clone(), self.services.clone());
        let opts = filmcraft_render::RenderOptions { scale, captions: true, mattes: self.magic_mask.token(), ..Default::default() };
        let img = filmcraft_render::render_sequence(&self.project, seq, st.time.max(filmcraft_time::Tick::ZERO), opts, &provider);
        let (w, h) = (u32::try_from(img.w).ok()?, u32::try_from(img.h).ok()?);
        Some(StillPicture { w, h, rgba: img.over_black_rgba8() })
    }
}

/// The render scale that keeps a frame `width` px wide within [`MAX_STILL_WIDTH`].
fn fit_scale(width: u32) -> f32 {
    if width <= MAX_STILL_WIDTH { 1.0 } else { MAX_STILL_WIDTH as f32 / width as f32 }
}

pub(crate) fn read_picture(path: &Path) -> Option<StillPicture> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut r = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIM);
    limits.max_image_height = Some(MAX_DECODE_DIM);
    limits.max_alloc = Some(256 << 20);
    r.limits(limits);
    let img = r.decode().ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    (w > 0 && h > 0).then(|| StillPicture { w, h, rgba: img.into_raw() })
}

/// The clip `stills.grab` shoots: `clip`, else the first selected video clip, else the topmost
/// video clip under the playhead.
fn grab_clip(s: &Session, p: &Value) -> Result<ClipId> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let on_video = |c: ClipId| q.video_tracks.iter().any(|t| t.item(c).is_some());
    if p.get("clip").is_some_and(|v| !v.is_null()) {
        let c = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad("stills.grab", "`clip` must be a clip id"))?;
        return if on_video(c) { Ok(c) } else { Err(bad("stills.grab", "`clip` must be a clip on a video track")) };
    }
    if let Some(c) = s.state.selection.iter().copied().find(|c| on_video(*c)) {
        return Ok(c);
    }
    let t = s.playhead();
    q.video_tracks.iter().rev().find_map(|tr| tr.item_at(t).map(|it| it.id)).ok_or_else(|| bad("stills.grab", "no video clip under the playhead (pass `clip`)"))
}

fn grab(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "stills.grab";
    if s.project.stills.len() >= MAX_STILLS {
        return Err(bad(CMD, format!("a project holds at most {MAX_STILLS} stills")));
    }
    let clip = grab_clip(s, p)?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let time = s.playhead();
    let (name, grade) = {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item(clip).ok_or_else(|| bad(CMD, "no such clip"))?;
        (it.name.clone(), it.grade.clone())
    };
    let width = s.active_sequence().map(|q| q.settings.width).unwrap_or(MAX_STILL_WIDTH);
    let img = s.render_program_at(fit_scale(width), time).ok_or(EngineError::NoSequence)?;
    let (w, h) = (u32::try_from(img.w).unwrap_or(0), u32::try_from(img.h).unwrap_or(0));
    if w == 0 || h == 0 {
        return Err(bad(CMD, "the frame is empty"));
    }
    let label = match str_p(p, "label") {
        Some(l) => clean_label(l),
        None => clean_label(&name),
    };
    let rgba = img.over_black_rgba8();
    let mut new_id = 0;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)).unwrap_or(0);
    s.edit("Grab Still", |pr, _| {
        let id = pr.alloc_id();
        new_id = id;
        let nonce = pr.stills.iter().map(|x| x.nonce).max().map_or(0, |m| m.saturating_add(1)).max(nanos);
        pr.stills.push(Still { id, label, clip: Some(clip), clip_name: name, sequence: Some(seq), time, grade, width: w, height: h, nonce });
        Ok(())
    })?;
    let file = s.project.stills.iter().find(|x| x.id == new_id).map(Still::file_name).unwrap_or_default();
    // an id can come back after an undo: never show the old picture for the new grab
    for d in s.stills_dirs() {
        let _ = std::fs::remove_file(d.join(&file));
    }
    let saved = s.stills_write_dir().is_some_and(|d| image::RgbaImage::from_raw(w, h, rgba).is_some_and(|im| im.save(d.join(&file)).is_ok()));
    Ok(json!({ "still": new_id, "picture": saved, "width": w, "height": h }))
}

/// REF.1: bring in a reference picture (PNG / JPEG / TIFF / … through the media crate's
/// decoders, size-capped) as a still without a clip or a grade. Its picture is stored like a
/// grab's (at most [`MAX_STILL_WIDTH`] wide); it can be compared against (`compare.set`) and
/// matched to (`grade.shotMatch` with `still`), never applied as a grade. The picture stays on
/// this machine (the stills folder); only its label is in the project.
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "stills.import";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(CMD, "reference pictures need files (not on the web)"));
    }
    if s.project.stills.len() >= MAX_STILLS {
        return Err(bad(CMD, format!("a project holds at most {MAX_STILLS} stills")));
    }
    let path = PathBuf::from(str_p(p, "path").ok_or_else(|| bad(CMD, "need `path` (a picture file)"))?);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let size = std::fs::metadata(&path).map_err(|e| bad(CMD, format!("can't read {name}: {e}")))?.len();
    if size > MAX_FILE_BYTES {
        return Err(bad(CMD, format!("{name} is larger than {} MB", MAX_FILE_BYTES >> 20)));
    }
    let bytes = std::fs::read(&path).map_err(|e| bad(CMD, format!("can't read {name}: {e}")))?;
    if !filmcraft_media::still::sniff(&name, &bytes) {
        return Err(bad(CMD, format!("{name} is not a picture FilmCraft reads (PNG, JPEG, TIFF, WebP, BMP, GIF, Targa, DPX)")));
    }
    // a decoder bug on a hostile file must not take the editor down
    let decoded = std::panic::catch_unwind(|| filmcraft_media::still::decode_raster_capped(&name, &bytes, MAX_IMPORT_DIM, MAX_IMPORT_ALLOC));
    let raster = match decoded {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(bad(CMD, format!("can't read the picture: {e}"))),
        Err(_) => return Err(bad(CMD, format!("can't read the picture {name} (damaged file)"))),
    };
    let img = image::RgbaImage::from_raw(raster.width, raster.height, raster.rgba).ok_or_else(|| bad(CMD, "the picture is empty"))?;
    let img = fit_picture(img);
    let (w, h) = img.dimensions();
    let label = match str_p(p, "label") {
        Some(l) => clean_label(l),
        None => clean_label(path.file_stem().map(|n| n.to_string_lossy()).as_deref().unwrap_or("Reference")),
    };
    let dir = s.stills_write_dir().ok_or_else(|| bad(CMD, "couldn't make the stills folder"))?;
    // written first, under a temporary name: a still without its picture never exists (a
    // reference picture can't be re-rendered)
    let nonce_name = format!(
        ".import-{}-{:x}.png",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    let staged = dir.join(nonce_name);
    img.save(&staged).map_err(|e| bad(CMD, format!("couldn't store the picture in the stills folder: {e}")))?;
    let mut new_id = 0;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)).unwrap_or(0);
    let edited = s.edit("Import Reference Still", |pr, _| {
        let id = pr.alloc_id();
        new_id = id;
        let nonce = pr.stills.iter().map(|x| x.nonce).max().map_or(0, |m| m.saturating_add(1)).max(nanos);
        pr.stills.push(Still {
            id,
            label,
            clip: None,
            clip_name: String::new(),
            sequence: None,
            time: filmcraft_time::Tick::ZERO,
            grade: None,
            width: w,
            height: h,
            nonce,
        });
        Ok(())
    });
    if let Err(e) = edited {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    let file = s.project.stills.iter().find(|x| x.id == new_id).map(Still::file_name).unwrap_or_default();
    for d in s.stills_dirs() {
        let _ = std::fs::remove_file(d.join(&file));
    }
    if std::fs::rename(&staged, dir.join(&file)).is_err() {
        // (rare: the folder changed under us) take the still back out, leaving no Redo for it
        let _ = std::fs::remove_file(&staged);
        let _ = s.undo();
        s.history.redo.pop();
        return Err(bad(CMD, "couldn't store the picture in the stills folder"));
    }
    Ok(json!({ "still": new_id, "width": w, "height": h }))
}

/// Scale a picture down (never up) so its longer side is at most [`MAX_STILL_WIDTH`].
fn fit_picture(img: image::RgbaImage) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= MAX_STILL_WIDTH {
        return img;
    }
    let f = MAX_STILL_WIDTH as f64 / long as f64;
    let nw = ((w as f64 * f).round() as u32).clamp(1, MAX_STILL_WIDTH);
    let nh = ((h as f64 * f).round() as u32).clamp(1, MAX_STILL_WIDTH);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let dirs = s.stills_dirs();
    let rows: Vec<Value> = s
        .project
        .stills
        .iter()
        .map(|st| {
            let nodes = st.grade.as_ref().map(|g| g.nodes.len());
            json!({
                "id": st.id, "label": st.label, "clip": st.clip.map(|c| c.0), "clipName": st.clip_name,
                "sequence": st.sequence.map(|q| q.0), "time": st.time.0, "graded": st.grade.is_some(), "nodes": nodes,
                "reference": st.is_reference(),
                "width": st.width, "height": st.height,
                "picture": dirs.iter().any(|d| d.join(st.file_name()).is_file()),
            })
        })
        .collect();
    Ok(json!({ "stills": rows, "max": MAX_STILLS }))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "stills.apply";
    let st = find(s, still_id(p, CMD)?, CMD)?;
    if st.is_reference() {
        return Err(bad(CMD, "a reference picture has no grade to apply; match to it with Shot Match (`grade.shotMatch` with `still`)"));
    }
    let targets = grade_targets(s, p, CMD, None)?;
    let n = edit_grades(s, &targets, "Apply Still Grade", |slot| *slot = st.grade.clone())?;
    Ok(json!({ "clips": n }))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "stills.delete";
    let id = still_id(p, CMD)?;
    find(s, id, CMD)?;
    // the picture file stays, so undo brings the still back whole
    s.edit("Delete Still", |pr, _| {
        pr.stills.retain(|x| x.id != id);
        Ok(())
    })?;
    if s.state.compare.still == Some(id) {
        s.state.compare = CompareState { still: None, mode: CompareMode::Off, ..s.state.compare.clone() };
    }
    Ok(Value::Null)
}

fn label(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "stills.label";
    let id = still_id(p, CMD)?;
    let text = clean_label(str_p(p, "label").ok_or_else(|| bad(CMD, "need `label`"))?);
    find(s, id, CMD)?;
    s.edit("Label Still", |pr, _| {
        if let Some(x) = pr.stills.iter_mut().find(|x| x.id == id) {
            x.label = text;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn compare_set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "compare.set";
    let mut next = s.state.compare.clone();
    match p.get("still") {
        None => {}
        Some(Value::Null) => {
            next.still = None;
            if p.get("mode").is_none() {
                next.mode = CompareMode::Off;
            }
        }
        Some(v) => {
            let id = v.as_u64().ok_or_else(|| bad(CMD, "`still` must be a still id or null"))?;
            find(s, id, CMD)?;
            next.still = Some(id);
        }
    }
    if let Some(m) = p.get("mode") {
        next.mode = match m.as_str() {
            Some("off") => CompareMode::Off,
            Some("wipe") => CompareMode::Wipe,
            Some("split") => CompareMode::Split,
            _ => return Err(bad(CMD, "`mode` must be \"off\", \"wipe\" or \"split\"")),
        };
    }
    if let Some(v) = p.get("position") {
        let x = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad(CMD, "`position` must be a number from 0 to 1"))?;
        next.position = x.clamp(0.0, 1.0) as f32;
    }
    if let Some(v) = p.get("vertical") {
        next.vertical = v.as_bool().ok_or_else(|| bad(CMD, "`vertical` must be true or false"))?;
    }
    if next.mode != CompareMode::Off && next.still.is_none() {
        return Err(bad(CMD, "choose a `still` to compare against"));
    }
    s.state.compare = next;
    compare_get(s, &Value::Null)
}

fn compare_get(s: &mut Session, _: &Value) -> Result<Value> {
    let c = &s.state.compare;
    let exists = c.still.is_some_and(|id| s.project.stills.iter().any(|x| x.id == id));
    Ok(json!({ "still": c.still, "mode": c.mode, "position": c.position, "vertical": c.vertical, "active": c.active() && exists }))
}
