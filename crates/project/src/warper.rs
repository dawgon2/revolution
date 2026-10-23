//! The colour warper on a grade node (fork GRADE-PLAN G4.1), modelled on Resolve's Color Warper:
//! two meshes of control points the colourist drags, a hue × saturation mesh (drawn as a disc)
//! and a chroma × luma mesh (saturation across, luma up). Moving a point warps the colours
//! around it smoothly; the render compiles both meshes into displacement fields
//! (`filmcraft_color::warp`).
//!
//! **Mesh geometry.** Point `index` of a mesh is column `index % cols`, row `index / cols`.
//! Hue-sat: column `i` sits at hue `i / cols` (wrapping), ring `j` at saturation
//! `(j + 1) / rings` (the centre, saturation 0, is fixed and not a point). Chroma-luma: column `i`
//! at saturation `i / (cols − 1)`, row `j` at luma `j / (rows − 1)`. A point's original position
//! follows from its index ([`WarpMesh::original`]); only the points that were moved or pinned are
//! stored ([`WarpPoint`]), so an untouched mesh stores nothing.
//!
//! Every number is hostile: loading cleans it ([`Warper::cleaned`]): grid sizes clamped to their
//! ranges, points outside the mesh or non-finite dropped, duplicates merged (the last wins),
//! positions clamped to the plane, strength / range clamped.

use serde::{Deserialize, Serialize};

/// Hue columns of a hue-sat mesh.
pub const HUE_COLUMNS: std::ops::RangeInclusive<u32> = 6..=16;
/// Saturation rings of a hue-sat mesh.
pub const SAT_RINGS: std::ops::RangeInclusive<u32> = 4..=8;
/// Saturation columns and luma rows of a chroma-luma mesh.
pub const CL_SIDE: std::ops::RangeInclusive<u32> = 4..=8;
/// Largest falloff range, in mesh steps.
pub const MAX_RANGE: f32 = 4.0;

/// Which mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WarpMode {
    /// Hue × saturation (the disc).
    #[default]
    HueSat,
    /// Saturation × luma.
    ChromaLuma,
}

/// A point that was moved or pinned.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpPoint {
    /// Index in its mesh (module docs).
    pub index: u32,
    /// Where it was moved to, in plane units (hue-sat: [hue, saturation]; chroma-luma:
    /// [saturation, luma]); its original position when only pinned.
    pub to: [f32; 2],
    /// Pinned: it stays where it is when neighbours move with a falloff range.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// One mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpMesh {
    /// Points across (hue columns / saturation columns).
    pub cols: u32,
    /// Points down (saturation rings / luma rows).
    pub rows: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<WarpPoint>,
}

impl WarpMesh {
    /// An untouched hue-sat mesh (8 hue columns, 4 rings).
    pub fn hue_sat() -> Self {
        Self { cols: 8, rows: 4, points: Vec::new() }
    }

    /// An untouched chroma-luma mesh (5 × 5).
    pub fn chroma_luma() -> Self {
        Self { cols: 5, rows: 5, points: Vec::new() }
    }

