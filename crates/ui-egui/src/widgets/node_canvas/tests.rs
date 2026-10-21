//! The node canvas on a headless egui context: ids, connecting, moving, disconnecting, keys.

use super::*;
use egui::{Event, Modifiers, RawInput};

fn graph() -> CanvasGraph {
    let node = |id: u64, x: f32, ins: usize, is_output: bool| NodeView {
        id,
        title: format!("N{id}"),
        pos: [x, 40.0],
        inputs: (0..ins).map(|i| format!("in{i}")).collect(),
        outputs: if is_output { vec![] } else { vec!["out".into()] },
        bypassed: false,
        error: None,
        is_output,
        key_inputs: 0,
        key_outputs: 0,
    };
    CanvasGraph {
        nodes: vec![node(1, 0.0, 0, false), node(2, 220.0, 2, false), node(3, 440.0, 1, true)],
        edges: vec![EdgeView { from: (1, 0), to: (2, 0) }],
        add_kinds: vec![("blur".into(), "Blur".into())],
        node_actions: Vec::new(),
        canvas_actions: Vec::new(),
        on_screen_ids: false,
    }
}

struct Rig {
    ctx: egui::Context,
    reg: Registry,
    state: CanvasState,
    g: CanvasGraph,
}

impl Rig {
    fn new() -> Rig {
        Rig { ctx: egui::Context::default(), reg: Registry::default(), state: CanvasState::default(), g: graph() }
    }

    /// One frame with `events`; returns the canvas events.
    fn frame(&mut self, mut events: Vec<Event>, modifiers: Modifiers) -> Vec<CanvasEvent> {
        events.insert(0, Event::ModifiersChanged(modifiers));
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 500.0))), events, ..Default::default() };
        let mut out = Vec::new();
        self.reg.begin_frame();
        let (reg, state, g) = (&mut self.reg, &mut self.state, &self.g);
        self.ctx.run_ui(input, |ui| out = node_canvas(ui, g, state, reg, "comp")).drop_without_applying_deltas();
        out
    }

    fn centre(&self, id: &str) -> Pos2 {
        let e = self.reg.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("no element {id}"));
        pos2(e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0)
    }

    /// Press at `a`, move to `b` in steps, release: every frame's events.
    fn drag(&mut self, a: Pos2, b: Pos2) -> Vec<CanvasEvent> {
        let none = Modifiers::default();
        let mut all = self.frame(vec![Event::PointerMoved(a)], none);
        all.extend(self.frame(vec![Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: none }], none));
        for k in 1..=4 {
            let p = a + (b - a) * (k as f32 / 4.0);
            all.extend(self.frame(vec![Event::PointerMoved(p)], none));
        }
        all.extend(self.frame(vec![Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: none }], none));
        all.extend(self.frame(vec![], none));
        all
    }

    fn click(&mut self, p: Pos2) -> Vec<CanvasEvent> {
        let none = Modifiers::default();
        let mut all = self.frame(vec![Event::PointerMoved(p)], none);
        all.extend(self.frame(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: none }], none));
        all.extend(self.frame(vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: none }], none));
        all
    }
}

#[test]
fn every_part_registers_an_automation_id() {
    let mut r = Rig::new();
    r.frame(vec![], Modifiers::default());
    for id in [
        "comp.canvas",
        "comp.node.1",
        "comp.node.2",
        "comp.node.3",
        "comp.port.1.out.0",
        "comp.port.2.in.0",
        "comp.port.2.in.1",
        "comp.port.3.in.0",
        "comp.edge.1-2.0",
    ] {
        assert!(r.reg.elements.iter().any(|e| e.id == id), "{id}");
    }
    assert!(!r.reg.elements.iter().any(|e| e.id == "comp.port.3.out.0"), "the output node has no output port");
}

