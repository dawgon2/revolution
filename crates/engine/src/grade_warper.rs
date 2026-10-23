//! Colour warper commands (`grade.warper.*`, fork GRADE-PLAN G4.1): edit a grade node's
//! hue-sat and chroma-luma meshes ([`filmcraft_project::warper`]). Each command is one undo step;
//! `grade.warper.movePoint` and `grade.warper.set` take a `merge` key so a whole drag is one.
//!
//! Clip and node are addressed as by every `grade.*` command (`clip`, `node`, else the current
//! node). `mesh` is `"hueSat"` or `"chromaLuma"` (default: the warper's shown mesh). Moving a
//! point or changing a setting adds a warper to a node without one.

use filmcraft_project::ClipId;
use filmcraft_project::warper::{CL_SIDE, HUE_COLUMNS, SAT_RINGS, WarpMode, WarpPoint, Warper};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, str_p};
use crate::grade::{current_for, edit_grade_merged, node_arg, pick};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_seq, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "grade.warper.set",
            "Set Colour Warper",
            r#"{"clip":id?,"node":id?,"warper":{..}?,"mode":"hueSat"|"chromaLuma"?,"strength":0..1?,"range":0..4?,"preserveLuma":bool?,"hueColumns":6..16?,"satRings":4..8?,"chromaColumns":4..8?,"lumaRows":4..8?,"merge":str?}"#,
            set,
        ),
        spec(
            "grade.warper.movePoint",
            "Move Warper Point",
            r#"{"clip":id?,"node":id?,"mesh":"hueSat"|"chromaLuma"?,"index":n,"to":[u,v],"merge":str?}"#,
            move_point,
        ),
        spec("grade.warper.pin", "Pin Warper Point", r#"{"clip":id?,"node":id?,"mesh":"hueSat"|"chromaLuma"?,"index":n,"on":bool?}"#, pin),
        spec("grade.warper.reset", "Reset Colour Warper", r#"{"clip":id?,"node":id?,"mesh":"hueSat"|"chromaLuma"?,"index":n?}"#, reset),
    ]
}

/// The node a command addresses on clip `c` and its warper, without editing.
fn peek(s: &Session, c: ClipId, want: Option<u64>, current: Option<u64>, cmd: &str) -> Result<(u64, Option<Warper>)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(c).ok_or_else(|| bad(cmd, "no such clip"))?;
    let g = it.grade.as_ref().ok_or_else(|| bad(cmd, "the clip has no grade"))?;
    let id = pick(g, want, current, cmd)?;
    let n = g.node(id).ok_or_else(|| bad(cmd, format!("no node {id}")))?;
    Ok((id, n.warper.clone()))
}

/// Run `f` on the addressed node's warper (created when `create` and missing) as one undo step;
/// the warper is cleaned afterwards. An edit that changes nothing makes no undo step. Returns
/// the clip, node and the warper after.
fn edit_warper(
    s: &mut Session,
    p: &Value,
    cmd: &'static str,
    label: &str,
    create: bool,
    merge: Option<&str>,
    f: impl Fn(&mut Option<Warper>) -> Result<()>,
) -> Result<(u64, u64, Option<Warper>)> {
    let want = node_arg(p, cmd)?;
    let (c, current) = current_for(s, p, cmd)?;
    // (a trial run on a copy: errors and no-op edits never reach the history)
    if let Ok((id, before)) = peek(s, c, want, current, cmd) {
        let mut trial = before.clone();
        if create && trial.is_none() {
            trial = Some(Warper::default());
        }
        f(&mut trial)?;
        if trial.map(Warper::cleaned) == before {
            return Ok((c.0, id, before));
        }
    }
    let mut q = p.clone();
    if let Some(o) = q.as_object_mut() {
        o.insert("clip".into(), json!(c.0));
    }
    let (id, w) = edit_grade_merged(s, &q, cmd, label, false, merge, |g| {
        let id = pick(g, want, current, cmd)?;
        let n = g.node_mut(id).ok_or_else(|| bad(cmd, format!("no node {id}")))?;
        if create && n.warper.is_none() {
            n.warper = Some(Warper::default());
        }
        f(&mut n.warper)?;
        n.warper = n.warper.take().map(Warper::cleaned);
        Ok((id, n.warper.clone()))
    })?;
    Ok((c.0, id, w))
}

