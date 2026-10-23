//! Roto and Paint node commands (fork C3.5b). Shapes and strokes are addressed by their list
//! index; coordinates are comp pixels, times comp ticks. Every edit is one undo step (a drag with
//! the same `gesture` merges into one); lists are checked against the caps before they are read,
//! and a refused command changes nothing.

use filmcraft_project::comp::{CompKind, RotoOutput};
use filmcraft_project::dag::NodeId;
use filmcraft_project::paint::{MAX_STROKE_POINTS, MAX_STROKES, MAX_TOTAL_POINTS, Paint, PaintStroke};
use filmcraft_project::roto::{MAX_KEYFRAMES, MAX_POINTS, MAX_SHAPES, Roto, RotoKind, RotoMode, RotoShape};
use filmcraft_project::{MaskPath, Param, ParamValue};
use filmcraft_time::TICKS_PER_SECOND;
use serde_json::{Value, json};

use super::{Comp, comp_of, edit_comp, node_mut, node_p, spec, target, ticks_p};
use crate::commands::{CommandSpec, bad, str_p};
use crate::{Result, Session};

pub(super) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "comp.setRoto",
            "Set Roto Shapes",
            r#"{"comp":id?,"group":[id]?,"node":id,"roto":{"shapes":[shape]}?,"output":"alpha"|"matte"?,"gesture":n?}"#,
            set_roto,
        ),
        spec(
            "comp.roto.addShape",
            "Add Roto Shape",
            r#"{"comp":id?,"group":[id]?,"node":id,"shape":shape?,"path":path?,"kind":"bezier"|"bSpline"?,"name":str?,"mode":"add"|"subtract"|"intersect"|"replace"?,"feather":n?,"at":n?,"time":ticks?}"#,
            add_shape,
        ),
        spec(
            "comp.roto.setShapeKeyframe",
            "Set Roto Shape Keyframe",
            r#"{"comp":id?,"group":[id]?,"node":id,"shape":n,"time":ticks,"path":path,"onScreen":bool? (a shape following a tracker: `path` is as seen at `time`, stored through the tracker's motion; default false = the tracker's reference frame, as stored),"gesture":n?}"#,
            set_keyframe,
        ),
        spec("comp.roto.removeShape", "Delete Roto Shape", r#"{"comp":id?,"group":[id]?,"node":id,"shape":n}"#, remove_shape),
        spec("comp.setPaint", "Set Paint Strokes", r#"{"comp":id?,"group":[id]?,"node":id,"paint":{"strokes":[stroke]},"gesture":n?}"#, set_paint),
        spec("comp.paint.addStroke", "Add Paint Stroke", r#"{"comp":id?,"group":[id]?,"node":id,"stroke":stroke,"at":n?}"#, add_stroke),
        spec("comp.paint.removeStroke", "Delete Paint Stroke", r#"{"comp":id?,"group":[id]?,"node":id,"stroke":n}"#, remove_stroke),
        spec("comp.paint.reorder", "Reorder Paint Stroke", r#"{"comp":id?,"group":[id]?,"node":id,"from":n,"to":n}"#, reorder),
    ]
}

/// A list index (a whole number ≥ 0; floats are refused).
fn index_p(p: &Value, key: &str, cmd: &str) -> Result<usize> {
    p.get(key).and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(cmd, format!("need `{key}` (a list index)")))
}

fn opt_index_p(p: &Value, key: &str, cmd: &str) -> Result<Option<usize>> {
    if p.get(key).is_none_or(Value::is_null) { Ok(None) } else { index_p(p, key, cmd).map(Some) }
}

/// The merge key of a drag (`gesture`): one undo step while the same gesture repeats.
fn gesture_key(s: &Session, p: &Value, cmd: &str, node: NodeId) -> Result<Option<String>> {
    match p.get("gesture").and_then(Value::as_u64) {
        Some(g) => Ok(Some(format!("{cmd}/{}/{node}/{g}", target(s, p, cmd)?.0))),
        None => Ok(None),
    }
}

fn roto_of<'a>(c: &'a mut Comp, node: NodeId, cmd: &str) -> Result<&'a mut Roto> {
    match &mut node_mut(c, node, cmd)?.kind {
        CompKind::Roto { roto, .. } => Ok(roto),
        _ => Err(bad(cmd, format!("node {node} is not a Roto node"))),
    }
}

fn paint_of<'a>(c: &'a mut Comp, node: NodeId, cmd: &str) -> Result<&'a mut Paint> {
    match &mut node_mut(c, node, cmd)?.kind {
        CompKind::Paint { paint } => Ok(paint),
        _ => Err(bad(cmd, format!("node {node} is not a Paint node"))),
    }
}

/// A shape path (any `masks` JSON form, per-point `feather_in` / `feather_out` included), cleaned.
fn path_p(p: &Value, key: &str, cmd: &str) -> Result<Option<MaskPath>> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let too_many = || bad(cmd, format!("a roto shape keeps at most {MAX_POINTS} points"));
    if v.as_array().or_else(|| v.get("vertices").and_then(Value::as_array)).is_some_and(|a| a.len() > MAX_POINTS) {
        return Err(too_many());
    }
    let path = crate::masks::path_from_json(v).ok_or_else(|| bad(cmd, format!("`{key}` is not a path")))?;
    if path.vertices.len() > MAX_POINTS {
        return Err(too_many());
    }
    Ok(Some(path.cleaned(MAX_POINTS)))
}

