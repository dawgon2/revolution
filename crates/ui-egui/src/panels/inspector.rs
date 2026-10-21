//! The Resolve workflow's Inspector (fork RESOLVE-UI-PLAN RS4.2): the selected clip's settings in
//! Resolve's layout, laid out from observation only (FilmCraft's own drawing and icons). A row of
//! tabs (Video, Audio, Effects, Transition, Image, File; greyed where they don't apply), then
//! collapsible sections: an on dot, the name, a keyframe diamond and a reset arrow on the header;
//! rows with the label right-aligned, dark value fields, a keyframe diamond and a reset arrow.
//!
//! It edits the same clip parameters as Effect Controls through the same engine commands
//! (`effects.setParam`, `effects.addKeyframe`, `effects.resetParam`, `effects.reset`,
//! `effects.toggleEnabled`, `effects.apply`, `effects.remove`, `clip.speedDuration`, `tnr.*`), so
//! no parameter maths is written twice; only the units shown are Resolve's (see
//! `inspector_video.rs`). There is no keyframe lane: a diamond adds or removes a keyframe at the
//! playhead.
//!
//! Automation ids: `inspector.tab.<video|audio|effects|transition|image|file>`; a section's
//! `inspector.section.<section>` (its header), `inspector.<section>.enable` (the dot),
//! `inspector.<section>.keyframe`, `inspector.<section>.reset`, `inspector.<section>.remove`; a
//! row's `inspector.<section>.<param>` (its field; `…X` / `…Y` for a pair), `.slider`,
//! `.keyframe` and `.reset`. Sections: `transform`, `cropping`, `composite`, `speed`,
//! `stabilization`, `tnr` (Video), the audio effects by id (`volume`, `panner`, …), the clip's
//! other effects by id (Effects), `file` (File).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ClipId, EffectDef, EffectInstance, ParamDef, ParamKind, ParamValue, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// The commands a frame of the panel asks for, run in order at its end.
pub(crate) type Acts = Vec<(String, Value)>;

/// Height of a parameter row and of a section header.
pub(crate) const ROW_H: f32 = 26.0;
const HEAD_H: f32 = 28.0;
/// Height of the tab row (icon over label).
const TABS_H: f32 = 50.0;

/// The Inspector's tabs, in Resolve's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Video,
    Audio,
    Effects,
    Transition,
    Image,
    File,
}

impl Tab {
    pub(crate) const ALL: [Tab; 6] = [Tab::Video, Tab::Audio, Tab::Effects, Tab::Transition, Tab::Image, Tab::File];
    pub(crate) fn key(self) -> &'static str {
        match self {
            Tab::Video => "video",
            Tab::Audio => "audio",
            Tab::Effects => "effects",
            Tab::Transition => "transition",
            Tab::Image => "image",
            Tab::File => "file",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Tab::Video => tl!("Video"),
            Tab::Audio => tl!("Audio"),
            Tab::Effects => tl!("Effects"),
            Tab::Transition => tl!("Transition"),
            Tab::Image => tl!("Image"),
            Tab::File => tl!("File"),
        }
    }
    fn from_key(k: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|t| t.key() == k)
    }
}

/// The clip a tab edits: its id, a copy of it, and the media time under the playhead.
pub(crate) struct Cx {
    pub clip: ClipId,
    pub it: TrackItem,
    pub mt: Tick,
}

impl Cx {
    pub(crate) fn new(app: &FilmcraftApp, clip: ClipId, it: TrackItem) -> Cx {
        let ph = app.session.playhead();
        let mt = it.effect_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
        Cx { clip, it, mt }
    }
}

/// A parameter's keyframe state at the playhead (how its diamond is drawn).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// Not animated: an outline.
    Off,
    /// Animated, no keyframe here: an accent outline.
    Animated,
    /// A keyframe at the playhead: filled.
    Here,
}

pub(crate) fn key_of(e: Option<&EffectInstance>, param: &str, mt: Tick) -> Key {
    match e.and_then(|e| e.param(param)) {
        Some(p) if p.keyframes.iter().any(|k| k.time == mt) => Key::Here,
        Some(p) if p.is_animated() => Key::Animated,
        _ => Key::Off,
    }
}