    /// Number of points.
    pub fn len(&self) -> u32 {
        self.cols.saturating_mul(self.rows)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The original position of point `index` in `mode`'s plane (module docs); None outside.
    pub fn original(&self, mode: WarpMode, index: u32) -> Option<[f32; 2]> {
        if self.cols < 2 || self.rows < 2 || index >= self.len() {
            return None;
        }
        let (i, j) = (index % self.cols, index / self.cols);
        Some(match mode {
            WarpMode::HueSat => [i as f32 / self.cols as f32, (j + 1) as f32 / self.rows as f32],
            WarpMode::ChromaLuma => [i as f32 / (self.cols - 1) as f32, j as f32 / (self.rows - 1) as f32],
        })
    }

    /// Where point `index` is now (moved or original).
    pub fn position(&self, mode: WarpMode, index: u32) -> Option<[f32; 2]> {
        let orig = self.original(mode, index)?;
        Some(self.points.iter().find(|p| p.index == index).map_or(orig, |p| p.to))
    }

    pub fn is_pinned(&self, index: u32) -> bool {
        self.points.iter().any(|p| p.index == index && p.pinned)
    }

    /// The displacement of each moved point (plane units; hue the short way round), for the
    /// compiler.
    pub fn moves(&self, mode: WarpMode) -> Vec<(u32, [f32; 2])> {
        self.points
            .iter()
            .filter_map(|p| {
                let o = self.original(mode, p.index)?;
                let mut d = [p.to[0] - o[0], p.to[1] - o[1]];
                match mode {
                    WarpMode::HueSat => d[0] = wrap_half(d[0]),
                    // (grey has no hue to push: the saturation-0 column only moves in luma)
                    WarpMode::ChromaLuma if p.index % self.cols == 0 => d[0] = 0.0,
                    WarpMode::ChromaLuma => {}
                }
                (d[0] != 0.0 || d[1] != 0.0).then_some((p.index, d))
            })
            .collect()
    }

    /// Whether any point is moved.
    pub fn is_identity(&self, mode: WarpMode) -> bool {
        self.moves(mode).is_empty()
    }

    /// Move point `index` to `to` (cleaned); a point back at its place and not pinned is
    /// forgotten. False: no such point or `to` not finite.
    pub fn set_point(&mut self, mode: WarpMode, index: u32, to: [f32; 2], pinned: Option<bool>) -> bool {
        let Some(orig) = self.original(mode, index) else {
            return false;
        };
        let Some(to) = clean_to(mode, to) else {
            return false;
        };
        let was = self.points.iter().position(|p| p.index == index);
        let pinned = pinned.unwrap_or_else(|| was.and_then(|i| self.points.get(i)).is_some_and(|p| p.pinned));
        if was.is_some() {
            self.points.retain(|p| p.index != index);
        }
        if pinned || !same(mode, to, orig) {
            self.points.push(WarpPoint { index, to, pinned });
            self.points.sort_by_key(|p| p.index);
        }
        true
    }

    /// The mesh cleaned for `mode` (module docs).
    pub fn cleaned(self, mode: WarpMode) -> Self {
        let (cr, rr) = match mode {
            WarpMode::HueSat => (HUE_COLUMNS, SAT_RINGS),
            WarpMode::ChromaLuma => (CL_SIDE, CL_SIDE),
        };
        let mut out = Self { cols: self.cols.clamp(*cr.start(), *cr.end()), rows: self.rows.clamp(*rr.start(), *rr.end()), points: Vec::new() };
        // (a resized mesh would put points elsewhere: they only survive an unchanged size)
        if out.cols != self.cols || out.rows != self.rows {
            return out;
        }
        for p in self.points.into_iter().take(out.len() as usize * 4) {
            out.set_point(mode, p.index, p.to, Some(p.pinned));
        }
        out
    }
}

/// `d` wrapped into −½…½.
fn wrap_half(d: f32) -> f32 {
    d - (d + 0.5).floor()
}

fn same(mode: WarpMode, a: [f32; 2], b: [f32; 2]) -> bool {
    let du = if mode == WarpMode::HueSat { wrap_half(a[0] - b[0]) } else { a[0] - b[0] };
    du.abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

/// A position from untrusted numbers: None unless finite; hue wrapped into 0…1, the rest clamped
/// to 0…1.
pub fn clean_to(mode: WarpMode, to: [f32; 2]) -> Option<[f32; 2]> {
    if !to.iter().all(|v| v.is_finite()) {
        return None;
    }
    let u = match mode {
        WarpMode::HueSat => {
            let h = to[0] - to[0].floor();
            if h >= 1.0 { 0.0 } else { h }
        }
        WarpMode::ChromaLuma => to[0].clamp(0.0, 1.0),
    };
    Some([u, to[1].clamp(0.0, 1.0)])
}

/// A node's colour warper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warper {
    /// The mesh the panel shows (both meshes always apply: hue-sat first).
    #[serde(default)]
    pub mode: WarpMode,
    #[serde(default = "WarpMesh::hue_sat")]
    pub hue_sat: WarpMesh,
    #[serde(default = "WarpMesh::chroma_luma")]
    pub chroma_luma: WarpMesh,
    /// Scales every move (0…1).
    #[serde(default = "one")]
    pub strength: f32,
    /// How far a move pulls the free points around it, in mesh steps (0…[`MAX_RANGE`]; 0: the
    /// neighbours stay put, the default).
    #[serde(default)]
    pub range: f32,
    /// Hue-sat moves keep each colour's luma.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preserve_luma: bool,
}

impl Default for Warper {
    fn default() -> Self {
        Self { mode: WarpMode::HueSat, hue_sat: WarpMesh::hue_sat(), chroma_luma: WarpMesh::chroma_luma(), strength: 1.0, range: 0.0, preserve_luma: false }
    }
}

/// `v` clamped to `lo..=hi`; ±∞ go to the bound on their side, NaN to `default`.
fn clean_num(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_nan() { default } else { v.clamp(lo, hi) }
}

fn one() -> f32 {
    1.0
}

impl Warper {
    pub fn mesh(&self, mode: WarpMode) -> &WarpMesh {
        match mode {
            WarpMode::HueSat => &self.hue_sat,
            WarpMode::ChromaLuma => &self.chroma_luma,
        }
    }

