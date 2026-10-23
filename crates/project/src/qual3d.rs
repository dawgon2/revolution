//! The 3D qualifier on a grade node (fork GRADE-PLAN G4.3), modelled on Resolve's 3D qualifier:
//! strokes drawn on the picture over the colours to select (include) or to leave out (exclude).
//! The colours under a stroke are sampled from the node's input and reduced to clusters: sphere
//! centres in the key's opponent colour space (`filmcraft_color::key3d`) with a radius (how far
//! the samples spread). The render keys the union of the include spheres minus the exclude ones,
//! with a soft edge ([`Qual3d::softness`]), and can pull the picked colour's chroma out of the
//! keyed pixels ([`Qual3d::despill`]).
//!
//! The node's other qualifier settings stay shared with the HSL qualifier: on (`qual.on`),
//! invert, matte blur and the highlight view. [`Qual3d::mode`] says which key the node uses; a
//! node without a `qual3d` uses the HSL one, so old projects load and save unchanged.
//!
//! Every number is hostile: loading cleans it ([`Qual3d::cleaned`]).

use serde::{Deserialize, Serialize};

/// Most clusters on each side (include, exclude).
pub const MAX_CLUSTERS: usize = 64;
/// Most colour samples one stroke contributes.
pub const MAX_SAMPLES: usize = 4096;
/// A sample this close (opponent units) to a cluster's centre joins it.
pub const MERGE_RADIUS: f32 = 0.03;
/// Largest cluster radius.
pub const MAX_RADIUS: f32 = 0.25;
/// Farthest a cluster centre may be from the origin on any axis (the opponent coordinates of 0…1
/// colours are within ±0.9).
pub const MAX_COORD: f32 = 2.0;
/// Softness range and default (opponent units).
pub const SOFTNESS: std::ops::RangeInclusive<f32> = 0.001..=0.5;
pub const DEFAULT_SOFTNESS: f32 = 0.05;
/// Most stroke paths kept to draw on the Program picture (fork LO.3; the oldest go first).
pub const MAX_PATHS: usize = 32;
/// Most points a kept stroke path has (a longer stroke is thinned evenly).
pub const MAX_PATH_POINTS: usize = 64;

/// Where a stroke was painted (fork LO.3), kept only to draw it on the Program picture: the
/// key itself is the clusters. Points are 0…1 of the sequence frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePath {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude: bool,
    #[serde(default)]
    pub pts: Vec<[f32; 2]>,
}

impl StrokePath {
    /// A path of `pts` (non-finite points dropped, the rest clamped to 0…1, evenly thinned to
    /// [`MAX_PATH_POINTS`]); None without a point.
    pub fn new(pts: &[[f32; 2]], exclude: bool) -> Option<Self> {
        let good: Vec<[f32; 2]> = pts.iter().filter(|p| p.iter().all(|v| v.is_finite())).map(|p| p.map(|v| v.clamp(0.0, 1.0))).collect();
        let n = good.len();
        if n == 0 {
            return None;
        }
        let pts = if n <= MAX_PATH_POINTS {
            good
        } else {
            (0..MAX_PATH_POINTS).filter_map(|i| good.get(i.saturating_mul(n - 1) / (MAX_PATH_POINTS - 1)).copied()).collect()
        };
        Some(Self { exclude, pts })
    }

    fn cleaned(self) -> Option<Self> {
        Self::new(&self.pts, self.exclude)
    }
}

/// Which key a node's qualifier uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QualMode {
    /// The HSL qualifier (`qual.*` ranges).
    #[default]
    #[serde(rename = "hsl")]
    Hsl,
    /// The 3D qualifier (this module).
    #[serde(rename = "3d")]
    ThreeD,
}

/// A cluster of picked colours.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    /// Centre in the opponent space.
    pub c: [f32; 3],
    /// Radius: the samples' approximate spread (an upper bound kept while merging).
    #[serde(default)]
    pub r: f32,
    /// How many samples it holds (its weight when merging).
    #[serde(default = "one")]
    pub n: u32,
}