/// A section's diamond: filled when every parameter has a keyframe here.
pub(crate) fn key_all(keys: &[Key]) -> Key {
    if !keys.is_empty() && keys.iter().all(|k| *k == Key::Here) {
        Key::Here
    } else if keys.iter().any(|k| *k != Key::Off) {
        Key::Animated
    } else {
        Key::Off
    }
}

/// The section diamond's click: remove the keyframes here when every parameter has one, else
/// add one to each parameter that has none (`effects.addKeyframe` adds or removes).
pub(crate) fn section_keyframes(acts: &mut Acts, clip: ClipId, effect: Value, params: &[(&str, Key)]) {
    let all = params.iter().all(|(_, k)| *k == Key::Here);
    for (p, k) in params {
        if all || *k != Key::Here {
            acts.push(("effects.addKeyframe".into(), json!({"clip": clip.0, "effect": effect, "param": p})));
        }
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clips = crate::panels::effect_controls::selected_clips(app);
    let video = clips.iter().find(|c| c.2 == TrackKind::Video).cloned();
    let audio = clips.iter().find(|c| c.2 == TrackKind::Audio).cloned();
    let transition = crate::panels::transition_controls::selected(app);
    let file = clips.first().and_then(|(_, it, _)| app.session.project.resolve_media(it.item).map(|_| it.clone()));
    let applies = |tab: Tab| match tab {
        Tab::Video | Tab::Effects => video.is_some(),
        Tab::Audio => audio.is_some(),
        Tab::Transition => transition.is_some(),
        // (Resolve's Image tab holds camera raw decode settings; FilmCraft has none)
        Tab::Image => false,
        Tab::File => file.is_some(),
    };
    // the tab the user picked, else the first that applies
    let picked = Tab::from_key(&app.ui.inspector_tab).filter(|tb| applies(*tb));
    let fallback = [Tab::Video, Tab::Audio, Tab::Transition, Tab::Effects, Tab::File].into_iter().find(|tb| applies(*tb));
    let active = picked.or(fallback);
    // the tab row
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TABS_H));
    let cell_w = (bar.width() / Tab::ALL.len() as f32).max(1.0);
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        let cell = Rect::from_min_size(pos2(bar.min.x + cell_w * i as f32, bar.min.y), vec2(cell_w, TABS_H));
        let on = applies(tab);
        let resp = ui.interact(cell, egui::Id::new(("inspector-tab", tab.key())), if on { Sense::click() } else { Sense::hover() });
        let ink = if Some(tab) == active {
            t.accent
        } else if !on {
            t.text_faint
        } else if resp.hovered() {
            t.text
        } else {
            t.text_dim
        };
        tab_icon(ui.painter(), Rect::from_center_size(pos2(cell.center().x, cell.min.y + 17.0), vec2(18.0, 18.0)), tab, ink);
        let p = ui.painter().with_clip_rect(cell);
        p.text(pos2(cell.center().x, cell.min.y + 37.0), Align2::CENTER_CENTER, tab.label(), Tokens::ui(11.0), ink);
        app.auto.add(&format!("inspector.tab.{}", tab.key()), cell, tab.label());
        if resp.clicked() {
            app.ui.inspector_tab = tab.key().to_string();
        }
    }
    ui.painter().line_segment([pos2(rect.min.x, bar.max.y), pos2(rect.max.x, bar.max.y)], Stroke::new(1.0, t.separator));
    let body = Rect::from_min_max(pos2(rect.min.x, bar.max.y + 1.0), rect.max);
    let Some(active) = active else {
        crate::dock::placeholder(ui, body, &t, tl!("(no clip selected)"));
        return;
    };
    if active == Tab::Transition {
        if let Some(id) = transition {
            crate::panels::transition_controls::effect_controls(app, ui, body, id);
        }
        return;
    }
    let mut acts: Acts = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(0.0, 2.0))).id_salt("inspector-body"));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical().id_salt(("inspector-scroll", active.key())).auto_shrink([false, false]).show(&mut bui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        match active {
            Tab::Video => {
                if let Some((clip, it, _)) = video.clone() {
                    let cx = Cx::new(app, clip, it);
                    crate::panels::inspector_video::video_tab(app, ui, &cx, &mut acts);
                }
            }
            Tab::Audio => {
                if let Some((clip, it, _)) = audio.clone() {
                    let cx = Cx::new(app, clip, it);
                    audio_tab(app, ui, &cx, &mut acts);
                }
            }
            Tab::Effects => {
                if let Some((clip, it, _)) = video.clone() {
                    let cx = Cx::new(app, clip, it);
                    effects_tab(app, ui, &cx, &mut acts);
                }
            }
            Tab::File => {
                if let Some(it) = file.clone() {
                    crate::panels::inspector_video::file_tab(app, ui, &it);
                }
            }
            Tab::Transition | Tab::Image => {}
        }
        ui.add_space(12.0);
    });
    crate::panels::effect_controls::run(app, ui.ctx(), acts);
}