fn mode_of(v: &Value, cmd: &str, k: &str) -> Result<Option<WarpMode>> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => match x.as_str() {
            Some("hueSat") => Ok(Some(WarpMode::HueSat)),
            Some("chromaLuma") => Ok(Some(WarpMode::ChromaLuma)),
            _ => Err(bad(cmd, format!("`{k}` must be \"hueSat\" or \"chromaLuma\""))),
        },
    }
}

/// An optional finite number.
fn num(p: &Value, k: &str, cmd: &str) -> Result<Option<f32>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        // (clamped before the cast: -1e300 must not become -∞)
        Some(v) => v.as_f64().filter(|f| f.is_finite()).map(|f| Some(f.clamp(-1e6, 1e6) as f32)).ok_or_else(|| bad(cmd, format!("`{k}` must be a number"))),
    }
}

/// An optional grid size within `range`.
fn size(p: &Value, k: &str, range: std::ops::RangeInclusive<u32>, cmd: &str) -> Result<Option<u32>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| range.contains(n))
            .map(Some)
            .ok_or_else(|| bad(cmd, format!("`{k}` must be a whole number from {} to {}", range.start(), range.end()))),
    }
}

fn index_p(p: &Value, cmd: &str) -> Result<u32> {
    p.get("index").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad(cmd, "need `index` (a point of the mesh)"))
}

fn optional_bool(p: &Value, k: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| bad(cmd, format!("`{k}` must be true or false"))),
    }
}

fn result(c: u64, node: u64, w: &Option<Warper>) -> Value {
    json!({"clip": c, "node": node, "warper": w})
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.warper.set";
    let whole = match p.get("warper") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value::<Warper>(v.clone()).map_err(|e| bad(CMD, format!("`warper` is not a colour warper: {e}")))?),
    };
    let mode = mode_of(p, CMD, "mode")?;
    let (mut strength, mut range) = (num(p, "strength", CMD)?, num(p, "range", CMD)?);
    let preserve = optional_bool(p, "preserveLuma", CMD)?;
    // (strength / range made keyframable, a node parameter, are set there: fork LO.3)
    if whole.is_none() && (strength.is_some() || range.is_some()) {
        let mut vals = [("warp.strength", strength), ("warp.range", range)];
        if crate::grade::set_keyed(s, p, CMD, &mut vals)? {
            [strength, range] = vals.map(|(_, v)| v);
            let rest = [range.is_some(), strength.is_some(), preserve.is_some(), p.get("mode").is_some_and(|m| !m.is_null())]
                .into_iter()
                .chain(["hueColumns", "satRings", "chromaColumns", "lumaRows"].map(|k| p.get(k).is_some_and(|m| !m.is_null())))
                .any(|b| b);
            if !rest {
                let (c, current) = current_for(s, p, CMD)?;
                let (id, w) = peek(s, c, node_arg(p, CMD)?, current, CMD)?;
                return Ok(result(c.0, id, &w));
            }
        }
    }
    let hue_cols = size(p, "hueColumns", HUE_COLUMNS, CMD)?;
    let rings = size(p, "satRings", SAT_RINGS, CMD)?;
    let cl_cols = size(p, "chromaColumns", CL_SIDE, CMD)?;
    let luma_rows = size(p, "lumaRows", CL_SIDE, CMD)?;
    let any = [strength.is_some(), range.is_some(), preserve.is_some(), mode.is_some()].into_iter().any(|b| b)
        || [hue_cols, rings, cl_cols, luma_rows].iter().any(Option::is_some);
    if whole.is_none() && !any {
        return Err(bad(CMD, "need `warper` or one of `mode`, `strength`, `range`, `preserveLuma`, `hueColumns`, `satRings`, `chromaColumns`, `lumaRows`"));
    }
    let merge = str_p(p, "merge").map(str::to_string);
    let (c, id, w) = edit_warper(s, p, CMD, "Change Colour Warper", true, merge.as_deref(), |slot| {
        if let Some(whole) = &whole {
            *slot = Some(whole.clone());
        }
        let w = slot.as_mut().ok_or_else(|| bad(CMD, "no warper"))?;
        if let Some(m) = mode {
            w.mode = m;
        }
        if let Some(v) = strength {
            w.strength = v;
        }
        if let Some(v) = range {
            w.range = v;
        }
        if let Some(v) = preserve {
            w.preserve_luma = v;
        }
        // (a new grid size starts that mesh afresh: points sit elsewhere on another grid)
        let mut resize = |mode: WarpMode, cols: Option<u32>, rows: Option<u32>| {
            let m = w.mesh_mut(mode);
            let (nc, nr) = (cols.unwrap_or(m.cols), rows.unwrap_or(m.rows));
            if (nc, nr) != (m.cols, m.rows) {
                m.cols = nc;
                m.rows = nr;
                m.points.clear();
            }
        };
        resize(WarpMode::HueSat, hue_cols, rings);
        resize(WarpMode::ChromaLuma, cl_cols, luma_rows);
        Ok(())
    })?;
    Ok(result(c, id, &w))
}

