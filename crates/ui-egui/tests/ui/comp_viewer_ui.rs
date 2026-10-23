//! Fork C3.9: the Program monitor's tools for the Compositor's Roto, Paint, Tracker and Planar
//! Tracker nodes, driven by simulated pointer events. Each test checks the project the commands
//! left and that a drag is one undo step.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    comp: u64,
    /// The picture rect (screen points) and the comp size (pixels).
    pic: [f64; 4],
    size: (f64, f64),
}

impl Driver {
    /// The demo project, the Compositor open on a new comp clip, a `kind` node selected.
    fn with_node(workflow: Workflow, kind: &str) -> (Self, u64) {
        let mut session = Session::default();
        session.prefs.workflow = workflow;
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, comp: 0, pic: [0.0; 4], size: (1.0, 1.0) };
        d.frames(4);
        d.ok("ui.menu.invoke", json!({"id": "window.panel.Compositor"}));
        d.frames(3);
        d.ok("ui.click", json!({"id": "comp.newClip"}));
        d.frames(3);
        d.comp = d.ok("ui.inspect", json!({}))["ui"]["panels"]["comp"]["comp"].as_u64().expect("the panel shows the new comp");
        let node = d.exec("comp.addNode", json!({"comp": d.comp, "kind": kind, "pos": [-200, 0]}))["node"].as_u64().unwrap();
        d.ok("ui.set", json!({"panels": {"comp": {"canvas": {"selection": [node]}}}}));
        d.frames(4);
        let ov = d.ok("ui.elements", json!({"prefix": "compViewer.overlay"}));
        let r = &ov[0]["rect"];
        assert!(r.is_array(), "{workflow:?}: the viewer tools are on the Program monitor: {ov}");
        d.pic = [0, 1, 2, 3].map(|i| r[i].as_f64().unwrap());
        let g = d.exec("comp.get", json!({"comp": d.comp}));
        d.size = (g["width"].as_f64().unwrap(), g["height"].as_f64().unwrap());
        (d, node)
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    /// The centre of element `id` (screen points).
    fn at(&mut self, id: &str) -> (f64, f64) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).cloned().unwrap_or_else(|| panic!("no element {id}: {v}"));
        let r = &e["rect"];
        let f = |i: usize| r[i].as_f64().unwrap();
        (f(0) + f(2) / 2.0, f(1) + f(3) / 2.0)
    }

    /// Comp pixels (as fractions of the comp) → screen points.
    fn scr(&self, fx: f64, fy: f64) -> (f64, f64) {
        (self.pic[0] + fx * self.pic[2], self.pic[1] + fy * self.pic[3])
    }

    /// Screen points per comp pixel.
    fn scale(&self) -> f64 {
        self.pic[2] / self.size.0
    }

    fn click(&mut self, (x, y): (f64, f64), modifiers: Value) {
        self.ok("ui.click", json!({"x": x, "y": y, "modifiers": modifiers}));
        self.frames(2);
    }

    fn drag(&mut self, (x, y): (f64, f64), (dx, dy): (f64, f64), modifiers: Value) {
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + dx, "y": y + dy}, "steps": 8, "modifiers": modifiers}));
        self.frames(3);
    }

    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().map_or(0, Vec::len)
    }

    fn node(&mut self, node: u64) -> Value {
        let g = self.exec("comp.get", json!({"comp": self.comp}));
        g["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!(node)).cloned().unwrap()
    }
}

/// The vertices of roto shape `k`'s last keyframe (the tools key the playhead's frame).
fn shape_vertices(node: &Value, k: usize) -> Vec<Value> {
    let sh = &node["kind"]["roto"]["shapes"][k]["shape"];
    let kf = sh["keyframes"].as_array().and_then(|a| a.last()).unwrap_or_else(|| panic!("shape {k} has a keyframe: {sh}"));
    kf["value"]["Path"]["vertices"].as_array().cloned().unwrap_or_else(|| panic!("a path: {kf}"))
}

fn xy(v: &Value) -> (f64, f64) {
    (v["x"].as_f64().unwrap_or(0.0), v["y"].as_f64().unwrap_or(0.0))
}

fn none() -> Value {
    json!({})
}

fn alt() -> Value {
    json!({"alt": true})
}

