//! Viewer tools for the Compositor's Roto, Paint, Tracker and Planar Tracker nodes (fork C3.9).
//!
//! While the Compositor is open and one such node is selected, its contents are drawn over the
//! Program monitor's picture and edited there, the way Resolve's Fusion page edits a Polygon /
//! B-Spline, Paint, Tracker or Planar Tracker node on its viewer (our own design and words, from
//! the public manual). Comp pixels map to the screen as for the Mask node's overlay
//! ([`crate::panels::comp::mask_overlay`]): through the comp clip's Motion, or straight onto the
//! picture while a node is viewed.
//!
//! The tools never change the project themselves: every edit is an existing command
//! (`comp.roto.addShape`, `comp.roto.setShapeKeyframe` at the playhead's comp time,
//! `comp.paint.addStroke`, `comp.track.setPoints`, `comp.planar.setRegion`), and a drag carries one
//! `gesture`, so it is one undo step. Tool state is `ui.panels.compViewer` (serde).
//!
//! Automation ids: `compViewer.overlay` (the picture), the tool bar `compViewer.roto.{select,
//! bezier,bSpline}`, `compViewer.paint.{color,clone,reveal,erase,size,hardness,opacity,swatch,
//! singleFrame}`, `compViewer.planar.{edit,draw}`; the handles `compViewer.roto<k>.point<i>`,
//! `compViewer.roto<k>.point<i>.{in,out}` (the selected shape's Bézier handles),
//! `compViewer.roto.featherIn` / `.featherOut` (the selected point's), `compViewer.draft.first`
//! (the first point of a shape or region being drawn), `compViewer.track.point<k>`,
//! `compViewer.track.pattern<k>`, `compViewer.track.search<k>`, `compViewer.planar.region<i>`,
//! `compViewer.planar.corner<k>`.

use egui::{Color32, Pos2, Rect, Sense, Stroke, UiBuilder, pos2, vec2};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::comp::Comp;
use filmcraft_project::comp::{CompKind, PATTERN_HALF, SEARCH_HALF, TrackerPoint};
use filmcraft_project::paint::{Brush, MAX_BRUSH_SIZE, MAX_STROKE_POINTS, PaintPoint, PaintStroke, PaintTool, StrokeDuration};
use filmcraft_project::roto::{MAX_FEATHER, RotoKind, RotoShape};
use filmcraft_project::{ItemId, ItemKind, MaskPath, ParamValue};
use filmcraft_render::comp::track::{TrackWarp, follow_warp, link_target, path_through};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// How a Roto node's viewer tool acts on a press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RotoTool {
    /// Pick and drag points, Bézier handles, feather handles or whole shapes.
    #[default]
    Select,
    /// Draw a new Bézier shape: click for a point, drag for its handles.
    Bezier,
    /// Draw a new B-spline shape: click for each control point.
    BSpline,
}

/// The paint brush the next stroke uses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrushState {
    pub tool: PaintTool,
    /// Diameter, comp pixels.
    pub size: f64,
    /// 0 soft .. 1 hard.
    pub hardness: f64,
    /// 0..1.
    pub opacity: f64,
    /// Linear RGBA, straight alpha (the Color tool).
    pub color: [f32; 4],
    /// Clone source offset (comp pixels) from the painted spot.
    pub clone_offset: [f64; 2],
    /// The clone source picked with Alt-click, waiting for the next stroke to set the offset.
    pub clone_source: Option<[f64; 2]>,
    /// The stroke shows on the playhead's frame only (else for the whole comp).
    pub single_frame: bool,
}

impl Default for BrushState {
    fn default() -> Self {
        let b = Brush::default();
        Self {
            tool: PaintTool::Color,
            size: b.size,
            hardness: b.hardness,
            opacity: b.opacity,
            color: [1.0, 1.0, 1.0, 1.0],
            clone_offset: [40.0, 0.0],
            clone_source: None,
            single_frame: false,
        }
    }
}

/// Points of a shape or region being drawn (comp pixels; `[x, y, tangent x, tangent y]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Draft {
    pub node: u64,
    pub points: Vec<[f64; 4]>,
}

/// `ui.panels.compViewer`: the viewer tools' state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompViewerState {
    /// The node the tools were last shown for (another node resets the tools).
    pub node: Option<u64>,
    /// The Roto tool; none = automatic (draw Bézier while the node has no shape, else Select).
    pub roto_tool: Option<RotoTool>,
    /// Planar Tracker: drawing a new region; none = automatic (while it has fewer than 3 points).
    pub draw_region: Option<bool>,
    /// The shape or region being drawn.
    pub draft: Option<Draft>,
    /// The selected roto point: `[shape, point]`.
    pub selected: Option<[usize; 2]>,
    pub brush: BrushState,
    /// The paint stroke being drawn (comp pixels).
    pub stroke: Vec<[f64; 2]>,
    /// The last drag's gesture number (one undo step per drag).
    pub gesture: u64,
}

/// Pick distance of a handle, screen points.
const PICK: f32 = 8.0;
/// Least screen offset, each way, of a tracker's pattern-box handle from its centre and of its
/// search-box handle from the pattern handle, so the three stay apart at any zoom (2·PICK apart
/// diagonally).
const TRACK_HANDLE_SEP: f32 = 12.0;
/// Screen distance of a feather handle from its point at zero feather.
const FEATHER_OFF: f32 = 14.0;
const COL: Color32 = Color32::from_rgb(0x5a, 0xb4, 0xff);
const FEATHER_COL: Color32 = Color32::from_rgb(0xff, 0xb0, 0x40);
const TRACK_COL: Color32 = Color32::from_rgb(0x60, 0xe0, 0x80);

/// Where the selected node's comp is drawn.
pub(crate) struct ViewMap {
    pub comp: ItemId,
    pub node: u64,
    /// The playhead's comp time.
    pub ct: Tick,
    /// The comp times the comp clip under the playhead shows.
    pub span: (Tick, Tick),
    pub to_screen: Affine,
    pub to_comp: Affine,
    /// Screen points per comp pixel.
    pub scale: f64,
}

impl ViewMap {
    fn sp(&self, v: Vec2) -> Pos2 {
        let q = self.to_screen.apply(v);
        pos2(q.x as f32, q.y as f32)
    }
    fn cp(&self, p: Pos2) -> Vec2 {
        self.to_comp.apply(Vec2::new(f64::from(p.x), f64::from(p.y)))
    }
}

/// The comp-to-screen mapping of the one selected comp node, when the Compositor is open and a
/// clip of its comp is under the playhead.
pub(crate) fn view_map(app: &FilmcraftApp, pic: Rect, frame: (u32, u32)) -> Option<ViewMap> {
    let kind = crate::dock::PanelKind::Compositor;
    if !app.ui.dock.contains(kind) && !app.ui.floating.iter().any(|g| g.panels.contains(&kind)) {
        return None;
    }
    let p = &app.ui.panels.comp;
    // (C4.8: inside a group the monitor shows the comp's own result; no overlay there yet)
    if !p.path.is_empty() {
        return None;
    }
    let (Some(comp), [node]) = (p.comp.map(ItemId), p.canvas.selection.as_slice()) else { return None };
    let node = *node;
    let ItemKind::Comp(c) = &app.session.project.item(comp)?.kind else { return None };
    c.node(node)?;
    let (cw, ch) = (c.width.max(1), c.height.max(1));
    let at = app.session.playhead();
    let seq = app.session.active_sequence()?;
    let it = seq.video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item == comp && i.range().contains(at))?;
    let ct = it.source_time_at(at);
    let (a, b) = (it.source_time_at(it.start), it.source_time_at(Tick(it.end().0.saturating_sub(1))));
    let to_screen = if crate::panels::comp::viewer_target(app).is_some() {
        Affine::translate(f64::from(pic.min.x), f64::from(pic.min.y))
            .then_apply(&Affine::scale(f64::from(pic.width()) / f64::from(cw), f64::from(pic.height()) / f64::from(ch)))
    } else {
        let motion = filmcraft_render::motion_matrix(seq, it, (cw, ch), None, it.effect_time_at(at));
        let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
        Affine::translate(f64::from(pic.min.x), f64::from(pic.min.y))
            .then_apply(&Affine::scale(f64::from(pic.width()) / fw, f64::from(pic.height()) / fh))
            .then_apply(&motion)
    };
    let to_comp = to_screen.inverse()?;
    let scale = (to_screen.apply(Vec2::new(1.0, 0.0)) - to_screen.apply(Vec2::ZERO)).length();
    if !scale.is_finite() || scale <= 1e-9 {
        return None;
    }
    Some(ViewMap { comp, node, ct, span: (a.min(b), a.max(b)), to_screen, to_comp, scale })
}

