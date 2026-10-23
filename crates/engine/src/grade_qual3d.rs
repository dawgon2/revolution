//! 3D qualifier commands (`grade.qual3d.*`, fork GRADE-PLAN G4.3): strokes on the picture pick
//! the colours a grade node's qualifier selects ([`filmcraft_project::qual3d`]). Each command is
//! one undo step; `grade.qual3d.set` takes a `merge` key so a slider drag is one.
//!
//! Clip and node are addressed as by every `grade.*` command (`clip`, `node`, else the current
//! node). A stroke is a list of points in the sequence frame (0…1 across and down, as the HSL
//! picker `grade.pickQualifier` takes them): the colours along it are sampled from the node's
//! input at `time` (else the playhead): the clip through its input transform and the nodes before
//! this one, no effects after the grade. A stroke switches the node's qualifier on and to 3D.

use filmcraft_project::grade::{GradeNode, NodeParamKind, node_param};
use filmcraft_project::qual3d::{MAX_SAMPLES, Qual3d, QualMode};
use filmcraft_project::{ClipId, Param, ParamValue};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, str_p};
use crate::grade::{current_for, edit_grade, edit_grade_merged, media_time, node_arg, pick};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "grade.qual3d.addStroke",
            "Add 3D Qualifier Stroke",
            r#"{"clip":id?,"node":id?,"points":[[x,y]|{"x":0..1,"y":0..1}],"exclude":bool?,"time":ticks?}"#,
            add_stroke,
        ),
        spec("grade.qual3d.set", "Set 3D Qualifier", r#"{"clip":id?,"node":id?,"mode":"hsl"|"3d"?,"softness":0.001..0.5?,"despill":0..1?,"merge":str?}"#, set),
        spec("grade.qual3d.clear", "Clear 3D Qualifier", r#"{"clip":id?,"node":id?,"which":"include"|"exclude"|"all"?}"#, clear),
    ]
}

/// Most stroke points used (more are thinned out evenly).
pub const MAX_POINTS: usize = 1024;
/// How far outside the frame (0…1) a point may be and still count (it is then clamped in).
const EDGE: f64 = 1e-3;

/// The stroke's points: finite and inside the frame (others dropped), at most [`MAX_POINTS`]
/// (evenly thinned). Each is `[x, y]` or `{"x": x, "y": y}`.
fn points(p: &Value, cmd: &str) -> Result<Vec<[f32; 2]>> {
    let arr = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(cmd, "need `points`: a list of [x, y] in 0…1 of the frame"))?;
    let read = |v: &Value| -> Option<[f64; 2]> {
        match v {
            Value::Array(a) if a.len() == 2 => Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]),
            Value::Object(o) => Some([o.get("x")?.as_f64()?, o.get("y")?.as_f64()?]),
            _ => None,
        }
    };
    let inside = |v: f64| v.is_finite() && (-EDGE..=1.0 + EDGE).contains(&v);
    let all: Vec<[f32; 2]> = arr.iter().filter_map(read).filter(|q| q.iter().all(|v| inside(*v))).map(|q| q.map(|v| v.clamp(0.0, 1.0) as f32)).collect();
    if all.is_empty() {
        return Err(bad(cmd, "the stroke has no point inside the frame (points are [x, y] in 0…1)"));
    }
    if all.len() <= MAX_POINTS {
        return Ok(all);
    }
    let n = all.len();
    Ok((0..MAX_POINTS).filter_map(|i| all.get(i.saturating_mul(n - 1) / (MAX_POINTS - 1)).copied()).collect())
}

/// Positions (0…1) along the stroke a pixel apart in `w` × `h`, at most [`MAX_SAMPLES`] (spread
/// evenly over a longer stroke); a single point (or a stroke shorter than a pixel) samples its
/// points.
fn along(pts: &[[f32; 2]], w: usize, h: usize) -> Vec<[f32; 2]> {
    let (fw, fh) = (w.max(1) as f32, h.max(1) as f32);
    let seg = |a: [f32; 2], b: [f32; 2]| (((b[0] - a[0]) * fw).powi(2) + ((b[1] - a[1]) * fh).powi(2)).sqrt();
    let total: f32 = pts.windows(2).map(|s| seg(s[0], s[1])).sum();
    if !(total.is_finite() && total >= 1.0) {
        return pts.iter().take(MAX_SAMPLES).copied().collect();
    }
    let step = (total / MAX_SAMPLES as f32).max(1.0);
    let mut out = Vec::new();
    let mut next = 0.0f32;
    let mut walked = 0.0f32;
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let len = seg(a, b);
        while next <= walked + len && out.len() < MAX_SAMPLES {
            let t = if len > 0.0 { ((next - walked) / len).clamp(0.0, 1.0) } else { 0.0 };
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            next += step;
        }
        walked += len;
    }
    out
}