    pub fn mesh_mut(&mut self, mode: WarpMode) -> &mut WarpMesh {
        match mode {
            WarpMode::HueSat => &mut self.hue_sat,
            WarpMode::ChromaLuma => &mut self.chroma_luma,
        }
    }

    /// The strength as [`Warper::cleaned`] would load it.
    pub fn effective_strength(&self) -> f32 {
        clean_num(self.strength, 0.0, 1.0, 1.0)
    }

    /// The falloff range as [`Warper::cleaned`] would load it.
    pub fn effective_range(&self) -> f32 {
        clean_num(self.range, 0.0, MAX_RANGE, 0.0)
    }

    /// Whether it changes nothing (no point moved, or strength 0; strength as loaded).
    pub fn is_identity(&self) -> bool {
        self.effective_strength() <= 0.0 || (self.hue_sat.is_identity(WarpMode::HueSat) && self.chroma_luma.is_identity(WarpMode::ChromaLuma))
    }

    /// The warper with hostile values removed (module docs).
    pub fn cleaned(self) -> Self {
        Self {
            mode: self.mode,
            strength: self.effective_strength(),
            range: self.effective_range(),
            hue_sat: self.hue_sat.cleaned(WarpMode::HueSat),
            chroma_luma: self.chroma_luma.cleaned(WarpMode::ChromaLuma),
            preserve_luma: self.preserve_luma,
        }
    }
}

/// Deserialize a node's warper, cleaned. A malformed one loads as no warper rather than failing
/// the whole project.
pub(crate) fn de_warper<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Warper>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value::<Warper>(v).ok()).map(Warper::cleaned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{Grade, GradeNode};

    #[test]
    fn old_nodes_have_no_warper_and_it_round_trips() {
        let json = r#"{"nodes":[{"id":1,"params":{}}]}"#;
        let g: Grade = serde_json::from_str(json).unwrap();
        assert_eq!(g.nodes[0].warper, None);
        let out = serde_json::to_string(&g).unwrap();
        assert!(!out.contains("warper"), "{out}");
        assert_eq!(serde_json::to_string(&serde_json::from_str::<Grade>(&out).unwrap()).unwrap(), out, "saves byte-identical");
        let mut n = GradeNode::new(2);
        let mut w = Warper { strength: 0.7, range: 1.5, preserve_luma: true, mode: WarpMode::ChromaLuma, ..Warper::default() };
        assert!(w.hue_sat.set_point(WarpMode::HueSat, 9, [0.2, 0.4], None));
        assert!(w.chroma_luma.set_point(WarpMode::ChromaLuma, 3, [0.75, 0.0], Some(true)));
        n.warper = Some(w);
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"warper\""), "{json}");
        assert_eq!(serde_json::from_str::<GradeNode>(&json).unwrap(), n);
    }

    #[test]
    fn moving_back_forgets_and_hue_wraps_the_short_way() {
        let mut m = WarpMesh::hue_sat();
        assert!(m.set_point(WarpMode::HueSat, 0, [0.95, 0.25], None));
        assert_eq!(m.moves(WarpMode::HueSat).len(), 1);
        let d = m.moves(WarpMode::HueSat)[0].1;
        assert!((d[0] + 0.05).abs() < 1e-6, "{d:?}");
        assert!(m.set_point(WarpMode::HueSat, 0, [1.0, 0.25], None));
        assert!(m.points.is_empty(), "back at its place");
        assert!(!m.set_point(WarpMode::HueSat, 32, [0.0, 0.0], None), "outside the mesh");
        assert!(!m.set_point(WarpMode::HueSat, 1, [f32::NAN, 0.0], None));
        assert_eq!(m.position(WarpMode::HueSat, 31), Some([7.0 / 8.0, 1.0]));
    }

    #[test]
    fn hostile_warpers_load_cleaned() {
        let mut pts = String::from("[{\"index\":2,\"to\":[0.3,0.3],\"pinned\":true},{\"index\":4294967295,\"to\":[0.1,0.1]},{\"index\":3,\"to\":[1e40,0]},");
        for i in 0..500 {
            pts.push_str(&format!("{{\"index\":{},\"to\":[{},2.5]}},", 4 + i % 40, i as f32 * 0.37));
        }
        pts.push_str("{\"index\":5,\"to\":[-0.25,-1]}]");
        let json = format!(
            r#"{{"id":1,"warper":{{"hueSat":{{"cols":8,"rows":4,"points":{pts}}},"chromaLuma":{{"cols":4000000000,"rows":0,"points":[{{"index":1,"to":[0.5,0.5]}}]}},"strength":1e9,"range":-3}}}}"#
        );
        let n: GradeNode = serde_json::from_str(&json).unwrap();
        let w = n.warper.unwrap();
        assert_eq!(w.strength, 1.0);
        assert_eq!(w.range, 0.0);
        assert!(w.hue_sat.points.len() <= 32);
        assert!(w.hue_sat.points.iter().all(|p| p.index < 32 && (0.0..1.0).contains(&p.to[0]) && (0.0..=1.0).contains(&p.to[1])));
        assert!(w.hue_sat.is_pinned(2));
        assert_eq!((w.chroma_luma.cols, w.chroma_luma.rows), (8, 4));
        assert!(w.chroma_luma.points.is_empty(), "points of a resized mesh are dropped");
        // NaN strength is not representable in JSON; in memory it cleans to 1
        let w = Warper { strength: f32::NAN, range: f32::INFINITY, ..Warper::default() }.cleaned();
        assert_eq!((w.strength, w.range), (1.0, MAX_RANGE));
        let w = Warper { strength: f32::NEG_INFINITY, range: f32::NEG_INFINITY, ..Warper::default() }.cleaned();
        assert_eq!((w.strength, w.range), (0.0, 0.0));
        // is_identity reads the strength as loaded: NaN is full strength
        let mut moved = Warper { strength: f32::NAN, ..Warper::default() };
        moved.hue_sat.set_point(WarpMode::HueSat, 1, [0.4, 0.25], None);
        assert!(!moved.is_identity());
        assert!(!moved.clone().cleaned().is_identity());
        // a hue push on chroma-luma's grey column is no move
        let mut grey = Warper::default();
        grey.chroma_luma.set_point(WarpMode::ChromaLuma, 5, [0.5, 0.25], None);
        assert!(grey.is_identity());
        // a malformed warper loads as none, the node still loads
        let n: GradeNode = serde_json::from_str(r#"{"id":1,"warper":{"hueSat":"nonsense"}}"#).unwrap();
        assert_eq!(n.warper, None);
        let n: GradeNode = serde_json::from_str(r#"{"id":1,"warper":{}}"#).unwrap();
        assert_eq!(n.warper, Some(Warper::default()));
        assert!(Warper::default().is_identity());
    }
}