/// Runs a command; a refusal shows as a toast. The command's result when it ran.
fn run(app: &mut FilmcraftApp, command: &str, params: Value) -> Option<Value> {
    match app.session.execute(command, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.session.toast(e.to_string());
            None
        }
    }
}

/// A path as command JSON (`masks::path_from_json` reads it back with tangents and feathers).
fn path_json(path: &MaskPath) -> Value {
    filmcraft_engine::masks::path_to_json(path)
}

/// The motion of the tracker roto shape `sh` follows, from the tracker's reference frame to `t`
/// (fork C3.7: a linked shape's keyframes are stored in reference-frame coordinates and moved by
/// this at render time). None when it follows none, the link dangles or the solve fails there
/// (the shape is then drawn as stored), as the renderer's `linked_roto`.
fn shape_warp(c: &Comp, sh: &RotoShape, t: Tick) -> Option<TrackWarp> {
    let l = sh.track_link.as_ref()?;
    let kind = link_target(&l.tracker, |id| c.node(id).map(|n| &n.kind))?;
    follow_warp(kind, c.rate, t, (f64::from(c.width.max(1)), f64::from(c.height.max(1))), &l.output)
}

/// A shape's outline where it is drawn at the playhead: the stored path moved by its tracker.
fn shown_path(stored: &MaskPath, warp: Option<TrackWarp>) -> MaskPath {
    warp.map_or_else(|| stored.clone(), |w| path_through(stored, w))
}

/// An outline edited where it is drawn, back in the coordinates the shape stores (the tracker's
/// reference frame for a linked shape). None when the tracker's map can't be inverted there.
fn stored_path(shown: &MaskPath, warp: Option<TrackWarp>) -> Option<MaskPath> {
    let Some(w) = warp else { return Some(shown.clone()) };
    let back = path_through(shown, w.inverse()?);
    Some(MaskPath { vertices: back.vertices.iter().map(|v| v.cleaned()).collect(), closed: back.closed })
}

/// Frame stamp of the last frame a shape or region was being drawn on the viewer.
fn drawing_id() -> egui::Id {
    egui::Id::new("comp-viewer-drawing")
}

/// Whether a shape or region is being drawn on the viewer right now (shown this frame or the
/// last): the app's Enter / Escape / Backspace shortcuts then leave those keys to the drawing.
/// A drawing left behind when the tools are hidden (another panel, node or clip) doesn't count.
pub(crate) fn drawing(app: &FilmcraftApp, ctx: &egui::Context) -> bool {
    let now = ctx.cumulative_frame_nr();
    app.ui.panels.comp_viewer.draft.is_some() && ctx.data(|d| d.get_temp::<u64>(drawing_id())).is_some_and(|n| n.saturating_add(1) >= now)
}

/// What the pointer did over the picture this frame.
struct Press {
    /// Pressed this frame (and not on the tool bar): where.
    pressed: Option<Pos2>,
    /// The primary button is down; the pointer is here.
    down: Option<Pos2>,
    released: bool,
    hover: Option<Pos2>,
    alt: bool,
    command: bool,
}

fn toolbar_id() -> egui::Id {
    egui::Id::new("comp-viewer-toolbar")
}

fn press(ui: &egui::Ui, resp: &egui::Response) -> Press {
    // the tool bar drawn last frame (a stale one, from a tool no longer shown, doesn't count)
    let now = ui.ctx().cumulative_frame_nr();
    let bar: Option<Rect> = ui.data(|d| d.get_temp::<(Rect, u64)>(toolbar_id())).filter(|(_, n)| n.saturating_add(1) >= now).map(|(r, _)| r);
    let (pressed, down, released, latest, alt, command) = ui.input(|i| {
        let p = &i.pointer;
        (p.primary_pressed().then(|| p.press_origin()).flatten(), p.primary_down(), p.primary_released(), p.latest_pos(), i.modifiers.alt, i.modifiers.command)
    });
    let pressed = pressed.filter(|o| resp.hovered() && !bar.is_some_and(|b| b.contains(*o)));
    Press { pressed, down: if down { latest } else { None }, released, hover: resp.hover_pos(), alt, command }
}

/// A small bar of tool buttons at the top left of the picture (wrapping onto more rows on a
/// narrow monitor, so every control stays on the picture).
fn toolbar(ui: &mut egui::Ui, pic: Rect, add: impl FnOnce(&mut egui::Ui)) {
    let r = Rect::from_min_size(pic.min + vec2(6.0, 6.0), vec2((pic.width() - 24.0).max(60.0), (pic.height() - 12.0).max(28.0)));
    let shown = ui
        .scope_builder(UiBuilder::new().max_rect(r), |ui| {
            egui::Frame::popup(ui.style())
                .show(ui, |ui| {
                    ui.set_max_width(r.width() - 16.0);
                    ui.horizontal_wrapped(add)
                })
                .response
                .rect
        })
        .inner;
    let now = ui.ctx().cumulative_frame_nr();
    ui.data_mut(|d| d.insert_temp(toolbar_id(), (shown, now)));
}

/// The tool's hint at the bottom left of the picture.
fn hint(ui: &egui::Ui, pic: Rect, text: &str) {
    let painter = ui.painter().with_clip_rect(pic);
    let galley = painter.layout(text.to_string(), egui::FontId::proportional(11.0), Color32::WHITE, (pic.width() - 28.0).max(60.0));
    let p = pos2(pic.min.x + 10.0, pic.max.y - galley.size().y - 10.0);
    painter.rect_filled(Rect::from_min_size(p - vec2(4.0, 2.0), galley.size() + vec2(8.0, 4.0)), 3.0, Color32::from_black_alpha(150));
    painter.galley(p, galley, Color32::WHITE);
}