#[test]
fn roto_shapes_are_drawn_and_edited_on_the_viewer() {
    let (mut d, node) = Driver::with_node(Workflow::Resolve, "roto");
    assert!(d.has("compViewer.roto.bezier") && d.has("compViewer.roto.bSpline") && d.has("compViewer.roto.select"));
    let tool = d.ok("ui.inspect", json!({}))["ui"]["panels"]["compViewer"].clone();
    assert_eq!(tool["rotoTool"], json!(null), "automatic: Bézier while the node has no shape: {tool}");
    let u0 = d.undo_len();
    // click, drag (a curved point), click, then click the first point: one shape, one undo step
    d.click(d.scr(0.3, 0.3), none());
    d.drag(d.scr(0.7, 0.3), (0.0, 30.0), none());
    d.click(d.scr(0.5, 0.7), none());
    assert!(d.has("compViewer.draft.first"), "the shape being drawn");
    let first = d.at("compViewer.draft.first");
    d.click(first, none());
    let n = d.node(node);
    let shapes = n["kind"]["roto"]["shapes"].as_array().cloned().unwrap_or_default();
    assert_eq!(shapes.len(), 1, "{n}");
    assert_eq!(shapes[0]["kind"], json!("bezier"));
    let vs = shape_vertices(&n, 0);
    assert_eq!(vs.len(), 3, "{vs:?}");
    let (px, py) = xy(&vs[0]["p"]);
    assert!((px - d.size.0 * 0.3).abs() < 3.0 && (py - d.size.1 * 0.3).abs() < 3.0, "the first point where clicked: {vs:?}");
    let (ox, oy) = xy(&vs[1]["t_out"]);
    let (ix, iy) = xy(&vs[1]["t_in"]);
    assert!(oy > 10.0 && ox.abs() < 1.0 && (ix + ox).abs() < 1e-6 && (iy + oy).abs() < 1e-6, "the dragged point is smooth along the drag: {vs:?}");
    assert_eq!(d.undo_len(), u0 + 1, "drawing a shape is one undo step");
    let tool = d.ok("ui.inspect", json!({}))["ui"]["panels"]["compViewer"].clone();
    assert_eq!(tool["selected"], json!([0, 0]), "the new shape's first point is selected: {tool}");

    // Select (automatic now): drag point 0
    let u1 = d.undo_len();
    let p0 = d.at("compViewer.roto0.point0");
    d.drag(p0, (20.0, 10.0), none());
    let vs = shape_vertices(&d.node(node), 0);
    let (qx, qy) = xy(&vs[0]["p"]);
    let s = d.scale();
    assert!((qx - px - 20.0 / s).abs() < 1.5 && (qy - py - 10.0 / s).abs() < 1.5, "point 0 followed the pointer: {vs:?}");
    assert_eq!(d.undo_len(), u1 + 1, "a point drag is one undo step");

    // its outer feather handle: drag it out along its line
    assert!(d.has("compViewer.roto.featherOut") && d.has("compViewer.roto.featherIn"));
    let (p, f) = (d.at("compViewer.roto0.point0"), d.at("compViewer.roto.featherOut"));
    let (ux, uy) = (f.0 - p.0, f.1 - p.1);
    let l = (ux * ux + uy * uy).sqrt().max(1e-6);
    d.drag(f, (ux / l * 20.0, uy / l * 20.0), none());
    let vs = shape_vertices(&d.node(node), 0);
    let fo = vs[0]["feather_out"].as_f64().unwrap_or(0.0);
    assert!((fo - 20.0 / s).abs() < 2.0 / s + 0.5, "outer feather ≈ the drag: {fo} ({vs:?})");
    assert_eq!(vs[0]["feather_in"].as_f64().unwrap_or(0.0), 0.0);
    assert_eq!(d.undo_len(), u1 + 2);

    // the selected shape's Bézier handle: select point 1, drag its out handle
    let p1 = d.at("compViewer.roto0.point1");
    d.click(p1, none());
    assert!(d.has("compViewer.roto0.point1.out"));
    let before = xy(&shape_vertices(&d.node(node), 0)[1]["t_out"]);
    let h = d.at("compViewer.roto0.point1.out");
    d.drag(h, (15.0, 0.0), none());
    let vs = shape_vertices(&d.node(node), 0);
    let (o, i) = (xy(&vs[1]["t_out"]), xy(&vs[1]["t_in"]));
    assert!(o.0 > before.0 + 10.0 / s - 1.5, "the out handle followed: {before:?} → {o:?}");
    assert!((o.0 + i.0).abs() < 1e-6 && (o.1 + i.1).abs() < 1e-6, "the in handle mirrors it: {vs:?}");
    assert_eq!(d.undo_len(), u1 + 3);

    // Alt-drag a point: the whole shape moves
    let before: Vec<(f64, f64)> = shape_vertices(&d.node(node), 0).iter().map(|v| xy(&v["p"])).collect();
    let p2 = d.at("compViewer.roto0.point2");
    d.drag(p2, (0.0, -20.0), alt());
    let after: Vec<(f64, f64)> = shape_vertices(&d.node(node), 0).iter().map(|v| xy(&v["p"])).collect();
    for (a, b) in before.iter().zip(&after) {
        assert!((b.0 - a.0).abs() < 1.5 && (b.1 - a.1 + 20.0 / s).abs() < 1.5, "every point moved: {before:?} → {after:?}");
    }
    assert_eq!(d.undo_len(), u1 + 4);

    // Alt-click point 0 (a corner): it becomes smooth
    let p0 = d.at("compViewer.roto0.point0");
    d.click(p0, alt());
    let vs = shape_vertices(&d.node(node), 0);
    assert_ne!(xy(&vs[0]["t_out"]), (0.0, 0.0), "Alt-click made point 0 smooth: {vs:?}");

    // B-Spline: four clicks and Enter
    d.ok("ui.click", json!({"id": "compViewer.roto.bSpline"}));
    d.frames(2);
    for (fx, fy) in [(0.1, 0.6), (0.25, 0.6), (0.25, 0.85), (0.1, 0.85)] {
        d.click(d.scr(fx, fy), none());
    }
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    let n = d.node(node);
    let shapes = n["kind"]["roto"]["shapes"].as_array().cloned().unwrap_or_default();
    assert_eq!(shapes.len(), 2, "{n}");
    assert_eq!(shapes[1]["kind"], json!("bSpline"));
    let vs = shape_vertices(&n, 1);
    assert_eq!(vs.len(), 4);
    assert!(vs.iter().all(|v| xy(&v["t_out"]) == (0.0, 0.0)), "B-spline points have no handles: {vs:?}");
    // Escape drops a drawing in progress
    d.ok("ui.click", json!({"id": "compViewer.roto.bezier"}));
    d.frames(2);
    d.click(d.scr(0.8, 0.8), none());
    assert!(!d.ok("ui.inspect", json!({}))["ui"]["panels"]["compViewer"]["draft"].is_null());
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.ok("ui.inspect", json!({}))["ui"]["panels"]["compViewer"]["draft"].is_null());
    // undo takes back exactly the last shape
    let u = d.undo_len();
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.node(node)["kind"]["roto"]["shapes"].as_array().map(Vec::len), Some(1));
    assert_eq!(d.undo_len(), u - 1);
}