fn one() -> u32 {
    1
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Cluster {
    /// The cluster from untrusted numbers: None unless finite; centre clamped to ±[`MAX_COORD`],
    /// radius to 0…[`MAX_RADIUS`], weight at least 1.
    pub fn cleaned(self) -> Option<Self> {
        if !(self.c.iter().all(|v| v.is_finite()) && !self.r.is_nan()) {
            return None;
        }
        Some(Self { c: self.c.map(|v| v.clamp(-MAX_COORD, MAX_COORD)), r: self.r.clamp(0.0, MAX_RADIUS), n: self.n.max(1) })
    }

    /// The two merged: the weighted mean centre, a radius covering both.
    fn merged(a: Cluster, b: Cluster) -> Cluster {
        let (wa, wb) = (a.n as f32, b.n as f32);
        let w = (wa + wb).max(1.0);
        let c = [0, 1, 2].map(|k| (a.c[k] * wa + b.c[k] * wb) / w);
        let r = (dist(a.c, c) + a.r).max(dist(b.c, c) + b.r).clamp(0.0, MAX_RADIUS);
        Cluster { c, r, n: a.n.saturating_add(b.n) }
    }
}

/// A node's 3D qualifier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Qual3d {
    /// Which key the node uses.
    #[serde(default)]
    pub mode: QualMode,
    /// Colours to select.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<Cluster>,
    /// Colours to leave out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Cluster>,
    /// Width of the soft edge around the volume ([`SOFTNESS`]).
    #[serde(default = "default_softness")]
    pub softness: f32,
    /// How much of the picked colour's chroma the node pulls out of the pixels it corrects (0…1).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub despill: f32,
    /// The strokes' paths on the picture, oldest first (fork LO.3; at most [`MAX_PATHS`]), to
    /// draw them; projects before LO.3 have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<StrokePath>,
}