/// Draws and edits the selected Roto / Paint / Tracker / Planar Tracker node on the Program
/// monitor (see the module docs). Id `compViewer.overlay`.
pub fn overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    let Some(v) = view_map(app, pic, frame) else { return };
    let Some(ItemKind::Comp(c)) = app.session.project.item(v.comp).map(|i| &i.kind) else { return };
    let Some(kind) = c.node(v.node).map(|n| n.kind.clone()) else { return };
    let size = (f64::from(c.width.max(1)), f64::from(c.height.max(1)));
    let warps: Vec<Option<TrackWarp>> = match &kind {
        CompKind::Roto { roto, .. } => roto.shapes.iter().map(|sh| shape_warp(c, sh, v.ct)).collect(),
        _ => Vec::new(),
    };
    if !matches!(kind, CompKind::Roto { .. } | CompKind::Paint { .. } | CompKind::Tracker { .. } | CompKind::PlanarTracker { .. }) {
        return;
    }
    let st = &mut app.ui.panels.comp_viewer;
    if st.node != Some(v.node) {
        // another node: its own tools from scratch (the brush is kept)
        *st = CompViewerState { node: Some(v.node), brush: st.brush.clone(), gesture: st.gesture, ..CompViewerState::default() };
    }
    if st.draft.as_ref().is_some_and(|d| d.node != v.node) {
        st.draft = None;
    }
    let resp = ui.interact(pic, egui::Id::new("comp-viewer-overlay"), Sense::click_and_drag());
    app.auto.add("compViewer.overlay", pic, "Comp node viewer tools");
    let input = press(ui, &resp);
    match kind {
        CompKind::Roto { roto, .. } => roto_tool(app, ui, &v, pic, &roto.shapes, &warps, &input),
        CompKind::Paint { .. } => paint_tool(app, ui, &v, pic, &input),
        CompKind::Tracker { points, .. } => tracker_tool(app, ui, &v, pic, &points, &input),
        CompKind::PlanarTracker { region, quad, corners, .. } => planar_tool(app, ui, &v, pic, size, (&region, &quad, &corners), &input),
        _ => {}
    }
}

/// A new gesture number (a press that starts an edit).
fn next_gesture(app: &mut FilmcraftApp) -> u64 {
    let st = &mut app.ui.panels.comp_viewer;
    st.gesture = st.gesture.wrapping_add(1);
    st.gesture
}

/// Whether a plain Enter / Escape / Backspace reaches the tools (no text field has the keyboard);
/// the press is consumed. (With Ctrl / Cmd it is left to the menus.)
fn key(ui: &egui::Ui, k: egui::Key) -> bool {
    !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k))
}

/// A shape's outline (comp pixels): the Bézier path, or the closed uniform cubic B-spline its
/// points control.
fn outline(kind: RotoKind, path: &MaskPath) -> Vec<Vec2> {
    match kind {
        RotoKind::Bezier => path.flatten(0.5),
        RotoKind::BSpline => {
            let n = path.vertices.len();
            if n < 3 {
                return path.vertices.iter().map(|v| v.p).collect();
            }
            let p = |i: usize| path.vertices.get(i % n).map_or(Vec2::ZERO, |v| v.p);
            let mut out = Vec::with_capacity(n.saturating_mul(8));
            for i in 0..n {
                let (a, b, c, d) = (p(i + n - 1), p(i), p(i + 1), p(i + 2));
                for s in 0..8 {
                    let t = f64::from(s) / 8.0;
                    let (t2, t3) = (t * t, t * t * t);
                    let w0 = (1.0 - t).powi(3);
                    let w1 = 3.0 * t3 - 6.0 * t2 + 4.0;
                    let w2 = -3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0;
                    out.push((a * w0 + b * w1 + c * w2 + d * t3) * (1.0 / 6.0));
                }
            }
            out
        }
    }
}

/// The outward unit normal at point `i` of a closed path (comp pixels), or none for a degenerate
/// spot.
fn outward(path: &MaskPath, kind: RotoKind, i: usize) -> Option<Vec2> {
    let n = path.vertices.len();
    let v = path.vertices.get(i)?;
    let prev = path.vertices.get((i + n - 1) % n)?.p;
    let next = path.vertices.get((i + 1) % n)?.p;
    let d = if kind == RotoKind::Bezier && !v.is_corner() { v.t_out - v.t_in } else { next - prev };
    let len = d.length();
    if !len.is_finite() || len < 1e-9 {
        return None;
    }
    // positive shoelace area (y down) = clockwise on screen, whose right-hand normal points out
    let pts = outline(kind, path);
    let m = pts.len();
    let area: f64 = (0..m).filter_map(|j| Some((*pts.get(j)?, *pts.get((j + 1) % m)?))).map(|(a, b)| a.x * b.y - b.x * a.y).sum();
    let s = if area >= 0.0 { 1.0 } else { -1.0 };
    Some(Vec2::new(d.y / len, -d.x / len) * s)
}

/// A screen-space unit direction for the comp-space direction `n` at `p`.
fn screen_dir(v: &ViewMap, p: Vec2, n: Vec2) -> egui::Vec2 {
    let d = v.sp(p + n) - v.sp(p);
    let l = d.length();
    if l > 1e-6 { d / l } else { egui::Vec2::ZERO }
}

/// What a roto press grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RotoHandle {
    Point(usize, usize),
    TanIn(usize, usize),
    TanOut(usize, usize),
    FeatherIn(usize, usize),
    FeatherOut(usize, usize),
    Shape(usize),
}

/// A roto drag in progress: the handle, the shape's path when it started, the press (comp
/// pixels), Alt held, whether it has moved yet and Alt-on-a-point (click: smooth ⇄ corner).
#[derive(Clone)]
struct RotoGrab {
    handle: RotoHandle,
    start: MaskPath,
    origin: Vec2,
    alt: bool,
    moved: bool,
    alt_point: Option<usize>,
    gesture: u64,
}

fn roto_grab_id() -> egui::Id {
    egui::Id::new("comp-viewer-roto-grab")
}

/// Where the feather handles of point `i` of a shape sit on screen (inner, outer, the outward
/// screen direction).
fn feather_handles(v: &ViewMap, path: &MaskPath, kind: RotoKind, i: usize) -> Option<(Pos2, Pos2, egui::Vec2)> {
    let vx = path.vertices.get(i)?;
    let n = outward(path, kind, i)?;
    let dir = screen_dir(v, vx.p, n);
    let p = v.sp(vx.p);
    let s = v.scale as f32;
    Some((p - dir * (vx.feather_in as f32 * s + FEATHER_OFF), p + dir * (vx.feather_out as f32 * s + FEATHER_OFF), dir))
}