/// The tabs' icons: FilmCraft's own (`icons.rs`), and for Transition two overlapping frames.
fn tab_icon(p: &egui::Painter, r: Rect, tab: Tab, ink: Color32) {
    match tab {
        Tab::Video => icons::paint(p, r, Icon::Film, ink),
        Tab::Audio => icons::paint(p, r, Icon::Audio, ink),
        Tab::Effects => icons::paint(p, r, Icon::Fx, ink),
        Tab::Image => icons::paint(p, r, Icon::Image, ink),
        Tab::File => icons::paint(p, r, Icon::Info, ink),
        Tab::Transition => {
            let a = Rect::from_min_size(r.min + vec2(1.0, 3.0), vec2(10.0, 10.0));
            let b = Rect::from_min_size(r.min + vec2(7.0, 5.0), vec2(10.0, 10.0));
            p.rect_stroke(a, 1.5, Stroke::new(1.4, ink), egui::StrokeKind::Inside);
            p.rect_filled(b, 1.5, ink.gamma_multiply(0.55));
        }
    }
}

// ---------- shared pieces: headers, rows, fields ----------

/// What a section header's controls asked for.
#[derive(Default)]
pub(crate) struct Head {
    pub open: bool,
    pub dot: bool,
    pub key: bool,
    pub reset: bool,
    pub remove: bool,
}

/// A section header: on dot (`dot`: `Some(on)`), name, and at the right the keyframe diamond
/// (`key`), reset arrow (`reset`) and, for an effect that can be taken off, a bin (`remove`).
/// Clicking the rest of the header folds the section.
pub(crate) fn section(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    sec: &str,
    title: &str,
    dot: Option<bool>,
    key: Option<Key>,
    reset: bool,
    remove: bool,
) -> Head {
    let t = app.tokens;
    let fold = format!("insp:{sec}");
    let open = !app.ui.collapsed_fx.contains(&fold);
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, HEAD_H), Sense::click());
    ui.painter().rect_filled(r, 0.0, t.tab_bg);
    ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.separator));
    let mut out = Head { open, ..Head::default() };
    let on = dot.unwrap_or(true);
    if let Some(on) = dot {
        out.dot = dot_button(app, ui, pos2(r.min.x + 14.0, r.center().y), &format!("inspector.{sec}.enable"), on);
    }
    let title_x = r.min.x + if dot.is_some() { 28.0 } else { 12.0 };
    // the reset arrow and the diamond in the rows' columns, the bin left of them
    let cy = r.center().y;
    if reset {
        out.reset = reset_button(app, ui, pos2(r.max.x - 14.0, cy), &format!("inspector.{sec}.reset"));
    }
    if let Some(k) = key {
        out.key = keyframe_button(app, ui, pos2(r.max.x - 34.0, cy), &format!("inspector.{sec}.keyframe"), k);
    }
    let mut right = r.max.x - 44.0;
    if remove {
        right -= 20.0;
        let rr = Rect::from_center_size(pos2(right + 10.0, cy), vec2(14.0, 14.0));
        let rresp = ui.interact(rr.expand(2.0), egui::Id::new(("insp-remove", sec)), Sense::click()).on_hover_text(tl!("Clear"));
        icons::paint(ui.painter(), rr, Icon::Trash, if rresp.hovered() { t.text } else { t.text_dim });
        app.auto.add(&format!("inspector.{sec}.remove"), rr, tl!("Clear"));
        out.remove = rresp.clicked();
    }
    ui.painter().with_clip_rect(Rect::from_min_max(r.min, pos2(right - 4.0, r.max.y))).text(
        pos2(title_x, r.center().y),
        Align2::LEFT_CENTER,
        title,
        Tokens::semibold(12.5),
        if on { t.text } else { t.text_dim },
    );
    app.auto.add(&format!("inspector.section.{sec}"), r, title);
    if resp.clicked() && !out.dot && !out.key && !out.reset && !out.remove {
        if open {
            app.ui.collapsed_fx.push(fold);
        } else {
            app.ui.collapsed_fx.retain(|k| *k != fold);
        }
    }
    out
}