fn paint_strokes(d: &mut Driver, node: u64) -> Vec<Value> {
    d.node(node)["kind"]["paint"]["strokes"].as_array().cloned().unwrap_or_default()
}

fn paint_round_trip(workflow: Workflow) {
    let (mut d, node) = Driver::with_node(workflow, "paint");
    for id in ["compViewer.paint.color", "compViewer.paint.erase", "compViewer.paint.size", "compViewer.paint.hardness", "compViewer.paint.opacity"] {
        assert!(d.has(id), "{workflow:?}: {id}");
    }
    // the brush is UI state: set it over the control channel
    d.ok("ui.set", json!({"panels": {"compViewer": {"brush": {"size": 30.0, "hardness": 0.5, "opacity": 0.75}}}}));
    d.frames(2);
    let u0 = d.undo_len();
    d.drag(d.scr(0.3, 0.5), (120.0, 40.0), none());
    let strokes = paint_strokes(&mut d, node);
    assert_eq!(strokes.len(), 1, "{workflow:?}: one stroke per drag: {strokes:?}");
    let s = &strokes[0];
    assert_eq!(s["tool"], json!("color"));
    assert_eq!((s["brush"]["size"].as_f64(), s["brush"]["hardness"].as_f64(), s["brush"]["opacity"].as_f64()), (Some(30.0), Some(0.5), Some(0.75)));
    let pts = s["points"].as_array().cloned().unwrap_or_default();
    assert!(pts.len() >= 3, "{workflow:?}: the stroke follows the drag: {s}");
    assert!(pts.iter().all(|p| p["pressure"] == json!(1.0)), "mouse pressure is 1");
    let (x0, y0) = (pts[0]["x"].as_f64().unwrap(), pts[0]["y"].as_f64().unwrap());
    assert!((x0 - d.size.0 * 0.3).abs() < 3.0 && (y0 - d.size.1 * 0.5).abs() < 3.0, "{workflow:?}: starts where the drag did: {s}");
    assert_eq!(d.undo_len(), u0 + 1, "{workflow:?}: one undo step per stroke");
    // the Erase tool, a single-frame stroke
    d.ok("ui.click", json!({"id": "compViewer.paint.erase"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "compViewer.paint.singleFrame"}));
    d.frames(2);
    d.drag(d.scr(0.5, 0.3), (-60.0, 60.0), none());
    let strokes = paint_strokes(&mut d, node);
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[1]["tool"], json!("erase"));
    assert_eq!(strokes[1]["duration"]["type"], json!("singleFrame"), "{}", strokes[1]);
    assert_eq!(d.undo_len(), u0 + 2);
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(paint_strokes(&mut d, node).len(), 1, "{workflow:?}: undo removes the last stroke only");
}