/// `warps[k]`: the motion of the tracker shape `k` follows (its outline is drawn and edited where
/// the tracker puts it at the playhead, and stored back in the tracker's reference frame).
fn roto_tool(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &ViewMap, pic: Rect, shapes: &[RotoShape], warps: &[Option<TrackWarp>], input: &Press) {
    let st = &app.ui.panels.comp_viewer;
    let tool = st.roto_tool.unwrap_or(if shapes.is_empty() { RotoTool::Bezier } else { RotoTool::Select });
    let selected = st.selected.filter(|[k, i]| shapes.get(*k).is_some_and(|s| *i < s.path_at(v.ct).vertices.len()));
    let painter = ui.painter().with_clip_rect(pic.expand(16.0));
    let paths: Vec<MaskPath> = shapes.iter().enumerate().map(|(k, s)| shown_path(&s.path_at(v.ct), warps.get(k).copied().flatten())).collect();
    // the shapes: outline, points, the selected shape's Bézier handles and the selected point's
    // feather handles
    for (k, (sh, path)) in shapes.iter().zip(&paths).enumerate() {
        let col = if sh.enabled { COL } else { COL.gamma_multiply(0.5) };
        let pts: Vec<Pos2> = outline(sh.kind, path).into_iter().map(|q| v.sp(q)).collect();
        painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, col)));
        if sh.kind == RotoKind::BSpline {
            let ctl: Vec<Pos2> = path.vertices.iter().map(|q| v.sp(q.p)).collect();
            painter.extend(egui::Shape::dashed_line(
                &[ctl.clone(), ctl.first().copied().into_iter().collect()].concat(),
                Stroke::new(1.0, col.gamma_multiply(0.6)),
                4.0,
                4.0,
            ));
        }
        let shape_selected = selected.is_some_and(|[s, _]| s == k);
        for (i, q) in path.vertices.iter().enumerate() {
            let p = v.sp(q.p);
            if shape_selected && sh.kind == RotoKind::Bezier && !q.is_corner() {
                for (tan, key) in [(q.t_in, "in"), (q.t_out, "out")] {
                    let h = v.sp(q.p + tan);
                    painter.line_segment([p, h], Stroke::new(1.0, col.gamma_multiply(0.8)));
                    painter.circle_filled(h, 3.5, Color32::WHITE);
                    painter.circle_stroke(h, 3.5, Stroke::new(1.0, col));
                    app.auto.add(&format!("compViewer.roto{k}.point{i}.{key}"), Rect::from_center_size(h, vec2(8.0, 8.0)), "Bézier handle");
                }
            }
            let is_sel = selected == Some([k, i]);
            let r = Rect::from_center_size(p, if is_sel { vec2(8.0, 8.0) } else { vec2(6.0, 6.0) });
            painter.rect_filled(r, 0.0, if is_sel { Color32::WHITE } else { col });
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, col), egui::StrokeKind::Middle);
            app.auto.add(&format!("compViewer.roto{k}.point{i}"), Rect::from_center_size(p, vec2(8.0, 8.0)), "Roto point");
        }
    }
    if let Some([k, i]) = selected
        && let (Some(sh), Some(path)) = (shapes.get(k), paths.get(k))
        && let Some((inner, outer, _)) = feather_handles(v, path, sh.kind, i)
        && let Some(q) = path.vertices.get(i)
    {
        let p = v.sp(q.p);
        for (h, id, label) in [(inner, "compViewer.roto.featherIn", "Inner feather"), (outer, "compViewer.roto.featherOut", "Outer feather")] {
            painter.extend(egui::Shape::dashed_line(&[p, h], Stroke::new(1.0, FEATHER_COL), 3.0, 3.0));
            let tri = vec![h + vec2(0.0, -5.0), h + vec2(5.0, 0.0), h + vec2(0.0, 5.0), h + vec2(-5.0, 0.0)];
            painter.add(egui::Shape::convex_polygon(tri, FEATHER_COL, Stroke::new(1.0, Color32::BLACK)));
            app.auto.add(id, Rect::from_center_size(h, vec2(10.0, 10.0)), label);
        }
    }
    // the tool bar and the tool's hint
    let mut pick: Option<RotoTool> = None;
    toolbar(ui, pic, |ui| {
        for (t, id, label) in [
            (RotoTool::Select, "compViewer.roto.select", tl!("Select")),
            (RotoTool::Bezier, "compViewer.roto.bezier", tl!("Bézier")),
            (RotoTool::BSpline, "compViewer.roto.bSpline", tl!("B-Spline")),
        ] {
            let r = ui.selectable_label(tool == t, label);
            // (a tool button never keeps the keyboard: Enter closes the drawing, not the button)
            r.surrender_focus();
            app.auto.add_response(id, &r, label);
            if r.clicked() {
                pick = Some(t);
            }
        }
    });
    if let Some(t) = pick {
        let st = &mut app.ui.panels.comp_viewer;
        st.roto_tool = Some(t);
        st.draft = None;
        return;
    }
    match tool {
        RotoTool::Select => {
            hint(ui, pic, tl!("Drag points, handles or a shape. Alt-drag a point moves its shape; Alt-click makes it smooth or sharp."));
            roto_select(app, ui, v, shapes, (&paths, warps), selected, input);
        }
        RotoTool::Bezier | RotoTool::BSpline => {
            let kind = if tool == RotoTool::Bezier { RotoKind::Bezier } else { RotoKind::BSpline };
            hint(
                ui,
                pic,
                if kind == RotoKind::Bezier {
                    tl!("Click to add points, drag to curve them. Click the first point (or press Enter) to close.")
                } else {
                    tl!("Click to add control points. Click the first point (or press Enter) to close.")
                },
            );
            if let Some(points) = draw_points(app, ui, v, pic, input, kind == RotoKind::Bezier) {
                let path = if kind == RotoKind::Bezier {
                    crate::panels::masks::draft_path(&points)
                } else {
                    MaskPath::polygon(&points.iter().map(|q| Vec2::new(q[0], q[1])).collect::<Vec<_>>())
                };
                let kind_key = if kind == RotoKind::Bezier { "bezier" } else { "bSpline" };
                let p = json!({"comp": v.comp.0, "node": v.node, "kind": kind_key, "path": path_json(&path), "time": v.ct.0});
                if let Some(r) = run(app, "comp.roto.addShape", p) {
                    let st = &mut app.ui.panels.comp_viewer;
                    st.selected = r["shape"].as_u64().and_then(|s| usize::try_from(s).ok()).map(|s| [s, 0]);
                    // back to automatic: Select now that there is a shape
                    st.roto_tool = None;
                }
            }
        }
    }
}

/// Click-to-add drawing of a shape or region on the picture (Bézier tangents by dragging when
/// `tangents`). Returns the points when the drawing is closed (clicking the first point or
/// Enter, with at least 3 points); Escape cancels, Backspace removes the last point.
fn draw_points(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &ViewMap, pic: Rect, input: &Press, tangents: bool) -> Option<Vec<[f64; 4]>> {
    let node = v.node;
    let now = ui.ctx().cumulative_frame_nr();
    ui.data_mut(|m| m.insert_temp(drawing_id(), now));
    let mut d = app.ui.panels.comp_viewer.draft.clone().filter(|d| d.node == node).unwrap_or(Draft { node, points: Vec::new() });
    let placing_id = egui::Id::new("comp-viewer-placing");
    let mut placing: bool = ui.data(|m| m.get_temp(placing_id)).unwrap_or(false);
    let mut close = false;
    if input.hover.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if let Some(o) = input.pressed {
        let near_first = d.points.first().is_some_and(|f| v.sp(Vec2::new(f[0], f[1])).distance(o) < PICK);
        if d.points.len() >= 3 && near_first {
            close = true;
        } else if d.points.len() < filmcraft_project::roto::MAX_POINTS {
            let c = v.cp(o);
            d.points.push([c.x, c.y, 0.0, 0.0]);
            placing = true;
        }
    }
    if tangents
        && placing
        && let (Some(p), Some(last)) = (input.down, d.points.last_mut())
    {
        let c = v.cp(p);
        let tan = Vec2::new(c.x - last[0], c.y - last[1]);
        if tan.length() * v.scale > 3.0 {
            last[2] = tan.x;
            last[3] = tan.y;
        }
    }
    if input.released || input.down.is_none() {
        placing = false;
    }
    ui.data_mut(|m| m.insert_temp(placing_id, placing));
    if key(ui, egui::Key::Enter) && d.points.len() >= 3 {
        close = true;
    }
    if key(ui, egui::Key::Escape) {
        app.ui.panels.comp_viewer.draft = None;
        return None;
    }
    if key(ui, egui::Key::Backspace) {
        d.points.pop();
    }
    // the drawing so far
    let painter = ui.painter().with_clip_rect(pic);
    let open = MaskPath { closed: false, ..crate::panels::masks::draft_path(&d.points) };
    let mut line: Vec<Pos2> = open.vertices.first().map(|q| v.sp(q.p)).into_iter().collect();
    for [p0, c1, c2, p3] in open.segments() {
        line.extend((1..=16).map(|k| v.sp(filmcraft_project::mask::cubic(p0, c1, c2, p3, f64::from(k) / 16.0))));
    }
    if line.len() >= 2 {
        painter.add(egui::Shape::line(line, Stroke::new(1.5, COL)));
    }
    if let (Some(h), Some(l)) = (input.hover, d.points.last()) {
        painter.line_segment([v.sp(Vec2::new(l[0], l[1])), h], Stroke::new(1.0, COL.gamma_multiply(0.5)));
    }
    for (i, q) in d.points.iter().enumerate() {
        let p = v.sp(Vec2::new(q[0], q[1]));
        let r = Rect::from_center_size(p, vec2(7.0, 7.0));
        painter.rect_filled(r, 0.0, if i == 0 { COL } else { Color32::WHITE });
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, COL), egui::StrokeKind::Middle);
        if q[2] != 0.0 || q[3] != 0.0 {
            for s in [1.0, -1.0] {
                let h = v.sp(Vec2::new(q[0] + q[2] * s, q[1] + q[3] * s));
                painter.line_segment([p, h], Stroke::new(1.0, COL));
                painter.circle_filled(h, 3.0, Color32::WHITE);
            }
        }
        if i == 0 {
            app.auto.add("compViewer.draft.first", Rect::from_center_size(p, vec2(8.0, 8.0)), "First point");
        }
    }
    if close {
        app.ui.panels.comp_viewer.draft = None;
        ui.data_mut(|m| m.insert_temp(placing_id, false));
        return Some(d.points);
    }
    app.ui.panels.comp_viewer.draft = (!d.points.is_empty()).then_some(d);
    None
}