/// A row's layout: the label right-aligned in the left column, the value area, and the
/// keyframe diamond and reset arrow at the right.
pub(crate) struct Row {
    pub value: Rect,
    pub key: Pos2,
    pub reset: Pos2,
}

pub(crate) fn row(ui: &mut egui::Ui, label: &str, t: &Tokens) -> Row {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::hover());
    let label_r = r.min.x + (w * 0.34).clamp(84.0, 150.0);
    // right-aligned, cut off with "…" where it would run past the row's left edge
    let mut job = egui::text::LayoutJob::single_section(label.to_owned(), egui::TextFormat::simple(Tokens::ui(12.0), t.text_dim));
    job.wrap.max_width = (label_r - r.min.x - 6.0).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos2(label_r - g.size().x, r.center().y - g.size().y / 2.0), g, t.text_dim);
    let value = Rect::from_min_max(pos2(label_r + 10.0, r.min.y + 3.0), pos2((r.max.x - 50.0).max(label_r + 30.0), r.max.y - 3.0));
    Row { value, key: pos2(r.max.x - 34.0, r.center().y), reset: pos2(r.max.x - 14.0, r.center().y) }
}

fn diamond(c: Pos2, s: f32) -> Vec<Pos2> {
    vec![c + vec2(0.0, -s), c + vec2(s, 0.0), c + vec2(0.0, s), c + vec2(-s, 0.0)]
}

/// A keyframe diamond; returns whether it was clicked.
pub(crate) fn keyframe_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, c: Pos2, id: &str, state: Key) -> bool {
    let t = app.tokens;
    let r = Rect::from_center_size(c, vec2(14.0, 14.0));
    let resp = ui.interact(r.expand(2.0), egui::Id::new(("insp-key", id)), Sense::click()).on_hover_text(tl!("Add/Remove Keyframe"));
    let s = 4.8;
    match state {
        Key::Here => {
            ui.painter().add(egui::Shape::convex_polygon(diamond(c, s), t.accent, Stroke::NONE));
        }
        Key::Animated => {
            ui.painter().add(egui::Shape::closed_line(diamond(c, s), Stroke::new(1.3, t.accent)));
        }
        Key::Off => {
            ui.painter().add(egui::Shape::closed_line(diamond(c, s), Stroke::new(1.2, if resp.hovered() { t.text } else { t.text_dim })));
        }
    }
    app.auto.add(id, r, tl!("Add/Remove Keyframe"));
    resp.clicked()
}

/// A reset arrow; returns whether it was clicked.
pub(crate) fn reset_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, c: Pos2, id: &str) -> bool {
    let t = app.tokens;
    let r = Rect::from_center_size(c, vec2(13.0, 13.0));
    let resp = ui.interact(r.expand(2.0), egui::Id::new(("insp-reset", id)), Sense::click()).on_hover_text(tl!("Reset"));
    icons::paint(ui.painter(), r, Icon::Reset, if resp.hovered() { t.text } else { t.text_dim });
    app.auto.add(id, r, tl!("Reset"));
    resp.clicked()
}

/// A section's on dot (lit in the accent colour while on); returns whether it was clicked.
fn dot_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, c: Pos2, id: &str, on: bool) -> bool {
    let t = app.tokens;
    let r = Rect::from_center_size(c, vec2(14.0, 14.0));
    let resp = ui.interact(r, egui::Id::new(("insp-dot", id)), Sense::click()).on_hover_text(if on { tl!("Turn Off") } else { tl!("Turn On") });
    if on {
        ui.painter().circle_filled(c, 4.5, t.accent);
    } else {
        ui.painter().circle_stroke(c, 4.0, Stroke::new(1.3, if resp.hovered() { t.text } else { t.text_faint }));
    }
    app.auto.add(id, r, if on { tl!("Turn Off") } else { tl!("Turn On") });
    resp.clicked()
}