/// Switch the node's qualifier on at media time `mt` (as the HSL picker does).
fn switch_on(n: &mut GradeNode, mt: Tick) {
    if n.float_at("qual.on", mt) >= 0.5 || !matches!(node_param("qual.on").map(|d| d.kind), Some(NodeParamKind::Float { .. })) {
        return;
    }
    let v = ParamValue::Float(1.0);
    match n.params.get_mut("qual.on") {
        Some(prm) => prm.set_at(mt, v),
        None => {
            n.params.insert("qual.on".to_string(), Param::new(v));
        }
    }
}

fn result(c: u64, node: u64, q: &Option<Qual3d>) -> Value {
    json!({"clip": c, "node": node, "qual3d": q})
}

fn add_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.qual3d.addStroke";
    let exclude = match p.get("exclude") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "`exclude` must be true or false"))?,
    };
    let pts = points(p, CMD)?;
    let (c, id, img, working) = crate::grade::node_input(s, p, CMD)?;
    let samples: Vec<[f32; 3]> = along(&pts, img.w, img.h)
        .into_iter()
        .filter_map(|[x, y]| crate::grade::sample(&img, x, y))
        .filter_map(|rgb| filmcraft_render::grade::qual3d_point(working, rgb))
        .collect();
    if samples.is_empty() {
        return Err(bad(CMD, "the clip has no picture under the stroke"));
    }
    let mt = media_time(s, p, c);
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("clip".into(), json!(c.0));
        o.insert("node".into(), json!(id));
    }
    let label = if exclude { "Exclude Colours (3D Qualifier)" } else { "Include Colours (3D Qualifier)" };
    let (used, q3) = edit_grade(s, &q, CMD, label, false, |g| {
        let n = g.node_mut(id).ok_or_else(|| bad(CMD, format!("no node {id}")))?;
        let mut k = n.qual3d.take().unwrap_or_default();
        k.mode = QualMode::ThreeD;
        let used = k.add_samples(&samples, exclude);
        n.qual3d = Some(k.cleaned());
        switch_on(n, mt);
        Ok((used, n.qual3d.clone()))
    })?;
    let mut r = result(c.0, id, &q3);
    r["samples"] = json!(used);
    Ok(r)
}

/// The node a command addresses on clip `c` and its 3D qualifier, without editing.
fn peek(s: &Session, c: ClipId, want: Option<u64>, current: Option<u64>, cmd: &str) -> Result<(u64, Option<Qual3d>)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    let g = it.grade.as_ref().ok_or_else(|| bad(cmd, "the clip has no grade"))?;
    let id = pick(g, want, current, cmd)?;
    let n = g.node(id).ok_or_else(|| bad(cmd, format!("no node {id}")))?;
    Ok((id, n.qual3d.clone()))
}

/// Run `f` on the addressed node's 3D qualifier as one undo step (cleaned afterwards); an edit
/// that changes nothing makes no undo step.
fn edit_q3(s: &mut Session, p: &Value, cmd: &'static str, label: &str, merge: Option<&str>, f: impl Fn(&mut Option<Qual3d>) -> Result<()>) -> Result<Value> {
    let want = node_arg(p, cmd)?;
    let (c, current) = current_for(s, p, cmd)?;
    let (id, before) = peek(s, c, want, current, cmd)?;
    let mut trial = before.clone();
    f(&mut trial)?;
    if trial.map(Qual3d::cleaned) == before {
        return Ok(result(c.0, id, &before));
    }
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("clip".into(), json!(c.0));
    }
    let after = edit_grade_merged(s, &q, cmd, label, false, merge, |g| {
        let n = g.node_mut(id).ok_or_else(|| bad(cmd, format!("no node {id}")))?;
        f(&mut n.qual3d)?;
        n.qual3d = n.qual3d.take().map(Qual3d::cleaned);
        Ok(n.qual3d.clone())
    })?;
    Ok(result(c.0, id, &after))
}