fn roto_select(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    v: &ViewMap,
    shapes: &[RotoShape],
    (paths, warps): (&[MaskPath], &[Option<TrackWarp>]),
    selected: Option<[usize; 2]>,
    input: &Press,
) {
    let gid = roto_grab_id();
    if let Some(o) = input.pressed {
        // feather handles of the selected point, then the selected shape's Bézier handles, then
        // any point, then inside a shape
        let mut near: Vec<(RotoHandle, f32)> = Vec::new();
        if let Some([k, i]) = selected
            && let (Some(sh), Some(path)) = (shapes.get(k), paths.get(k))
        {
            if let Some((inner, outer, _)) = feather_handles(v, path, sh.kind, i) {
                near.push((RotoHandle::FeatherIn(k, i), inner.distance(o)));
                near.push((RotoHandle::FeatherOut(k, i), outer.distance(o)));
            }
            if sh.kind == RotoKind::Bezier {
                for (j, q) in path.vertices.iter().enumerate().filter(|(_, q)| !q.is_corner()) {
                    near.push((RotoHandle::TanIn(k, j), v.sp(q.p + q.t_in).distance(o)));
                    near.push((RotoHandle::TanOut(k, j), v.sp(q.p + q.t_out).distance(o)));
                }
            }
        }
        for (k, path) in paths.iter().enumerate() {
            for (i, q) in path.vertices.iter().enumerate() {
                // a point wins a tie with its own handle
                near.push((RotoHandle::Point(k, i), v.sp(q.p).distance(o) - 0.01));
            }
        }
        let hit = near.into_iter().filter(|(_, d)| *d < PICK).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(h, _)| h);
        let oc = v.cp(o);
        let hit = hit.or_else(|| {
            shapes
                .iter()
                .zip(paths)
                .enumerate()
                .rev()
                .find(|(_, (sh, p))| crate::panels::comp::contains(&outline(sh.kind, p), oc))
                .map(|(k, _)| RotoHandle::Shape(k))
        });
        let st = &mut app.ui.panels.comp_viewer;
        match hit {
            Some(h) => {
                let (k, alt_point) = match h {
                    RotoHandle::Point(k, i) => {
                        st.selected = Some([k, i]);
                        (k, input.alt.then_some(i))
                    }
                    RotoHandle::Shape(k) => {
                        if st.selected.is_none_or(|[s, _]| s != k) {
                            st.selected = Some([k, 0]);
                        }
                        (k, None)
                    }
                    RotoHandle::TanIn(k, _) | RotoHandle::TanOut(k, _) | RotoHandle::FeatherIn(k, _) | RotoHandle::FeatherOut(k, _) => (k, None),
                };
                let handle = if alt_point.is_some() { RotoHandle::Shape(k) } else { h };
                let gesture = next_gesture(app);
                let start = paths.get(k).cloned().unwrap_or_default();
                ui.data_mut(|d| d.insert_temp(gid, RotoGrab { handle, start, origin: oc, alt: input.alt, moved: false, alt_point, gesture }));
            }
            None => st.selected = None,
        }
    }
    let Some(mut g) = ui.data(|d| d.get_temp::<RotoGrab>(gid)) else { return };
    let k = match g.handle {
        RotoHandle::Point(k, _)
        | RotoHandle::TanIn(k, _)
        | RotoHandle::TanOut(k, _)
        | RotoHandle::FeatherIn(k, _)
        | RotoHandle::FeatherOut(k, _)
        | RotoHandle::Shape(k) => k,
    };
    let warp = warps.get(k).copied().flatten();
    // `path` is where the outline is drawn; a linked shape keys it in its tracker's reference frame
    let send = |app: &mut FilmcraftApp, path: &MaskPath, gesture: u64| {
        let Some(stored) = stored_path(path, warp) else {
            app.session.toast(tl!("The tracker this shape follows has no usable motion at this frame, so the shape can't be edited here."));
            return;
        };
        let p = json!({"comp": v.comp.0, "node": v.node, "shape": k, "time": v.ct.0, "path": path_json(&stored), "gesture": gesture});
        run(app, "comp.roto.setShapeKeyframe", p);
    };
    let Some(pos) = input.down else {
        // released: an Alt-click on a point (no drag) switches it between smooth and corner
        ui.data_mut(|d| d.remove::<RotoGrab>(gid));
        if let (false, Some(i)) = (g.moved, g.alt_point)
            && shapes.get(k).is_some_and(|s| s.kind == RotoKind::Bezier)
        {
            let mut path = g.start.clone();
            if path.toggle_smooth(i) {
                send(app, &path, g.gesture);
            }
        }
        return;
    };
    let delta = v.cp(pos) - g.origin;
    if !g.moved && delta.length() * v.scale < 1.0 {
        return;
    }
    g.moved = true;
    let mut path = g.start.clone();
    let kind = shapes.get(k).map_or(RotoKind::Bezier, |s| s.kind);
    match g.handle {
        RotoHandle::Point(_, i) => {
            if let Some(q) = path.vertices.get_mut(i) {
                q.p = q.p + delta;
            }
        }
        RotoHandle::TanIn(_, i) | RotoHandle::TanOut(_, i) => {
            let out = matches!(g.handle, RotoHandle::TanOut(..));
            if let Some(q) = path.vertices.get_mut(i) {
                let tan = if out { q.t_out } else { q.t_in };
                q.set_tangent(out, tan + delta, g.alt);
            }
        }
        RotoHandle::FeatherIn(_, i) | RotoHandle::FeatherOut(_, i) => {
            let outer = matches!(g.handle, RotoHandle::FeatherOut(..));
            if let (Some(n), Some(q)) = (outward(&g.start, kind, i), g.start.vertices.get(i)) {
                let dir = screen_dir(v, q.p, n);
                let along = (pos - v.sp(q.p)).dot(dir) * if outer { 1.0 } else { -1.0 };
                let f = (f64::from(along - FEATHER_OFF) / v.scale).clamp(0.0, MAX_FEATHER);
                if let Some(q) = path.vertices.get_mut(i) {
                    *q = if outer { q.with_feather(q.feather_in, f) } else { q.with_feather(f, q.feather_out) };
                }
            }
        }
        RotoHandle::Shape(_) => path.vertices.iter_mut().for_each(|q| q.p = q.p + delta),
    }
    ui.data_mut(|d| d.insert_temp(gid, g.clone()));
    if paths.get(k) != Some(&path) {
        send(app, &path, g.gesture);
    }
}