/// Resolve's dark value boxes for the widgets of `ui`.
fn field_style(ui: &mut egui::Ui, t: &Tokens) {
    let v = &mut ui.style_mut().visuals;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.weak_bg_fill = t.field_bg;
        w.bg_fill = t.field_bg;
        w.corner_radius = 2.into();
        w.fg_stroke.color = t.text;
        w.expansion = 0.0;
    }
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, t.field_border);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.text_faint);
    v.widgets.active.bg_stroke = Stroke::new(1.0, t.accent);
    v.extreme_bg_color = t.field_bg;
    ui.spacing_mut().button_padding = vec2(4.0, 1.0);
}

/// A number field filling `rect`: drag to change, click to type. A typed value is taken on
/// Enter. With `defer_drag` a drag shows its value and is taken once, on release (for commands
/// that aren't merged into one undo step while dragging). `value` is shown as 0 when it isn't a
/// number; `range` may come in either order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn num_field(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    value: f64,
    speed: f64,
    range: (f64, f64),
    decimals: usize,
    defer_drag: bool,
) -> Option<f64> {
    let t = app.tokens;
    if !rect.is_positive() {
        return None;
    }
    let (lo, hi) = if range.0 <= range.1 { range } else { (range.1, range.0) };
    let shown = if value.is_finite() { value.clamp(lo, hi) } else { 0.0 };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("insp-field", id)).layout(egui::Layout::left_to_right(egui::Align::Center)));
    field_style(&mut child, &t);
    child.spacing_mut().interact_size = rect.size();
    let held = egui::Id::new(("insp-field-drag", id));
    let mut v: f64 = if defer_drag { child.data(|d| d.get_temp(held)).unwrap_or(shown) } else { shown };
    let resp = child.add(egui::DragValue::new(&mut v).speed(speed).range(lo..=hi).fixed_decimals(decimals).update_while_editing(false));
    app.auto.add_response(id, &resp, id);
    if defer_drag {
        if resp.dragged() {
            child.data_mut(|d| d.insert_temp(held, v));
            return None;
        }
        if resp.drag_stopped() {
            child.data_mut(|d| d.remove::<f64>(held));
            return (v != shown).then_some(v);
        }
    }
    (resp.changed() && v.is_finite()).then_some(v)
}

/// A slider filling `rect` (no value text: the field beside it shows the value).
pub(crate) fn slider(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, id: &str, value: f64, range: (f64, f64)) -> Option<f64> {
    if !rect.is_positive() || range.0.partial_cmp(&range.1) != Some(std::cmp::Ordering::Less) {
        return None;
    }
    let mut v = if value.is_finite() { value.clamp(range.0, range.1) } else { range.0 };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("insp-slider", id)).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().slider_width = rect.width();
    let resp = child.add(egui::Slider::new(&mut v, range.0..=range.1).show_value(false));
    app.auto.add_response(&format!("{id}.slider"), &resp, id);
    resp.changed().then_some(v)
}

/// A label drawn left of a pair field ("X" / "Y").
pub(crate) fn axis_label(ui: &egui::Ui, at: Pos2, text: &str, t: &Tokens) {
    ui.painter().text(at, Align2::LEFT_CENTER, text, Tokens::ui(11.0), t.text_faint);
}

/// The two halves of a value area for an X / Y pair, each with room for its axis letter on the
/// left and a gap between them (for the Zoom link).
pub(crate) fn pair_rects(value: Rect) -> (Rect, Rect) {
    let gap = 22.0;
    let half = ((value.width() - gap) / 2.0).max(0.0);
    let a = Rect::from_min_size(value.min, vec2(half, value.height()));
    let b = Rect::from_min_size(pos2(value.min.x + half + gap, value.min.y), vec2(half, value.height()));
    let field = |r: Rect| Rect::from_min_max(pos2(r.min.x + 12.0, r.min.y), r.max);
    (field(a), field(b))
}