/// An optional finite number.
fn num(p: &Value, k: &str, cmd: &str) -> Result<Option<f32>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        // (clamped before the cast: -1e300 must not become -∞)
        Some(v) => v.as_f64().filter(|f| f.is_finite()).map(|f| Some(f.clamp(-1e6, 1e6) as f32)).ok_or_else(|| bad(cmd, format!("`{k}` must be a number"))),
    }
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.qual3d.set";
    if p.get("mode").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err(bad(CMD, "`mode` must be \"hsl\" or \"3d\""));
    }
    let mode = match str_p(p, "mode") {
        None => None,
        Some("hsl") => Some(QualMode::Hsl),
        Some("3d") => Some(QualMode::ThreeD),
        Some(m) => return Err(bad(CMD, format!("unknown mode `{m}` (hsl or 3d)"))),
    };
    let (softness, despill) = (num(p, "softness", CMD)?, num(p, "despill", CMD)?);
    if mode.is_none() && softness.is_none() && despill.is_none() {
        return Err(bad(CMD, "need one of `mode`, `softness`, `despill`"));
    }
    let merge = str_p(p, "merge").map(str::to_string);
    edit_q3(s, p, CMD, "Change 3D Qualifier", merge.as_deref(), |slot| {
        // (switching an HSL node without a 3D qualifier to HSL changes nothing)
        if slot.is_none() && mode == Some(QualMode::Hsl) && softness.is_none() && despill.is_none() {
            return Ok(());
        }
        let k = slot.get_or_insert_with(|| Qual3d { mode: QualMode::Hsl, ..Qual3d::default() });
        if let Some(m) = mode {
            k.mode = m;
        }
        if let Some(v) = softness {
            k.softness = v;
        }
        if let Some(v) = despill {
            k.despill = v;
        }
        Ok(())
    })
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.qual3d.clear";
    let which = str_p(p, "which").unwrap_or("all");
    if !matches!(which, "include" | "exclude" | "all") {
        return Err(bad(CMD, format!("unknown `which` `{which}` (include, exclude or all)")));
    }
    edit_q3(s, p, CMD, "Clear 3D Qualifier", None, |slot| {
        let nothing = || bad(CMD, "the node has no 3D qualifier strokes to clear");
        match which {
            // (back to the HSL qualifier, as before the first stroke)
            "all" => slot.take().map(|_| ()).ok_or_else(nothing),
            side => {
                let k = slot.as_mut().ok_or_else(nothing)?;
                let v = if side == "exclude" { &mut k.exclude } else { &mut k.include };
                if v.is_empty() {
                    return Err(nothing());
                }
                v.clear();
                Ok(())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_cleaned_and_thinned() {
        let mut many: Vec<Value> = (0..10_000).map(|i| json!([i as f64 / 10_000.0, 0.5])).collect();
        many.push(json!([f64::MAX, 0.5]));
        many.push(json!({"x": -3.0, "y": 0.5}));
        many.push(json!("nonsense"));
        many.push(json!([0.5]));
        let pts = points(&json!({"points": many}), "t").unwrap();
        assert_eq!(pts.len(), MAX_POINTS);
        assert!(pts.iter().all(|p| (0.0..=1.0).contains(&p[0]) && p[1] == 0.5));
        assert!(pts.first().is_some_and(|p| p[0] == 0.0) && pts.last().is_some_and(|p| p[0] > 0.99));
        assert!(points(&json!({"points": [[2.0, 0.5], {"x": 0.5}]}), "t").is_err());
        assert!(points(&json!({"points": 3}), "t").is_err());
        assert_eq!(points(&json!({"points": [{"x": 1.0005, "y": -0.0002}]}), "t").unwrap(), vec![[1.0, 0.0]]);
    }

    #[test]
    fn samples_walk_the_stroke_a_pixel_apart_and_are_capped() {
        let s = along(&[[0.0, 0.0], [1.0, 0.0]], 100, 50);
        assert!((100..=102).contains(&s.len()), "{}", s.len());
        let long = along(&[[0.0, 0.0], [1.0, 1.0], [0.0, 1.0], [1.0, 0.0]], 8000, 8000);
        assert!(long.len() <= MAX_SAMPLES && long.len() > MAX_SAMPLES / 2, "{}", long.len());
        assert_eq!(along(&[[0.3, 0.3]], 100, 100), vec![[0.3, 0.3]]);
        assert_eq!(along(&[[0.3, 0.3], [0.3, 0.3]], 100, 100).len(), 2);
    }
}