fn paint_tool(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &ViewMap, pic: Rect, input: &Press) {
    let b = app.ui.panels.comp_viewer.brush.clone();
    let painter = ui.painter().with_clip_rect(pic);
    // the stroke so far, the brush cursor and (Clone) where it copies from
    let stroke = app.ui.panels.comp_viewer.stroke.clone();
    let width = (b.size * v.scale) as f32;
    let shown = Color32::from(egui::Rgba::from_rgba_unmultiplied(b.color[0], b.color[1], b.color[2], 1.0)).gamma_multiply((b.opacity as f32).clamp(0.2, 1.0));
    if stroke.len() >= 2 {
        let line: Vec<Pos2> = stroke.iter().map(|q| v.sp(Vec2::new(q[0], q[1]))).collect();
        painter.add(egui::Shape::line(line, Stroke::new(width.max(1.0), shown.gamma_multiply(0.6))));
    }
    if let Some(h) = input.hover {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        painter.circle_stroke(h, (width / 2.0).max(1.0), Stroke::new(1.0, Color32::WHITE));
        painter.circle_stroke(h, (width / 2.0 + 1.0).max(2.0), Stroke::new(1.0, Color32::BLACK));
        if b.tool == PaintTool::Clone {
            let src = match b.clone_source {
                Some(s) => v.sp(Vec2::new(s[0], s[1])),
                None => v.sp(v.cp(h) + Vec2::new(b.clone_offset[0], b.clone_offset[1])),
            };
            painter.line_segment([src - vec2(6.0, 0.0), src + vec2(6.0, 0.0)], Stroke::new(1.0, Color32::WHITE));
            painter.line_segment([src - vec2(0.0, 6.0), src + vec2(0.0, 6.0)], Stroke::new(1.0, Color32::WHITE));
        }
    }
    // the tool bar: tool, size, hardness, opacity, colour, duration
    let mut nb = b.clone();
    toolbar(ui, pic, |ui| {
        for (t, id, label) in [
            (PaintTool::Color, "compViewer.paint.color", tl!("Color")),
            (PaintTool::Clone, "compViewer.paint.clone", tl!("Clone")),
            (PaintTool::Reveal, "compViewer.paint.reveal", tl!("Reveal")),
            (PaintTool::Erase, "compViewer.paint.erase", tl!("Erase")),
        ] {
            let r = ui.selectable_label(nb.tool == t, label);
            r.surrender_focus();
            app.auto.add_response(id, &r, label);
            if r.clicked() {
                nb.tool = t;
            }
        }
        ui.separator();
        ui.label(tl!("Size"));
        let r = ui.add(egui::DragValue::new(&mut nb.size).range(1.0..=MAX_BRUSH_SIZE).speed(0.5).suffix(" px"));
        app.auto.add_response("compViewer.paint.size", &r, "Size");
        ui.label(tl!("Hardness"));
        let mut h = nb.hardness * 100.0;
        let r = ui.add(egui::DragValue::new(&mut h).range(0.0..=100.0).speed(0.5).suffix(" %"));
        app.auto.add_response("compViewer.paint.hardness", &r, "Hardness");
        nb.hardness = (h / 100.0).clamp(0.0, 1.0);
        ui.label(tl!("Opacity"));
        let mut o = nb.opacity * 100.0;
        let r = ui.add(egui::DragValue::new(&mut o).range(0.0..=100.0).speed(0.5).suffix(" %"));
        app.auto.add_response("compViewer.paint.opacity", &r, "Opacity");
        nb.opacity = (o / 100.0).clamp(0.0, 1.0);
        if nb.tool == PaintTool::Color {
            let r = ui.color_edit_button_rgba_unmultiplied(&mut nb.color);
            app.auto.add_response("compViewer.paint.swatch", &r, "Paint colour");
        }
        let r = ui.checkbox(&mut nb.single_frame, tl!("This frame only"));
        app.auto.add_response("compViewer.paint.singleFrame", &r, "This frame only");
    });
    if !nb.size.is_finite() {
        nb.size = b.size;
    }
    hint(
        ui,
        pic,
        if nb.tool == PaintTool::Clone {
            tl!("Drag to paint; each drag is one stroke. Alt-click picks where Clone copies from.")
        } else {
            tl!("Drag to paint; each drag is one stroke.")
        },
    );
    app.ui.panels.comp_viewer.brush = nb.clone();
    // Alt-click with Clone: the clone source
    if let Some(o) = input.pressed
        && input.alt
        && nb.tool == PaintTool::Clone
    {
        let c = v.cp(o);
        app.ui.panels.comp_viewer.brush.clone_source = Some([c.x, c.y]);
        return;
    }
    let st = &mut app.ui.panels.comp_viewer;
    if let Some(o) = input.pressed {
        let c = v.cp(o);
        st.stroke = vec![[c.x, c.y]];
        if let Some(s) = st.brush.clone_source.take() {
            st.brush.clone_offset = [s[0] - c.x, s[1] - c.y];
        }
    }
    if st.stroke.is_empty() {
        return;
    }
    if let Some(p) = input.down {
        let c = v.cp(p);
        let far = st.stroke.last().is_none_or(|l| (Vec2::new(l[0], l[1]) - c).length() * v.scale >= 1.0);
        if far && st.stroke.len() < MAX_STROKE_POINTS {
            st.stroke.push([c.x, c.y]);
        }
        return;
    }
    // released: the stroke is one `comp.paint.addStroke`
    let points: Vec<PaintPoint> = std::mem::take(&mut st.stroke).into_iter().map(|q| PaintPoint::new(q[0], q[1])).collect();
    let b = &st.brush;
    let mut s = PaintStroke::new(b.tool, points, Brush { size: b.size, hardness: b.hardness, opacity: b.opacity, ..Brush::default() });
    s.color = b.color;
    s.clone_offset = Vec2::new(b.clone_offset[0], b.clone_offset[1]);
    s.duration = if b.single_frame { StrokeDuration::SingleFrame { at: v.ct } } else { StrokeDuration::default() };
    match serde_json::to_value(&s) {
        Ok(stroke) => {
            run(app, "comp.paint.addStroke", json!({"comp": v.comp.0, "node": v.node, "stroke": stroke}));
        }
        Err(e) => app.session.toast(e.to_string()),
    }
}

/// What a tracker press grabbed: a point's centre, its pattern box corner or its search box
/// corner.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TrackHandle {
    Centre(usize),
    Pattern(usize),
    Search(usize),
}

#[derive(Clone)]
struct TrackGrab {
    handle: TrackHandle,
    start: Vec<TrackerPoint>,
    origin: Vec2,
    gesture: u64,
}