// ---------- effect sections generated from the parameter schemas ----------

/// One effect shown as a section: `inst` is the clip's instance (index, effect), `None` while the
/// clip doesn't have the effect yet (Cropping, Stabilization): its rows then show the defaults,
/// and turning the dot on or editing a value applies it first.
pub(crate) struct Fx<'a> {
    pub sec: &'a str,
    pub title: &'a str,
    pub def: &'static EffectDef,
    pub inst: Option<(usize, &'a EffectInstance)>,
    /// Parameters shown first, in this order (the rest follow in the schema's order).
    pub order: &'a [&'a str],
    /// Parameters left out.
    pub skip: &'a [&'a str],
    /// Labels in place of the schema's (param, label).
    pub labels: &'a [(&'a str, &'a str)],
    pub removable: bool,
}

/// Queue `effects.apply` for an effect the clip doesn't have yet (once per frame).
fn ensure_applied(acts: &mut Acts, clip: ClipId, effect: &str) {
    let queued = acts.iter().any(|(c, p)| c == "effects.apply" && p["effect"] == effect && p["clips"] == json!([clip.0]));
    if !queued {
        acts.push(("effects.apply".into(), json!({"clips": [clip.0], "effect": effect})));
    }
}

pub(crate) fn fx_section(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, fx: Fx<'_>, acts: &mut Acts) {
    let e = fx.inst.map(|(_, e)| e);
    let mut params: Vec<&ParamDef> = fx.order.iter().filter_map(|id| fx.def.param(id)).collect();
    params.extend(fx.def.params.iter().filter(|p| !fx.order.contains(&p.id)));
    params.retain(|p| !fx.skip.contains(&p.id));
    let keys: Vec<(&str, Key)> = params.iter().filter(|p| p.animatable).map(|p| (p.id, key_of(e, p.id, cx.mt))).collect();
    let state: Vec<Key> = keys.iter().map(|k| k.1).collect();
    let on = e.is_some_and(|e| e.enabled);
    let head = section(app, ui, fx.sec, fx.title, Some(on), (!keys.is_empty()).then(|| key_all(&state)), fx.inst.is_some(), fx.removable && fx.inst.is_some());
    // the effect as the commands name it: by index once on the clip (two of a kind stay apart)
    let eref = fx.inst.map_or_else(|| json!(fx.def.id), |(i, _)| json!(i));
    match fx.inst {
        Some((i, _)) => {
            if head.dot {
                acts.push(("effects.toggleEnabled".into(), json!({"clip": cx.clip.0, "index": i})));
            }
            if head.reset {
                acts.push(("effects.reset".into(), json!({"clip": cx.clip.0, "index": i})));
            }
            if head.remove {
                acts.push(("effects.remove".into(), json!({"clip": cx.clip.0, "index": i})));
            }
        }
        None if head.dot => ensure_applied(acts, cx.clip, fx.def.id),
        None => {}
    }
    if head.key {
        if fx.inst.is_none() {
            ensure_applied(acts, cx.clip, fx.def.id);
        }
        section_keyframes(acts, cx.clip, eref.clone(), &keys);
    }
    if !head.open {
        return;
    }
    for pd in params {
        let label = fx.labels.iter().find(|(p, _)| *p == pd.id).map_or(pd.label, |(_, l)| *l);
        param_row(app, ui, cx, &fx, pd, crate::i18n::t(label), &eref, acts);
    }
    // Corner Pin's Track Surface buttons (fork Y3.3c), Effect Controls' row
    if let Some((idx, _)) = fx.inst.filter(|_| fx.def.id == "corner_pin") {
        let (body, lane) = ec_row_rects(ui);
        crate::panels::effect_controls::corner_pin_track_row(app, ui, body, &lane, cx.clip, idx, acts);
    }
}

/// The body and the (zero-width, so absent) keyframe lane for one of Effect Controls' rows
/// reused here.
pub(crate) fn ec_row_rects(ui: &egui::Ui) -> (Rect, Rect) {
    let body = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ROW_H));
    (body, Rect::from_min_max(pos2(body.max.x, body.min.y), pos2(body.max.x, body.min.y)))
}