/// A shape from `shape` (its serialized form) and / or the short fields; default a centred
/// ellipse a fifth of the comp's short side.
fn shape_p(p: &Value, cmd: &str, (w, h): (f64, f64)) -> Result<RotoShape> {
    let mut shape = match p.get("shape").filter(|v| !v.is_null()) {
        Some(v) => serde_json::from_value::<RotoShape>(v.clone()).map_err(|e| bad(cmd, format!("not a roto shape: {e}")))?,
        None => {
            let (w, h) = (w.max(1.0), h.max(1.0));
            let r = w.min(h) * 0.2;
            let ellipse = MaskPath::ellipse(filmcraft_geom::Vec2::new(w / 2.0, h / 2.0), filmcraft_geom::Vec2::new(r, r));
            RotoShape::new("", RotoKind::Bezier, ellipse)
        }
    };
    if let Some(path) = path_p(p, "path", cmd)? {
        shape.shape = Param::new(ParamValue::Path(path));
    }
    if let Some(k) = p.get("kind") {
        shape.kind = match k.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("bezier") => RotoKind::Bezier,
            Some("bspline" | "b-spline") => RotoKind::BSpline,
            _ => return Err(bad(cmd, "`kind` must be \"bezier\" or \"bSpline\"")),
        };
    }
    if let Some(m) = p.get("mode") {
        shape.mode = m.as_str().and_then(RotoMode::from_name).ok_or_else(|| bad(cmd, "`mode` must be add, subtract, intersect or replace"))?;
    }
    if let Some(n) = str_p(p, "name") {
        shape.name = n.to_string();
    }
    if let Some(f) = p.get("feather") {
        let f = f.as_f64().filter(|f| f.is_finite() && *f >= 0.0).ok_or_else(|| bad(cmd, "`feather` must be a distance ≥ 0 (comp pixels)"))?;
        shape.feather = Param::new(ParamValue::Float(f));
    }
    shape.sanitize();
    Ok(shape)
}

fn set_roto(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setRoto";
    let node = node_p(p, "node", CMD)?;
    let roto = match p.get("roto").filter(|v| !v.is_null()) {
        Some(v) => {
            if v.get("shapes").and_then(Value::as_array).is_some_and(|a| a.len() > MAX_SHAPES) {
                return Err(bad(CMD, format!("a Roto node keeps at most {MAX_SHAPES} shapes")));
            }
            let mut r = serde_json::from_value::<Roto>(v.clone()).map_err(|e| bad(CMD, format!("not a roto: {e}")))?;
            r.sanitize();
            Some(r)
        }
        None => None,
    };
    let output = match p.get("output").filter(|v| !v.is_null()) {
        Some(o) => Some(o.as_str().and_then(RotoOutput::from_name).ok_or_else(|| bad(CMD, "`output` must be \"alpha\" or \"matte\""))?),
        None => None,
    };
    if roto.is_none() && output.is_none() {
        return Err(bad(CMD, "need `roto` and / or `output`"));
    }
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Set Roto Shapes", merge, |c| {
        let CompKind::Roto { roto: r, output: o } = &mut node_mut(c, node, CMD)?.kind else {
            return Err(bad(CMD, format!("node {node} is not a Roto node")));
        };
        if let Some(new) = roto {
            *r = new;
        }
        if let Some(new) = output {
            *o = new;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn add_shape(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.roto.addShape";
    let node = node_p(p, "node", CMD)?;
    let at = opt_index_p(p, "at", CMD)?;
    let time = ticks_p(p, "time", CMD)?;
    let id = target(s, p, CMD)?;
    let size = comp_of(&s.project, id).map_or((1920.0, 1080.0), |c| (f64::from(c.width), f64::from(c.height)));
    let mut shape = shape_p(p, CMD, size)?;
    edit_comp(s, p, CMD, "Add Roto Shape", None, |c| {
        let roto = roto_of(c, node, CMD)?;
        let i = at.unwrap_or(roto.shapes.len());
        if shape.name.is_empty() {
            shape.name = format!("Shape {}", roto.shapes.len().saturating_add(1));
        }
        // with a `time` the path starts as a keyframe there (the shape is animated)
        if let Some(t) = time {
            let path = shape.path_at(t);
            shape.shape = Param::new(ParamValue::Path(path.clone()));
            shape.put_path_keyframe(t, path);
        }
        roto.insert(i, shape).map_err(|e| bad(CMD, e))?;
        Ok(json!({ "shape": i }))
    })
}

fn set_keyframe(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.roto.setShapeKeyframe";
    let node = node_p(p, "node", CMD)?;
    let i = index_p(p, "shape", CMD)?;
    let time = ticks_p(p, "time", CMD)?.ok_or_else(|| bad(CMD, format!("need `time` (comp ticks, {TICKS_PER_SECOND}/s)")))?;
    let path = path_p(p, "path", CMD)?.ok_or_else(|| bad(CMD, "need `path`"))?;
    let on_screen = match p.get("onScreen") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "`onScreen` is true or false"))?,
    };
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Set Roto Shape Keyframe", merge, |c| {
        // LO.5: a shape following a tracker stores its outline in the tracker's reference frame
        let link = roto_of(c, node, CMD)?.shapes.get(i).and_then(|sh| sh.track_link.clone());
        let path = match (on_screen, link) {
            (true, Some(l)) => crate::comp_roto_track::screen_to_stored(c, &l, time, &path)
                .ok_or_else(|| bad(CMD, "the tracker this shape follows has no usable motion at that time: track it there first"))?,
            _ => path,
        };
        let shape = roto_of(c, node, CMD)?.shapes.get_mut(i).ok_or_else(|| bad(CMD, format!("no shape {i}")))?;
        if shape.shape.keyframes.len() >= MAX_KEYFRAMES && !shape.shape.keyframes.iter().any(|k| k.time == time) {
            return Err(bad(CMD, format!("a roto shape keeps at most {MAX_KEYFRAMES} keyframes")));
        }
        shape.put_path_keyframe(time, path);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn remove_shape(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.roto.removeShape";
    let node = node_p(p, "node", CMD)?;
    let i = index_p(p, "shape", CMD)?;
    edit_comp(s, p, CMD, "Delete Roto Shape", None, |c| roto_of(c, node, CMD)?.remove(i).map(|_| ()).ok_or_else(|| bad(CMD, format!("no shape {i}"))))?;
    Ok(Value::Null)
}