fn tracker_tool(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &ViewMap, pic: Rect, points: &[TrackerPoint], input: &Press) {
    let painter = ui.painter().with_clip_rect(pic.expand(16.0));
    let at_of = |p: &TrackerPoint| p.track.value_at(v.ct).as_vec2().unwrap_or(Vec2::ZERO);
    let mut handles: Vec<(TrackHandle, Pos2)> = Vec::new();
    for (k, pt) in points.iter().enumerate() {
        // the analysed path over the comp clip's frames
        let path: Vec<Pos2> = pt
            .track
            .keyframes
            .iter()
            .filter(|kf| kf.time >= v.span.0 && kf.time <= v.span.1)
            .take(20_000)
            .filter_map(|kf| kf.value.as_vec2())
            .map(|q| v.sp(q))
            .collect();
        if path.len() >= 2 {
            painter.add(egui::Shape::line(path.clone(), Stroke::new(1.0, TRACK_COL.gamma_multiply(0.7))));
        }
        for q in path.iter().step_by((path.len() / 400).max(1)) {
            painter.circle_filled(*q, 1.5, TRACK_COL);
        }
        let c = at_of(pt);
        let pat = Rect::from_two_pos(v.sp(c - pt.pattern), v.sp(c + pt.pattern));
        let reach = pt.pattern + pt.search;
        let search = Rect::from_two_pos(v.sp(c - reach), v.sp(c + reach));
        painter.rect_stroke(pat, 0.0, Stroke::new(1.5, TRACK_COL), egui::StrokeKind::Middle);
        let corners = [search.left_top(), search.right_top(), search.right_bottom(), search.left_bottom(), search.left_top()];
        painter.extend(egui::Shape::dashed_line(&corners, Stroke::new(1.0, TRACK_COL.gamma_multiply(0.7)), 4.0, 3.0));
        let p = v.sp(c);
        let (ph, sh) = track_handles(p, pat.right_bottom(), search.right_bottom());
        painter.line_segment([p - vec2(5.0, 0.0), p + vec2(5.0, 0.0)], Stroke::new(1.5, Color32::WHITE));
        painter.line_segment([p - vec2(0.0, 5.0), p + vec2(0.0, 5.0)], Stroke::new(1.5, Color32::WHITE));
        let name = if pt.name.is_empty() { format!("P{}", k.saturating_add(1)) } else { pt.name.clone() };
        painter.text(pat.left_top() - vec2(0.0, 2.0), egui::Align2::LEFT_BOTTOM, name, egui::FontId::proportional(11.0), TRACK_COL);
        for (h, q, id) in [
            (TrackHandle::Centre(k), p, format!("compViewer.track.point{k}")),
            (TrackHandle::Pattern(k), ph, format!("compViewer.track.pattern{k}")),
            (TrackHandle::Search(k), sh, format!("compViewer.track.search{k}")),
        ] {
            if !matches!(h, TrackHandle::Centre(_)) {
                // (a handle pushed out from a box too small on screen keeps a thin lead to it)
                let corner = if matches!(h, TrackHandle::Pattern(_)) { pat.right_bottom() } else { search.right_bottom() };
                if corner.distance(q) > 0.5 {
                    painter.line_segment([corner, q], Stroke::new(1.0, TRACK_COL.gamma_multiply(0.7)));
                }
                painter.rect_filled(Rect::from_center_size(q, vec2(6.0, 6.0)), 0.0, TRACK_COL);
            }
            app.auto.add(&id, Rect::from_center_size(q, vec2(8.0, 8.0)), "Tracker handle");
            handles.push((h, q));
        }
    }
    hint(ui, pic, tl!("Drag a point, its pattern box corner or its search box corner. Ctrl-click (Cmd-click) adds a point."));
    let gid = egui::Id::new("comp-viewer-track-grab");
    let set = |app: &mut FilmcraftApp, list: &[TrackerPoint], gesture: Option<u64>| match serde_json::to_value(list) {
        Ok(points) => {
            let mut p = json!({"comp": v.comp.0, "node": v.node, "points": points});
            if let Some(g) = gesture {
                p["gesture"] = json!(g);
            }
            run(app, "comp.track.setPoints", p);
        }
        Err(e) => app.session.toast(e.to_string()),
    };
    if let Some(o) = input.pressed {
        let hit = handles.iter().map(|(h, q)| (*h, q.distance(o))).filter(|(_, d)| *d < PICK).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(h, _)| h);
        match hit {
            Some(handle) => {
                let gesture = next_gesture(app);
                ui.data_mut(|d| d.insert_temp(gid, TrackGrab { handle, start: points.to_vec(), origin: v.cp(o), gesture }));
            }
            None if input.command && points.len() < filmcraft_project::comp::MAX_TRACK_POINTS => {
                // Ctrl-click: a new point there (one undo step)
                let mut list = points.to_vec();
                list.push(TrackerPoint::at(format!("P{}", list.len().saturating_add(1)), v.cp(o)));
                set(app, &list, None);
            }
            None => {}
        }
    }
    let Some(g) = ui.data(|d| d.get_temp::<TrackGrab>(gid)) else { return };
    let Some(pos) = input.down else {
        ui.data_mut(|d| d.remove::<TrackGrab>(gid));
        return;
    };
    let q = v.cp(pos);
    let delta = q - g.origin;
    if delta.length() * v.scale < 1.0 {
        return;
    }
    let mut list = g.start.clone();
    match g.handle {
        TrackHandle::Centre(k) => {
            if let Some(p) = list.get_mut(k) {
                let c = p.track.value_at(v.ct).as_vec2().unwrap_or(Vec2::ZERO);
                p.track.set_at(v.ct, ParamValue::Vec2(c + delta));
            }
        }
        TrackHandle::Pattern(k) | TrackHandle::Search(k) => {
            // the box grows by how much farther from the centre the pointer is than at the press
            // (the handle may sit off the corner, see `track_handles`; any Motion flip or turn)
            if let Some(p) = list.get_mut(k) {
                let c = p.track.value_at(v.ct).as_vec2().unwrap_or(Vec2::ZERO);
                let abs = |d: Vec2| Vec2::new(d.x.abs(), d.y.abs());
                let delta = abs(q - c) - abs(g.origin - c);
                if matches!(g.handle, TrackHandle::Pattern(_)) {
                    let s = p.pattern + delta;
                    p.pattern = Vec2::new(s.x.clamp(PATTERN_HALF.0, PATTERN_HALF.1), s.y.clamp(PATTERN_HALF.0, PATTERN_HALF.1));
                } else {
                    let s = p.search + delta;
                    p.search = Vec2::new(s.x.clamp(SEARCH_HALF.0, SEARCH_HALF.1), s.y.clamp(SEARCH_HALF.0, SEARCH_HALF.1));
                }
            }
        }
    }
    if list.as_slice() != points {
        set(app, &list, Some(g.gesture));
    }
}

/// Where a tracker point's pattern- and search-box handles sit on screen: at the boxes'
/// bottom-right corners, pushed out (down and right) to at least [`TRACK_HANDLE_SEP`] from the
/// centre and from each other when the boxes are small on screen, so each stays grabbable.
fn track_handles(centre: Pos2, pattern: Pos2, search: Pos2) -> (Pos2, Pos2) {
    let out = |from: Pos2, to: Pos2| pos2(to.x.max(from.x + TRACK_HANDLE_SEP), to.y.max(from.y + TRACK_HANDLE_SEP));
    let ph = out(centre, pattern);
    (ph, out(ph, search))
}

/// What a planar press grabbed: a region point or a plane corner.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PlanarHandle {
    Region(usize),
    Corner(usize),
}

#[derive(Clone)]
struct PlanarGrab {
    handle: PlanarHandle,
    region: Vec<Vec2>,
    quad: [Vec2; 4],
    origin: Vec2,
    gesture: u64,
}