fn move_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.warper.movePoint";
    let mesh = mode_of(p, CMD, "mesh")?;
    let index = index_p(p, CMD)?;
    let to = p
        .get("to")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 2)
        .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]))
        .filter(|t| t.iter().all(|v| v.is_finite()))
        .ok_or_else(|| bad(CMD, "need `to`: [u, v], two finite numbers"))?;
    let to = to.map(|v| v.clamp(-1e6, 1e6) as f32);
    let merge = str_p(p, "merge").map(str::to_string);
    let (c, id, w) = edit_warper(s, p, CMD, "Move Warper Point", true, merge.as_deref(), |slot| {
        let w = slot.as_mut().ok_or_else(|| bad(CMD, "no warper"))?;
        let mode = mesh.unwrap_or(w.mode);
        let m = w.mesh_mut(mode);
        if m.is_pinned(index) {
            return Err(bad(CMD, format!("point {index} is pinned: unpin it first (grade.warper.pin)")));
        }
        if !m.set_point(mode, index, to, None) {
            return Err(bad(CMD, format!("the mesh has no point {index} ({} points)", m.len())));
        }
        Ok(())
    })?;
    Ok(result(c, id, &w))
}

fn pin(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.warper.pin";
    let mesh = mode_of(p, CMD, "mesh")?;
    let index = index_p(p, CMD)?;
    let on = optional_bool(p, "on", CMD)?;
    let (c, id, w) = edit_warper(s, p, CMD, "Pin Warper Point", true, None, |slot| {
        let w = slot.as_mut().ok_or_else(|| bad(CMD, "no warper"))?;
        let mode = mesh.unwrap_or(w.mode);
        let m = w.mesh_mut(mode);
        let at = m.position(mode, index).ok_or_else(|| bad(CMD, format!("the mesh has no point {index} ({} points)", m.len())))?;
        let on = on.unwrap_or(!m.is_pinned(index));
        m.set_point(mode, index, at, Some(on));
        Ok(())
    })?;
    Ok(result(c, id, &w))
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "grade.warper.reset";
    let mesh = mode_of(p, CMD, "mesh")?;
    let index = match p.get("index") {
        None | Some(Value::Null) => None,
        Some(_) => Some(index_p(p, CMD)?),
    };
    let (c, id, w) = edit_warper(s, p, CMD, "Reset Colour Warper", false, None, |slot| {
        if mesh.is_none() && index.is_none() {
            // the whole warper
            return match slot.take() {
                Some(_) => Ok(()),
                None => Err(bad(CMD, "the node has no colour warper")),
            };
        }
        let Some(w) = slot.as_mut() else {
            return Err(bad(CMD, "the node has no colour warper"));
        };
        match (mesh, index) {
            (m, None) => {
                let m = w.mesh_mut(m.unwrap_or(w.mode));
                m.points.clear();
            }
            (m, Some(i)) => {
                let mode = m.unwrap_or(w.mode);
                let m = w.mesh_mut(mode);
                if m.original(mode, i).is_none() {
                    return Err(bad(CMD, format!("the mesh has no point {i} ({} points)", m.len())));
                }
                let keep: Vec<WarpPoint> = m.points.iter().copied().filter(|p| p.index != i).collect();
                m.points = keep;
            }
        }
        Ok(())
    })?;
    Ok(result(c, id, &w))
}