/// Fork C2: key ports are flagged per port; nodes off screen register no automation id; `fit`
/// brings every node into view and tames hostile positions and sizes.
#[test]
fn key_ports_offscreen_nodes_and_fit() {
    assert!(is_key(0b10, 1) && !is_key(0b10, 0) && !is_key(u32::MAX, 40) && is_key(u32::MAX, 31));
    let mut r = Rig::new();
    r.g.on_screen_ids = true;
    r.g.nodes[2].pos = [100_000.0, 0.0];
    r.frame(vec![], Modifiers::default());
    assert!(r.reg.elements.iter().any(|e| e.id == "comp.node.1"));
    assert!(!r.reg.elements.iter().any(|e| e.id == "comp.node.3" || e.id == "comp.port.3.in.0"), "off screen");
    let mut st = CanvasState::default();
    fit(&mut st, &r.g, vec2(800.0, 600.0));
    assert!(st.zoom >= MIN_ZOOM && st.zoom <= 1.0 && st.pan.iter().all(|v| v.is_finite()));
    r.g.nodes[0].pos = [f32::NAN, f32::INFINITY];
    fit(&mut r.state, &r.g, vec2(0.0, -5.0));
    assert!(r.state.zoom.is_finite() && r.state.pan.iter().all(|v| v.is_finite()));
    fit(&mut r.state, &CanvasGraph::default(), vec2(800.0, 600.0));
    assert_eq!(r.state.zoom, 1.0);
    // a small graph fits whole at 1:1
    let mut r = Rig::new();
    fit(&mut r.state, &r.g, vec2(900.0, 500.0));
    r.frame(vec![], Modifiers::default());
    assert!(["comp.node.1", "comp.node.2", "comp.node.3"].iter().all(|id| r.reg.elements.iter().any(|e| e.id == *id)));
}

#[test]
fn dragging_an_output_to_an_input_connects() {
    let mut r = Rig::new();
    r.frame(vec![], Modifiers::default());
    let (a, b) = (r.centre("comp.port.2.out.0"), r.centre("comp.port.3.in.0"));
    let ev = r.drag(a, b);
    assert!(ev.contains(&CanvasEvent::Connect { from: (2, 0), to: (3, 0) }), "{ev:?}");
}

#[test]
fn dragging_a_node_moves_it_and_ends_once() {
    let mut r = Rig::new();
    r.frame(vec![], Modifiers::default());
    let a = r.centre("comp.node.2") - vec2(0.0, 20.0);
    let ev = r.drag(a, a + vec2(60.0, 30.0));
    let ends: Vec<_> = ev.iter().filter(|e| matches!(e, CanvasEvent::MoveEnd(..))).collect();
    assert_eq!(ends.len(), 1, "{ev:?}");
    let CanvasEvent::MoveEnd(id, p) = ends[0] else { unreachable!() };
    assert_eq!(*id, 2);
    // egui starts a drag after a few points of movement; the node follows the pointer from there
    assert!((p[0] - 280.0).abs() < 8.0 && (p[1] - 70.0).abs() < 8.0, "{p:?}");
    assert_eq!(r.state.selection, vec![2]);
}

#[test]
fn clicking_a_connected_input_disconnects_and_keys_act_on_the_selection() {
    let mut r = Rig::new();
    r.frame(vec![], Modifiers::default());
    let ev = r.click(r.centre("comp.port.2.in.0"));
    assert!(ev.contains(&CanvasEvent::Disconnect { to: (2, 0) }), "{ev:?}");
    // an unconnected input does nothing
    let ev = r.click(r.centre("comp.port.2.in.1"));
    assert!(!ev.iter().any(|e| matches!(e, CanvasEvent::Disconnect { .. })));
    // select node 3, then Delete and Ctrl+B
    let ev = r.click(r.centre("comp.node.3") - vec2(0.0, 15.0));
    assert!(ev.contains(&CanvasEvent::Select(vec![3])), "{ev:?}");
    let key = |k: Key, m: Modifiers| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
    let ev = r.frame(vec![key(Key::Delete, Modifiers::default())], Modifiers::default());
    assert!(ev.contains(&CanvasEvent::Delete(vec![3])), "{ev:?}");
    let ctrl = Modifiers::COMMAND;
    let ev = r.frame(vec![key(Key::B, ctrl)], ctrl);
    assert!(ev.contains(&CanvasEvent::Bypass(vec![3])), "{ev:?}");
}

#[test]
fn state_round_trips_and_hostile_values_are_tamed() {
    let s = CanvasState { pan: [1.0, 2.0], zoom: 2.0, selection: vec![4], viewed: Some(4) };
    let back: CanvasState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    let partial: CanvasState = serde_json::from_str(r#"{"zoom": 1000}"#).unwrap();
    assert_eq!(partial.selection, Vec::<u64>::new());
    let mut r = Rig::new();
    r.state = CanvasState { pan: [f32::NAN, f32::INFINITY], zoom: 1000.0, ..Default::default() };
    r.g.nodes[0].pos = [f32::NAN, 1e30];
    r.frame(vec![], Modifiers::default());
    assert_eq!(r.state.zoom, MAX_ZOOM);
    assert!(r.state.pan.iter().all(|v| v.is_finite()));
}