fn planar_tool(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    v: &ViewMap,
    pic: Rect,
    size: (f64, f64),
    (region, quad, corners): (&MaskPath, &[Vec2; 4], &[filmcraft_project::Param]),
    input: &Press,
) {
    let poly: Vec<Vec2> = region.vertices.iter().map(|q| q.p).collect();
    let q = filmcraft_render::comp::track::reference_quad(quad, &poly, size);
    let drawing = app.ui.panels.comp_viewer.draw_region.unwrap_or(poly.len() < 3);
    let painter = ui.painter().with_clip_rect(pic.expand(16.0));
    // the region, the plane at the reference frame and (once analysed) where it is now
    if poly.len() >= 3 {
        painter.add(egui::Shape::closed_line(poly.iter().map(|p| v.sp(*p)).collect(), Stroke::new(1.5, TRACK_COL)));
    }
    let qs: Vec<Pos2> = q.iter().map(|p| v.sp(*p)).collect();
    painter.add(egui::Shape::closed_line(qs.clone(), Stroke::new(1.5, FEATHER_COL)));
    if corners.len() == 4 && corners.iter().any(|c| !c.keyframes.is_empty()) {
        let now: Vec<Pos2> = corners.iter().filter_map(|c| c.value_at(v.ct).as_vec2()).map(|p| v.sp(p)).collect();
        if now.len() == 4 {
            let mut ring = now.clone();
            ring.extend(now.first().copied());
            painter.extend(egui::Shape::dashed_line(&ring, Stroke::new(1.0, Color32::WHITE), 5.0, 3.0));
        }
    }
    let mut handles: Vec<(PlanarHandle, Pos2)> = Vec::new();
    if !drawing {
        // (a corner wins a tie with a region point under it: listed first)
        for (k, s) in qs.iter().enumerate() {
            painter.circle_filled(*s, 4.5, FEATHER_COL);
            painter.circle_stroke(*s, 4.5, Stroke::new(1.0, Color32::BLACK));
            app.auto.add(&format!("compViewer.planar.corner{k}"), Rect::from_center_size(*s, vec2(10.0, 10.0)), "Plane corner");
            handles.push((PlanarHandle::Corner(k), *s));
        }
        for (i, p) in poly.iter().enumerate() {
            let s = v.sp(*p);
            painter.rect_filled(Rect::from_center_size(s, vec2(6.0, 6.0)), 0.0, TRACK_COL);
            app.auto.add(&format!("compViewer.planar.region{i}"), Rect::from_center_size(s, vec2(8.0, 8.0)), "Region point");
            handles.push((PlanarHandle::Region(i), s));
        }
    }
    let mut pick: Option<bool> = None;
    toolbar(ui, pic, |ui| {
        for (d, id, label) in [(false, "compViewer.planar.edit", tl!("Edit")), (true, "compViewer.planar.draw", tl!("Draw region"))] {
            let r = ui.selectable_label(drawing == d, label);
            r.surrender_focus();
            app.auto.add_response(id, &r, label);
            if r.clicked() {
                pick = Some(d);
            }
        }
    });
    if let Some(d) = pick {
        let st = &mut app.ui.panels.comp_viewer;
        st.draw_region = Some(d);
        st.draft = None;
        return;
    }
    let pts = |v: &[Vec2]| v.iter().map(|p| json!([p.x, p.y])).collect::<Vec<_>>();
    if drawing {
        hint(ui, pic, tl!("Click to add region points around the flat area. Click the first point (or press Enter) to close."));
        if let Some(points) = draw_points(app, ui, v, pic, input, false) {
            let region: Vec<Vec2> = points.iter().map(|p| Vec2::new(p[0], p[1])).collect();
            if run(app, "comp.planar.setRegion", json!({"comp": v.comp.0, "node": v.node, "region": pts(&region)})).is_some() {
                app.ui.panels.comp_viewer.draw_region = None;
            }
        }
        return;
    }
    hint(ui, pic, tl!("Drag the region's points, or the plane's four corners at the reference frame."));
    let gid = egui::Id::new("comp-viewer-planar-grab");
    if let Some(o) = input.pressed
        && let Some(handle) = handles.iter().map(|(h, s)| (*h, s.distance(o))).filter(|(_, d)| *d < PICK).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(h, _)| h)
    {
        let gesture = next_gesture(app);
        ui.data_mut(|d| d.insert_temp(gid, PlanarGrab { handle, region: poly.clone(), quad: q, origin: v.cp(o), gesture }));
    }
    let Some(g) = ui.data(|d| d.get_temp::<PlanarGrab>(gid)) else { return };
    let Some(pos) = input.down else {
        ui.data_mut(|d| d.remove::<PlanarGrab>(gid));
        return;
    };
    let delta = v.cp(pos) - g.origin;
    if delta.length() * v.scale < 1.0 {
        return;
    }
    let (mut nr, mut nq) = (g.region.clone(), g.quad);
    let p = match g.handle {
        PlanarHandle::Region(i) => {
            if let Some(p) = nr.get_mut(i) {
                *p = *p + delta;
            }
            json!({"comp": v.comp.0, "node": v.node, "region": pts(&nr), "gesture": g.gesture})
        }
        PlanarHandle::Corner(k) => {
            if let Some(c) = nq.get_mut(k) {
                *c = *c + delta;
            }
            // a region that is the quad (or none yet) moves with it, as in the inspector
            let follows = nr.len() < 3 || (nr.len() == 4 && nr.iter().zip(g.quad.iter()).all(|(a, b)| (*a - *b).length() < 1e-6));
            if follows {
                nr = nq.to_vec();
            }
            json!({"comp": v.comp.0, "node": v.node, "region": pts(&nr), "quad": pts(&nq), "gesture": g.gesture})
        }
    };
    if nr != poly || nq != q {
        run(app, "comp.planar.setRegion", p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> MaskPath {
        MaskPath::polygon(&[Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)])
    }

    #[test]
    fn comp_viewer_outward_normals_point_out_either_winding() {
        let p = square();
        let n = outward(&p, RotoKind::Bezier, 1).unwrap();
        assert!(n.x > 0.5 && n.y < -0.5, "{n:?}");
        let mut r = p.clone();
        r.vertices.reverse();
        // vertex 2 of the reversed square is (10, 0) again
        let n = outward(&r, RotoKind::Bezier, 2).unwrap();
        assert!(n.x > 0.5 && n.y < -0.5, "{n:?}");
        // degenerate paths give none, never a panic
        assert!(outward(&MaskPath::default(), RotoKind::Bezier, 0).is_none());
        let dot = MaskPath::polygon(&[Vec2::ZERO, Vec2::ZERO, Vec2::ZERO]);
        assert!(outward(&dot, RotoKind::BSpline, 1).is_none());
    }

    #[test]
    fn comp_viewer_bspline_outline_stays_in_its_hull() {
        let o = outline(RotoKind::BSpline, &square());
        assert_eq!(o.len(), 32);
        assert!(o.iter().all(|q| (0.0..=10.0).contains(&q.x) && (0.0..=10.0).contains(&q.y)), "{o:?}");
        assert!(outline(RotoKind::BSpline, &MaskPath::default()).is_empty());
    }

    #[test]
    fn comp_viewer_state_round_trips_through_serde() {
        let s = CompViewerState { roto_tool: Some(RotoTool::BSpline), selected: Some([1, 2]), ..CompViewerState::default() };
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["rotoTool"], json!("bSpline"));
        assert_eq!(j["brush"]["tool"], json!("color"));
        let back: CompViewerState = serde_json::from_value(j).unwrap();
        assert_eq!(back, s);
        // garbage fields fall back to defaults
        let d: CompViewerState = serde_json::from_value(json!({"brush": {"size": 5.0}})).unwrap();
        assert_eq!(d.brush.size, 5.0);
        assert_eq!(d.brush.opacity, 1.0);
    }
}