/// One parameter row of an [`Fx`] section, by the parameter's kind: a slider and a field for a
/// number, a field for an angle, two fields for a point, a check box, a list, a colour. Other
/// kinds (curves, text, paths) use Effect Controls' row.
#[allow(clippy::too_many_arguments)]
fn param_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, fx: &Fx<'_>, pd: &ParamDef, label: &str, eref: &Value, acts: &mut Acts) {
    let t = app.tokens;
    let e = fx.inst.map(|(_, e)| e);
    let value = e.and_then(|e| e.param(pd.id)).map_or_else(|| pd.default.clone(), |p| p.value_at(cx.mt));
    let id = format!("inspector.{}.{}", fx.sec, pd.id);
    let simple = matches!(
        (&pd.kind, &value),
        (ParamKind::Float { .. } | ParamKind::Angle, ParamValue::Float(_))
            | (ParamKind::Point, ParamValue::Vec2(_))
            | (ParamKind::Bool, ParamValue::Bool(_))
            | (ParamKind::Choice(_), ParamValue::Choice(_))
            | (ParamKind::Color, ParamValue::Color(_))
    );
    if !simple {
        // Effect Controls' own row (no keyframe lane: a zero-width one at the right edge)
        if let Some((idx, inst)) = fx.inst {
            let (body, lane) = ec_row_rects(ui);
            let it = cx.it.clone();
            crate::panels::effect_controls::param_row(app, ui, body, cx.clip, idx, inst, None, pd, cx.mt, acts, &lane, &|_| lane.max.x, &it);
        }
        return;
    }
    let row = row(ui, label, &t);
    let mut set: Option<Value> = None;
    match (&pd.kind, &value) {
        (ParamKind::Float { min, max, soft_min, soft_max, decimals, .. }, ParamValue::Float(v)) => {
            let soft = soft_min < soft_max;
            let speed = if soft { ((soft_max - soft_min) / 300.0).max(0.001) } else { 0.1 };
            let (sr, fr) = if soft && row.value.width() > 150.0 {
                let fw = 64.0;
                (
                    Some(Rect::from_min_max(row.value.min, pos2(row.value.max.x - fw - 8.0, row.value.max.y))),
                    Rect::from_min_max(pos2(row.value.max.x - fw, row.value.min.y), row.value.max),
                )
            } else {
                (None, row.value)
            };
            if let Some(sr) = sr
                && let Some(nv) = slider(app, ui, sr, &id, *v, (*soft_min, *soft_max))
            {
                set = Some(json!(nv));
            }
            if let Some(nv) = num_field(app, ui, fr, &id, *v, speed, (*min, *max), *decimals as usize, false) {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Angle, ParamValue::Float(v)) => {
            let fr = Rect::from_min_size(row.value.min, vec2(row.value.width().min(90.0), row.value.height()));
            if let Some(nv) = num_field(app, ui, fr, &id, *v, 0.5, (-36000.0, 36000.0), 1, false) {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Point, ParamValue::Vec2(p)) => {
            let (a, b) = pair_rects(row.value);
            axis_label(ui, pos2(a.min.x - 11.0, a.center().y), "X", &t);
            axis_label(ui, pos2(b.min.x - 11.0, b.center().y), "Y", &t);
            let nx = num_field(app, ui, a, &format!("{id}X"), p.x, 1.0, (-100_000.0, 100_000.0), 1, false);
            let ny = num_field(app, ui, b, &format!("{id}Y"), p.y, 1.0, (-100_000.0, 100_000.0), 1, false);
            if nx.is_some() || ny.is_some() {
                set = Some(json!([nx.unwrap_or(p.x), ny.unwrap_or(p.y)]));
            }
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut v = *b;
            let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(row.value).layout(egui::Layout::left_to_right(egui::Align::Center)));
            crate::panels::effect_controls::fit_to_row(&mut vui);
            let r = vui.checkbox(&mut v, "");
            app.auto.add_response(&id, &r, label);
            if r.changed() {
                set = Some(json!(v));
            }
        }
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            let mut sel = *c as usize;
            let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(row.value).layout(egui::Layout::left_to_right(egui::Align::Center)));
            field_style(&mut vui, &t);
            crate::panels::effect_controls::fit_to_row(&mut vui);
            let r = egui::ComboBox::from_id_salt(("insp-choice", &id))
                .selected_text(opts.get(sel).map_or("", |o| crate::i18n::t(o)))
                .width(row.value.width())
                .show_ui(&mut vui, |ui| {
                    for (i, o) in opts.iter().enumerate() {
                        if ui.selectable_value(&mut sel, i, crate::i18n::t(o)).changed() {
                            set = Some(json!(i));
                        }
                    }
                });
            app.auto.add(&id, r.response.rect, label);
        }
        (ParamKind::Color, ParamValue::Color(c)) => {
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(row.value).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let r = egui::color_picker::color_edit_button_rgba(&mut vui, &mut rgba, egui::color_picker::Alpha::Opaque);
            app.auto.add(&id, r.rect, label);
            if r.changed() {
                set = Some(json!([rgba.r(), rgba.g(), rgba.b(), rgba.a()]));
            }
        }
        _ => {}
    }
    if let Some(v) = set {
        if fx.inst.is_none() {
            ensure_applied(acts, cx.clip, fx.def.id);
        }
        acts.push(("effects.setParam".into(), json!({"clip": cx.clip.0, "effect": eref, "param": pd.id, "value": v})));
    }
    if pd.animatable && keyframe_button(app, ui, row.key, &format!("{id}.keyframe"), key_of(e, pd.id, cx.mt)) {
        if fx.inst.is_none() {
            ensure_applied(acts, cx.clip, fx.def.id);
        }
        acts.push(("effects.addKeyframe".into(), json!({"clip": cx.clip.0, "effect": eref, "param": pd.id})));
    }
    if fx.inst.is_some() && reset_button(app, ui, row.reset, &format!("{id}.reset")) {
        acts.push(("effects.resetParam".into(), json!({"clip": cx.clip.0, "effect": eref, "param": pd.id})));
    }
}

