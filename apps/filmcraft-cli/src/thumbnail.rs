//! `filmcraft-cli thumbnail <file> <size> <out.png>`: a small picture of a project or a media file
//! for file managers (LINUX-PLAN L2.2). The freedesktop `.thumbnailer`
//! (`packaging/linux/io.github.dawgon2.Revolution.thumbnailer`, GNOME Files, Nemo, Thunar…) and the
//! fork's KDE / Dolphin plugin (`fork/kde-thumbnailer/`) both run it.
//!
//! A project (`.fcproj`) shows its active sequence; a video or picture file shows itself. The
//! frame is taken a tenth of the way in (at most 10 s), past most fades from black. The longer
//! side of the PNG is `size` pixels (clamped to [`MIN_SIZE`]–[`MAX_SIZE`]). Everything in the
//! file is untrusted: a broken project or clip is an error (exit status 1), never a crash, and
//! the file manager's own timeout bounds the time.

use std::path::Path;

use filmcraft_engine::Session;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

/// Smallest and largest thumbnail sizes accepted (the spec's sizes are 128–1024).
pub const MIN_SIZE: u32 = 16;
pub const MAX_SIZE: u32 = 2048;
/// Projects bigger than this are not opened for a thumbnail (a real one is a few MB at most).
pub const MAX_PROJECT_BYTES: u64 = 256 * 1024 * 1024;

/// Parse the size argument.
pub fn parse_size(s: &str) -> Result<u32, String> {
    let n: u32 = s.trim().parse().map_err(|_| format!("thumbnail size must be a number of pixels, got `{s}`"))?;
    Ok(n.clamp(MIN_SIZE, MAX_SIZE))
}

/// Where in a sequence of `duration` the picture is taken.
pub fn poster_time(duration: Tick) -> Tick {
    Tick(duration.0.max(0) / 10).min(Tick(10 * TICKS_PER_SECOND))
}

/// Render the thumbnail of `input` as PNG bytes, longer side `size`.
pub fn render(input: &Path, size: u32) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(input).map_err(|e| format!("{}: {e}", input.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", input.display()));
    }
    let path = input.to_string_lossy().into_owned();
    let mut s = Session::default();
    let is_project = input.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("fcproj"));
    if is_project {
        if meta.len() > MAX_PROJECT_BYTES {
            return Err(format!("{path}: too big for a project ({} bytes)", meta.len()));
        }
        s.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())?;
    } else {
        // a thumbnail never copies, transcodes or makes proxies (a fresh session has no ingest and
        // no automatic proxies; said explicitly so it stays that way)
        std::sync::Arc::make_mut(&mut s.project).settings.ingest.enabled = false;
        s.auto_proxies.enabled = false;
        let r = s.execute("file.import", json!({"paths": [path]})).map_err(|e| e.to_string())?;
        let item = first_item(&r).ok_or_else(|| format!("{path}: nothing Revolution can show"))?;
        s.execute("file.newSequenceFromClip", json!({"items": [item]})).map_err(|e| e.to_string())?;
    }
    let seq = s.active_sequence().ok_or_else(|| format!("{path}: the project has no sequence"))?;
    let (w, h) = (seq.settings.width.max(1), seq.settings.height.max(1));
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    let scale = (size as f32 / w.max(h) as f32).clamp(0.001, 1.0);
    let t = poster_time(seq.duration());
    s.set_playhead(t);
    let img = s.try_render_program_at(scale, s.playhead()).map_err(|e| e.to_string())?;
    let (iw, ih) = (u32::try_from(img.w).map_err(|e| e.to_string())?, u32::try_from(img.h).map_err(|e| e.to_string())?);
    filmcraft_automation::png_rgba(iw, ih, img.over_black_rgba8(), size).map_err(|e| e.to_string())
}

/// The first imported item's id in a `file.import` result (`items` holds ids or `{id}` objects).
fn first_item(r: &Value) -> Option<u64> {
    let first = r.get("items")?.as_array()?.first()?;
    first.as_u64().or_else(|| first.get("id").and_then(Value::as_u64))
}

/// Render and write `out` (written next to it first, then renamed: a file manager never reads a
/// half-written PNG).
pub fn write(input: &Path, size: u32, out: &Path) -> Result<(), String> {
    let png = render(input, size)?;
    let tmp = out.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, png).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", out.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_poster_times() {
        assert_eq!(parse_size("256"), Ok(256));
        assert_eq!(parse_size(" 128\n"), Ok(128));
        assert_eq!(parse_size("1"), Ok(MIN_SIZE));
        assert_eq!(parse_size("999999"), Ok(MAX_SIZE));
        for bad in ["", "-5", "abc", "1e3", "4294967296"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        assert_eq!(poster_time(Tick(0)), Tick(0));
        assert_eq!(poster_time(Tick(-5)), Tick(0));
        assert_eq!(poster_time(Tick(20 * TICKS_PER_SECOND)), Tick(2 * TICKS_PER_SECOND));
        assert_eq!(poster_time(Tick(i64::MAX)), Tick(10 * TICKS_PER_SECOND));
        assert_eq!(first_item(&json!({"items": [7]})), Some(7));
        assert_eq!(first_item(&json!({"items": [{"id": 9}]})), Some(9));
        assert_eq!(first_item(&json!({"items": []})), None);
        assert_eq!(first_item(&json!(null)), None);
    }
}