#[test]
fn paint_strokes_on_the_viewer_resolve() {
    paint_round_trip(Workflow::Resolve);
}

#[test]
fn paint_strokes_on_the_viewer_premiere() {
    paint_round_trip(Workflow::Premiere);
}

#[test]
fn tracker_points_are_placed_and_sized_on_the_viewer() {
    let (mut d, node) = Driver::with_node(Workflow::Resolve, "tracker");
    let s = d.scale();
    let u0 = d.undo_len();
    // a plain click adds nothing; Ctrl-click (Cmd on macOS) adds a point there
    d.click(d.scr(0.5, 0.5), none());
    assert_eq!(d.node(node)["kind"]["points"].as_array().map(Vec::len), Some(0));
    d.click(d.scr(0.5, 0.5), json!({"ctrl": true, "command": true}));
    let pts = d.node(node)["kind"]["points"].as_array().cloned().unwrap_or_default();
    assert_eq!(pts.len(), 1, "{pts:?}");
    let (x, y) = xy(&pts[0]["track"]["value"]["Vec2"]);
    assert!((x - d.size.0 * 0.5).abs() < 3.0 && (y - d.size.1 * 0.5).abs() < 3.0, "{pts:?}");
    assert_eq!(d.undo_len(), u0 + 1);
    // drag its centre
    let c = d.at("compViewer.track.point0");
    d.drag(c, (30.0, -15.0), none());
    let pts = d.node(node)["kind"]["points"].as_array().cloned().unwrap_or_default();
    let (x1, y1) = xy(&pts[0]["track"]["value"]["Vec2"]);
    assert!((x1 - x - 30.0 / s).abs() < 1.5 && (y1 - y + 15.0 / s).abs() < 1.5, "the point followed: ({x}, {y}) → ({x1}, {y1})");
    assert_eq!(d.undo_len(), u0 + 2, "one undo step for the drag");
    // the pattern box corner: a bigger pattern
    let pat0 = xy(&pts[0]["pattern"]);
    let h = d.at("compViewer.track.pattern0");
    d.drag(h, (12.0, 12.0), none());
    let pts = d.node(node)["kind"]["points"].as_array().cloned().unwrap_or_default();
    let pat1 = xy(&pts[0]["pattern"]);
    assert!(pat1.0 > pat0.0 + 8.0 / s - 1.5 && pat1.1 > pat0.1 + 8.0 / s - 1.5, "{pat0:?} → {pat1:?}");
    assert_eq!(xy(&pts[0]["track"]["value"]["Vec2"]), (x1, y1), "resizing leaves the point where it is");
    // the search box corner: a wider search
    let sea0 = xy(&pts[0]["search"]);
    let h = d.at("compViewer.track.search0");
    d.drag(h, (20.0, 0.0), none());
    let pts = d.node(node)["kind"]["points"].as_array().cloned().unwrap_or_default();
    let sea1 = xy(&pts[0]["search"]);
    assert!(sea1.0 > sea0.0 + 15.0 / s - 1.5, "{sea0:?} → {sea1:?}");
    assert_eq!(d.undo_len(), u0 + 4);
}