/// The Audio tab: every audio effect on the clip, Volume and Pan first as the clip carries them.
fn audio_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, acts: &mut Acts) {
    let it = cx.it.clone();
    let mut seen: Vec<&str> = Vec::new();
    for (i, e) in it.effects.iter().enumerate() {
        let Some(def) = e.def() else { continue };
        // a second instance of an effect gets its index in its section id
        let sec = if seen.contains(&e.effect.as_str()) { format!("{}{i}", e.effect) } else { e.effect.clone() };
        seen.push(&e.effect);
        let title = match e.effect.as_str() {
            "volume" => tl!("Volume"),
            "panner" => tl!("Pan"),
            _ => crate::i18n::t(def.name),
        };
        let labels: &[(&str, &str)] = match e.effect.as_str() {
            "volume" => &[("level", "Volume")],
            "panner" => &[("balance", "Pan")],
            _ => &[],
        };
        let fx = Fx { sec: &sec, title, def, inst: Some((i, e)), order: &[], skip: &[], labels, removable: !def.intrinsic };
        fx_section(app, ui, cx, fx, acts);
    }
}

/// Effects the Video tab shows in its own sections (not repeated on the Effects tab).
pub(crate) const VIDEO_TAB_EFFECTS: [&str; 4] = ["crop", "horizontal_flip", "vertical_flip", "warp_stabilizer"];

/// The Effects tab: the clip's other video effects, each a section.
fn effects_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &Cx, acts: &mut Acts) {
    let t = app.tokens;
    let it = cx.it.clone();
    let mut seen: Vec<&str> = Vec::new();
    let mut any = false;
    for (i, e) in it.effects.iter().enumerate() {
        let Some(def) = e.def() else { continue };
        if def.intrinsic || VIDEO_TAB_EFFECTS.contains(&e.effect.as_str()) {
            continue;
        }
        any = true;
        let sec = if seen.contains(&e.effect.as_str()) { format!("{}{i}", e.effect) } else { e.effect.clone() };
        seen.push(&e.effect);
        let fx = Fx { sec: &sec, title: crate::i18n::t(def.name), def, inst: Some((i, e)), order: &[], skip: &[], labels: &[], removable: true };
        fx_section(app, ui, cx, fx, acts);
    }
    if !any {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("No effects applied"), Tokens::ui(12.0), t.text_faint);
    }
}