/// A stroke from JSON, its point count checked before it is read.
fn stroke_from(v: &Value, cmd: &str) -> Result<PaintStroke> {
    if v.get("points").and_then(Value::as_array).is_some_and(|a| a.len() > MAX_STROKE_POINTS) {
        return Err(bad(cmd, format!("a stroke keeps at most {MAX_STROKE_POINTS} points")));
    }
    serde_json::from_value::<PaintStroke>(v.clone()).map_err(|e| bad(cmd, format!("not a paint stroke: {e}")))
}

fn set_paint(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setPaint";
    let node = node_p(p, "node", CMD)?;
    let raw = p.get("paint").and_then(|v| v.get("strokes")).and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `paint` ({\"strokes\": [...]})"))?;
    if raw.len() > MAX_STROKES {
        return Err(bad(CMD, format!("a Paint node keeps at most {MAX_STROKES} strokes")));
    }
    let mut total = 0usize;
    for v in raw {
        total = total.saturating_add(v.get("points").and_then(Value::as_array).map_or(0, Vec::len));
    }
    if total > MAX_TOTAL_POINTS {
        return Err(bad(CMD, format!("a Paint node keeps at most {MAX_TOTAL_POINTS} points in all")));
    }
    let strokes = raw.iter().map(|v| stroke_from(v, CMD)).collect::<Result<Vec<_>>>()?;
    let mut paint = Paint { strokes };
    paint.sanitize();
    let merge = gesture_key(s, p, CMD, node)?;
    edit_comp(s, p, CMD, "Set Paint Strokes", merge, |c| {
        *paint_of(c, node, CMD)? = paint;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn add_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.paint.addStroke";
    let node = node_p(p, "node", CMD)?;
    let at = opt_index_p(p, "at", CMD)?;
    let stroke = stroke_from(p.get("stroke").ok_or_else(|| bad(CMD, "need `stroke`"))?, CMD)?;
    edit_comp(s, p, CMD, "Add Paint Stroke", None, |c| {
        let paint = paint_of(c, node, CMD)?;
        if paint.total_points().saturating_add(stroke.points.len()) > MAX_TOTAL_POINTS {
            return Err(bad(CMD, format!("a Paint node keeps at most {MAX_TOTAL_POINTS} points in all")));
        }
        let i = at.unwrap_or(paint.strokes.len());
        paint.insert(i, stroke).map_err(|e| bad(CMD, e))?;
        Ok(json!({ "stroke": i }))
    })
}

fn remove_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.paint.removeStroke";
    let node = node_p(p, "node", CMD)?;
    let i = index_p(p, "stroke", CMD)?;
    edit_comp(s, p, CMD, "Delete Paint Stroke", None, |c| paint_of(c, node, CMD)?.remove(i).map(|_| ()).ok_or_else(|| bad(CMD, format!("no stroke {i}"))))?;
    Ok(Value::Null)
}

fn reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.paint.reorder";
    let node = node_p(p, "node", CMD)?;
    let (from, to) = (index_p(p, "from", CMD)?, index_p(p, "to", CMD)?);
    edit_comp(s, p, CMD, "Reorder Paint Stroke", None, |c| {
        if paint_of(c, node, CMD)?.reorder(from, to) { Ok(()) } else { Err(bad(CMD, format!("no stroke {from} or {to}"))) }
    })?;
    Ok(Value::Null)
}