#[test]
fn planar_region_and_corners_are_edited_on_the_viewer() {
    let (mut d, node) = Driver::with_node(Workflow::Resolve, "planarTracker");
    assert!(d.has("compViewer.planar.draw") && d.has("compViewer.planar.edit"));
    // no region yet: drawing it is automatic
    let u0 = d.undo_len();
    for (fx, fy) in [(0.3, 0.3), (0.7, 0.35), (0.65, 0.7), (0.35, 0.65)] {
        d.click(d.scr(fx, fy), none());
    }
    let first = d.at("compViewer.draft.first");
    d.click(first, none());
    let n = d.node(node);
    let region: Vec<(f64, f64)> = n["kind"]["region"]["vertices"].as_array().cloned().unwrap_or_default().iter().map(|v| xy(&v["p"])).collect();
    assert_eq!(region.len(), 4, "{n}");
    assert!((region[1].0 - d.size.0 * 0.7).abs() < 3.0 && (region[1].1 - d.size.1 * 0.35).abs() < 3.0, "{region:?}");
    assert_eq!(d.undo_len(), u0 + 1);
    // edit (automatic now): drag a corner of the plane; the region stays as drawn
    assert!(d.has("compViewer.planar.corner0") && d.has("compViewer.planar.region1"));
    let s = d.scale();
    let c0 = d.at("compViewer.planar.corner0");
    d.drag(c0, (-20.0, -10.0), none());
    let n = d.node(node);
    let q0 = xy(&n["kind"]["quad"][0]);
    let (lo_x, lo_y) = (d.size.0 * 0.3, d.size.1 * 0.3);
    assert!((q0.0 - lo_x + 20.0 / s).abs() < 2.0 && (q0.1 - lo_y + 10.0 / s).abs() < 2.0, "corner 0 followed: {q0:?}");
    let r2: Vec<(f64, f64)> = n["kind"]["region"]["vertices"].as_array().cloned().unwrap_or_default().iter().map(|v| xy(&v["p"])).collect();
    assert_eq!(r2, region, "the drawn region is not the quad, so it stays");
    assert_eq!(d.undo_len(), u0 + 2, "one undo step for the corner drag");
    // drag a region point
    let r1 = d.at("compViewer.planar.region1");
    d.drag(r1, (10.0, 10.0), none());
    let n = d.node(node);
    let r3: Vec<(f64, f64)> = n["kind"]["region"]["vertices"].as_array().cloned().unwrap_or_default().iter().map(|v| xy(&v["p"])).collect();
    assert!((r3[1].0 - region[1].0 - 10.0 / s).abs() < 1.5 && (r3[1].1 - region[1].1 - 10.0 / s).abs() < 1.5, "{region:?} → {r3:?}");
    assert_eq!(xy(&n["kind"]["quad"][0]), q0, "the quad stays");
    assert_eq!(d.undo_len(), u0 + 3);
}