fn default_softness() -> f32 {
    DEFAULT_SOFTNESS
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Default for Qual3d {
    fn default() -> Self {
        Self { mode: QualMode::ThreeD, include: Vec::new(), exclude: Vec::new(), softness: DEFAULT_SOFTNESS, despill: 0.0, paths: Vec::new() }
    }
}

/// `v` clamped to `lo..=hi`; ±∞ go to the bound on their side, NaN to `default`.
fn clean_num(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_nan() { default } else { v.clamp(lo, hi) }
}

impl Qual3d {
    /// Whether the node keys with it.
    pub fn is_active(&self) -> bool {
        self.mode == QualMode::ThreeD
    }

    /// The softness as [`Qual3d::cleaned`] would load it.
    pub fn effective_softness(&self) -> f32 {
        clean_num(self.softness, *SOFTNESS.start(), *SOFTNESS.end(), DEFAULT_SOFTNESS)
    }

    /// The despill amount as [`Qual3d::cleaned`] would load it.
    pub fn effective_despill(&self) -> f32 {
        clean_num(self.despill, 0.0, 1.0, 0.0)
    }

    pub fn side(&self, exclude: bool) -> &Vec<Cluster> {
        if exclude { &self.exclude } else { &self.include }
    }

    fn side_mut(&mut self, exclude: bool) -> &mut Vec<Cluster> {
        if exclude { &mut self.exclude } else { &mut self.include }
    }

    /// Keep a stroke's path to draw it (the oldest past [`MAX_PATHS`] dropped).
    pub fn add_path(&mut self, pts: &[[f32; 2]], exclude: bool) {
        if let Some(p) = StrokePath::new(pts, exclude) {
            self.paths.push(p);
        }
        let extra = self.paths.len().saturating_sub(MAX_PATHS);
        self.paths.drain(..extra);
    }

    /// Add picked colours (opponent coordinates) to one side: each finite sample (at most
    /// [`MAX_SAMPLES`]) joins the nearest cluster within [`MERGE_RADIUS`] of it or starts a new
    /// one; past [`MAX_CLUSTERS`] the two closest clusters merge. Returns the samples used.
    pub fn add_samples(&mut self, samples: &[[f32; 3]], exclude: bool) -> usize {
        let side = self.side_mut(exclude);
        let mut used = 0;
        for s in samples.iter().filter(|s| s.iter().all(|v| v.is_finite())).take(MAX_SAMPLES) {
            let s = s.map(|v| v.clamp(-MAX_COORD, MAX_COORD));
            used += 1;
            let near = side.iter_mut().map(|c| (dist(c.c, s), c)).filter(|(d, _)| *d <= MERGE_RADIUS).min_by(|a, b| a.0.total_cmp(&b.0));
            match near {
                Some((_, c)) => {
                    let n = c.n.saturating_add(1);
                    let w = 1.0 / n as f32;
                    c.c = [0, 1, 2].map(|k| c.c[k] + (s[k] - c.c[k]) * w);
                    c.r = c.r.max(dist(c.c, s)).min(MAX_RADIUS);
                    c.n = n;
                }
                None => {
                    side.push(Cluster { c: s, r: 0.0, n: 1 });
                    if side.len() > MAX_CLUSTERS {
                        merge_closest(side);
                    }
                }
            }
        }
        used
    }

    /// The direction (opponent chroma `[a, b]`) of the picked colour: the weighted mean of the
    /// include clusters' chroma.
    pub fn key_chroma(&self) -> [f32; 2] {
        let (mut a, mut b, mut w) = (0.0f32, 0.0f32, 0.0f32);
        for c in &self.include {
            let n = c.n as f32;
            a += c.c[1] * n;
            b += c.c[2] * n;
            w += n;
        }
        if w > 0.0 { [a / w, b / w] } else { [0.0, 0.0] }
    }

    /// The 3D qualifier with hostile values removed: clusters cleaned (non-finite dropped, at
    /// most [`MAX_CLUSTERS`] a side), softness and despill clamped, stroke paths cleaned (the
    /// newest [`MAX_PATHS`]).
    pub fn cleaned(self) -> Self {
        let side = |v: Vec<Cluster>| v.into_iter().filter_map(Cluster::cleaned).take(MAX_CLUSTERS).collect();
        let skip = self.paths.len().saturating_sub(MAX_PATHS);
        Self {
            mode: self.mode,
            softness: self.effective_softness(),
            despill: self.effective_despill(),
            include: side(self.include),
            exclude: side(self.exclude),
            paths: self.paths.into_iter().skip(skip).filter_map(StrokePath::cleaned).collect(),
        }
    }
}

/// Merge the two closest clusters of `side` (O(n²), n ≤ [`MAX_CLUSTERS`] + 1).
fn merge_closest(side: &mut Vec<Cluster>) {
    let mut best: Option<(f32, usize, usize)> = None;
    for i in 0..side.len() {
        for j in i + 1..side.len() {
            let (Some(a), Some(b)) = (side.get(i), side.get(j)) else { continue };
            let d = dist(a.c, b.c);
            if best.is_none_or(|(bd, ..)| d < bd) {
                best = Some((d, i, j));
            }
        }
    }
    let Some((_, i, j)) = best else { return };
    let b = side.remove(j);
    if let Some(a) = side.get_mut(i) {
        *a = Cluster::merged(*a, b);
    }
}

/// Deserialize a node's 3D qualifier, cleaned. A malformed cluster is dropped (the mode and the
/// good clusters survive); a qualifier that is malformed otherwise loads as none rather than
/// failing the whole project.
pub(crate) fn de_qual3d<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Qual3d>, D::Error> {
    let Some(mut v) = Option::<serde_json::Value>::deserialize(d)? else { return Ok(None) };
    let mut sides: [Vec<Cluster>; 2] = [Vec::new(), Vec::new()];
    let mut paths: Vec<StrokePath> = Vec::new();
    if let Some(o) = v.as_object_mut() {
        for (side, key) in sides.iter_mut().zip(["include", "exclude"]) {
            if let Some(serde_json::Value::Array(items)) = o.remove(key) {
                // (bounded: more than a side keeps are never read)
                *side = items.into_iter().filter_map(|c| serde_json::from_value::<Cluster>(c).ok()).filter_map(Cluster::cleaned).take(MAX_CLUSTERS).collect();
            }
        }
        // (a malformed path is dropped; only the newest MAX_PATHS are read)
        if let Some(serde_json::Value::Array(items)) = o.remove("paths") {
            let skip = items.len().saturating_sub(MAX_PATHS);
            paths = items.into_iter().skip(skip).filter_map(|p| serde_json::from_value::<StrokePath>(p).ok()).collect();
        }
    }
    let Ok(mut q) = serde_json::from_value::<Qual3d>(v) else { return Ok(None) };
    let [include, exclude] = sides;
    q.include = include;
    q.exclude = exclude;
    q.paths = paths;
    Ok(Some(q.cleaned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{Grade, GradeNode};

    #[test]
    fn old_nodes_have_no_3d_qualifier_and_it_round_trips() {
        let json = r#"{"nodes":[{"id":1,"params":{}}]}"#;
        let g: Grade = serde_json::from_str(json).unwrap();
        assert_eq!(g.nodes[0].qual3d, None);
        let out = serde_json::to_string(&g).unwrap();
        assert!(!out.contains("qual3d"), "{out}");
        assert_eq!(serde_json::to_string(&serde_json::from_str::<Grade>(&out).unwrap()).unwrap(), out, "saves byte-identical");
        let mut n = GradeNode::new(2);
        let mut q = Qual3d { despill: 0.4, softness: 0.1, ..Qual3d::default() };
        q.add_samples(&[[0.3, 0.2, -0.1], [0.31, 0.2, -0.1]], false);
        q.add_samples(&[[0.3, -0.2, 0.1]], true);
        n.qual3d = Some(q);
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"qual3d\"") && json.contains("\"3d\""), "{json}");
        assert_eq!(serde_json::from_str::<GradeNode>(&json).unwrap(), n);
    }

    #[test]
    fn samples_cluster_and_the_count_is_capped() {
        let mut q = Qual3d::default();
        assert_eq!(q.add_samples(&[[0.3, 0.2, 0.0], [0.31, 0.2, 0.0], [0.3, 0.21, 0.0]], false), 3);
        assert_eq!(q.include.len(), 1);
        let c = q.include[0];
        assert_eq!(c.n, 3);
        assert!(c.r > 0.0 && c.r < MERGE_RADIUS, "{c:?}");
        // a spread of colours over the whole space: capped, and every sample still near a cluster
        let spread: Vec<[f32; 3]> = (0..3000).map(|i| [(i % 10) as f32 * 0.08, ((i / 10) % 10) as f32 * 0.1 - 0.5, (i / 100) as f32 * 0.03 - 0.45]).collect();
        let mut q = Qual3d::default();
        assert_eq!(q.add_samples(&spread, true), 3000);
        assert_eq!(q.exclude.len(), MAX_CLUSTERS);
        assert!(q.include.is_empty());
        assert!(q.exclude.iter().all(|c| c.r <= MAX_RADIUS));
        // hostile samples: NaN / ∞ skipped, absurd ones clamped, at most MAX_SAMPLES used
        let mut q = Qual3d::default();
        let bad = vec![[f32::NAN, 0.0, 0.0], [f32::INFINITY, 0.0, 0.0], [1e30, -1e30, 0.0]];
        assert_eq!(q.add_samples(&bad, false), 1);
        assert_eq!(q.include[0].c, [MAX_COORD, -MAX_COORD, 0.0]);
        let lots = vec![[0.1, 0.1, 0.1]; 10_000];
        assert_eq!(q.add_samples(&lots, false), MAX_SAMPLES);
    }

    #[test]
    fn hostile_qualifiers_load_cleaned() {
        let mut inc = String::from("[");
        for i in 0..300 {
            inc.push_str(&format!("{{\"c\":[{},0,0],\"r\":{},\"n\":0}},", i as f32 * 0.001, if i % 2 == 0 { 1e9 } else { -4.0 }));
        }
        inc.push_str("{\"c\":[1e39,0,0]}]");
        let json = format!(r#"{{"id":1,"qual3d":{{"mode":"3d","include":{inc},"exclude":[{{"c":[0,0]}}],"softness":-5,"despill":1e9}}}}"#);
        // a malformed cluster is dropped; the mode and the good clusters survive
        let n: GradeNode = serde_json::from_str(&json).unwrap();
        let q = n.qual3d.unwrap();
        assert_eq!((q.mode, q.include.len(), q.exclude.len()), (QualMode::ThreeD, MAX_CLUSTERS, 0));
        let n: GradeNode = serde_json::from_str(
            r#"{"id":1,"qual3d":{"mode":"3d","include":[{"c":[0.1,0.2,0.3],"r":0.01},"junk",{"c":[1,2]},{"c":[0.4,0.0,0.0]}],"exclude":7}}"#,
        )
        .unwrap();
        let q = n.qual3d.unwrap();
        assert_eq!((q.mode, q.include.len(), q.exclude.len()), (QualMode::ThreeD, 2, 0));
        let json = format!(r#"{{"id":1,"qual3d":{{"mode":"3d","include":{inc},"softness":-5,"despill":1e9}}}}"#);
        let n: GradeNode = serde_json::from_str(&json).unwrap();
        let q = n.qual3d.unwrap();
        assert_eq!(q.include.len(), MAX_CLUSTERS);
        assert!(q.include.iter().all(|c| c.n >= 1 && (0.0..=MAX_RADIUS).contains(&c.r) && c.c.iter().all(|v| v.abs() <= MAX_COORD)));
        assert_eq!((q.softness, q.despill), (*SOFTNESS.start(), 1.0));
        let q = Qual3d { softness: f32::NAN, despill: f32::NAN, ..Qual3d::default() }.cleaned();
        assert_eq!((q.softness, q.despill), (DEFAULT_SOFTNESS, 0.0));
        let n: GradeNode = serde_json::from_str(r#"{"id":1,"qual3d":{"mode":"rgb"}}"#).unwrap();
        assert_eq!(n.qual3d, None);
        let n: GradeNode = serde_json::from_str(r#"{"id":1,"qual3d":{}}"#).unwrap();
        assert_eq!(n.qual3d.map(|q| q.mode), Some(QualMode::Hsl), "a bare one keeps the HSL key");
    }

    /// Fork LO.3: stroke paths are kept (thinned, capped), round-trip, and load cleaned.
    #[test]
    fn stroke_paths_are_kept_capped_and_cleaned() {
        let mut q = Qual3d::default();
        let long: Vec<[f32; 2]> = (0..500).map(|i| [i as f32 / 500.0, 0.5]).collect();
        q.add_path(&long, false);
        q.add_path(&[[f32::NAN, 0.1], [2.0, -1.0]], true);
        q.add_path(&[[f32::NAN, 0.1]], true);
        assert_eq!(q.paths.len(), 2, "a path without a good point is not kept");
        assert_eq!(q.paths[0].pts.len(), MAX_PATH_POINTS);
        assert_eq!(q.paths[1], StrokePath { exclude: true, pts: vec![[1.0, 0.0]] });
        for _ in 0..MAX_PATHS {
            q.add_path(&[[0.2, 0.2]], false);
        }
        assert_eq!(q.paths.len(), MAX_PATHS);
        assert!(!q.paths.iter().any(|p| p.exclude), "the oldest went first");
        let n = GradeNode { qual3d: Some(q), ..GradeNode::new(1) };
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(serde_json::from_str::<GradeNode>(&json).unwrap(), n);
        // hostile: out-of-range points clamp, malformed paths drop, too many keep the newest
        let mut many: Vec<serde_json::Value> = (0..1000).map(|i| serde_json::json!({"pts": [[i as f64 / 1000.0, 5.0]]})).collect();
        many.push(serde_json::json!({"pts": "nope"}));
        many.push(serde_json::json!({"exclude": true, "pts": [[-3.0, 1e30]]}));
        let n: GradeNode = serde_json::from_value(serde_json::json!({"id": 1, "qual3d": {"mode": "3d", "softness": 0.1, "paths": many}})).unwrap();
        let q = n.qual3d.unwrap();
        assert_eq!(q.softness, 0.1, "the rest of the qualifier survives");
        assert_eq!(q.paths.len(), MAX_PATHS - 1);
        assert_eq!(q.paths.last(), Some(&StrokePath { exclude: true, pts: vec![[0.0, 1.0]] }));
        assert!(q.paths.iter().all(|p| p.pts.iter().all(|v| (0.0..=1.0).contains(&v[0]) && (0.0..=1.0).contains(&v[1]))));
    }
}