/// Review fix: a roto shape following a tracker (C3.7) keeps its keyframes in the tracker's
/// reference-frame coordinates. The viewer draws it where the tracker puts it at the playhead and
/// converts a drag back, so the edit lands where it was made (it used to show and key the
/// stored, reference-frame outline as if it were the screen one).
#[test]
fn a_shape_following_a_tracker_is_edited_where_it_is_drawn() {
    use filmcraft_geom::Vec2;
    use filmcraft_project::comp::TrackerPoint;
    use filmcraft_project::{Keyframe, ParamValue};
    let (mut d, node) = Driver::with_node(Workflow::Resolve, "roto");
    for (fx, fy) in [(0.3, 0.3), (0.7, 0.3), (0.5, 0.7)] {
        d.click(d.scr(fx, fy), none());
    }
    let first = d.at("compViewer.draft.first");
    d.click(first, none());
    let drawn = xy(&shape_vertices(&d.node(node), 0)[0]["p"]);
    // a tracker whose point "A" moves (+1, +0.5) px a frame, with its reference frame at frame 240
    let tracker = d.exec("comp.addNode", json!({"comp": d.comp, "kind": "tracker", "pos": [-400, 0]}))["node"].as_u64().unwrap();
    let frame = 254_016_000_000i64 / 24;
    let mut a = TrackerPoint::at("A", Vec2::new(100.0, 100.0));
    a.track.keyframes =
        (0..=240).map(|f| Keyframe::new(filmcraft_time::Tick(f * frame), ParamValue::Vec2(Vec2::new(100.0 + f as f64, 100.0 + 0.5 * f as f64)))).collect();
    d.exec("comp.track.setPoints", json!({"comp": d.comp, "node": tracker, "points": serde_json::to_value(vec![a]).unwrap()}));
    d.exec("comp.track.setOptions", json!({"comp": d.comp, "node": tracker, "reference": 240 * frame}));
    d.exec("comp.roto.linkTracker", json!({"comp": d.comp, "node": node, "shape": 0, "tracker": tracker, "output": "A"}));
    d.ok("ui.set", json!({"panels": {"comp": {"canvas": {"selection": [node]}}}}));
    d.frames(4);
    let stored = xy(&shape_vertices(&d.node(node), 0)[0]["p"]);
    assert!((stored.0 - drawn.0).abs() > 50.0, "the link stored the shape in reference-frame coordinates: {drawn:?} → {stored:?}");
    // the handle is where the shape is drawn (it stayed put on screen when linked)
    let s = d.scale();
    let (want, p0) = (d.scr(0.3, 0.3), d.at("compViewer.roto0.point0"));
    assert!((p0.0 - want.0).abs() < 3.0 && (p0.1 - want.1).abs() < 3.0, "point 0 is shown where the tracker puts it: {p0:?}, want {want:?}");
    let u = d.undo_len();
    d.drag(p0, (20.0, 10.0), none());
    let p1 = d.at("compViewer.roto0.point0");
    assert!((p1.0 - p0.0 - 20.0).abs() < 1.5 && (p1.1 - p0.1 - 10.0).abs() < 1.5, "the drawn point followed the pointer: {p0:?} → {p1:?}");
    let after = xy(&shape_vertices(&d.node(node), 0)[0]["p"]);
    assert!(
        (after.0 - stored.0 - 20.0 / s).abs() < 1.5 && (after.1 - stored.1 - 10.0 / s).abs() < 1.5,
        "keyed back in reference-frame coordinates: {stored:?} → {after:?}"
    );
    assert_eq!(d.undo_len(), u + 1, "one undo step");
}

/// Review fix: at a small zoom a tracker point's pattern- and search-box handles used to sit a
/// few screen points from its centre, under it; they are now pushed apart so each can be grabbed.
#[test]
fn tracker_box_handles_stay_apart_at_small_zoom() {
    let (mut d, node) = Driver::with_node(Workflow::Premiere, "tracker");
    d.click(d.scr(0.5, 0.5), json!({"ctrl": true, "command": true}));
    // the smallest boxes: a few comp pixels, well under a handle on screen
    let mut pts = d.node(node)["kind"]["points"].clone();
    pts[0]["pattern"] = json!({"x": 1.0, "y": 1.0});
    pts[0]["search"] = json!({"x": 1.0, "y": 1.0});
    d.exec("comp.track.setPoints", json!({"comp": d.comp, "node": node, "points": pts}));
    d.frames(3);
    let pts = d.node(node)["kind"]["points"].clone();
    let (pat0, sea0) = (xy(&pts[0]["pattern"]), xy(&pts[0]["search"]));
    let (c, p, s) = (d.at("compViewer.track.point0"), d.at("compViewer.track.pattern0"), d.at("compViewer.track.search0"));
    let dist = |a: (f64, f64), b: (f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    assert!(dist(c, p) >= 16.0 && dist(p, s) >= 16.0, "centre {c:?}, pattern {p:?}, search {s:?}");
    // grabbing the pattern handle grows the pattern box (not moving the point), by the drag
    let centre = xy(&pts[0]["track"]["value"]["Vec2"]);
    let sc = d.scale();
    d.drag(p, (12.0, 12.0), none());
    let pts = d.node(node)["kind"]["points"].clone();
    assert_eq!(xy(&pts[0]["track"]["value"]["Vec2"]), centre, "the point stays");
    let pat1 = xy(&pts[0]["pattern"]);
    assert!(pat1.0 > pat0.0 + 6.0 / sc && pat1.1 > pat0.1 + 6.0 / sc, "{pat0:?} → {pat1:?}");
    assert_eq!(xy(&pts[0]["search"]), sea0);
}
