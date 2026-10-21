//! A3.3: a selected audio track (click its header in the Timeline) in Effect Controls / the
//! Inspector: its track effects (mixer inserts) with the same kind of controls a clip's effects
//! get: Bypass, Pre / Post-Fader, Edit… for the effects with a graphical editor, Remove, and each
//! parameter. Every change is one `mixer.setInsert` / `mixer.removeInsert`; dragging a value
//! commits once on release (one undo step). An audio effect dropped on the view goes in the
//! track's first free slot (`mixer.addInsert`), as on the track header or a mixer strip.

use egui::{Rect, RichText, Stroke, StrokeKind, vec2};
use filmcraft_project::{ParamKind, ParamValue, TrackId, TrackKind};
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// The selected audio track, when nothing on the timeline (clip, transition) is selected instead.
pub fn selected(app: &FilmcraftApp) -> Option<TrackId> {
    let id = app.ui.selected_track?;
    let seq = app.session.active_sequence()?;
    seq.audio_tracks.iter().find(|t| t.id.0 == id).map(|t| t.id)
}

pub fn effect_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, id: TrackId) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence() else { return };
    let Some(tr) = seq.track(id).filter(|tr| tr.kind == TrackKind::Audio).cloned() else { return };
    let label = filmcraft_engine::mixer::strip_label(seq, id);
    let mut acts: Vec<(String, Value)> = Vec::new();
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut open_editor = None;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))).id_salt(("track-ec", id.0)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let head = child.label(RichText::new(tlf!("Track · {label} · {name}", label = label, name = tr.name)).color(t.text_dim));
    elems.push(("effectControls.track.title".into(), head.rect, label.clone()));
    child.add_space(4.0);
    egui::ScrollArea::vertical().id_salt(("track-ec-scroll", id.0)).auto_shrink([false, false]).show(&mut child, |ui| {
        if tr.effects.is_empty() {
            ui.label(
                RichText::new(tl!("No track effects. Drag an audio effect from the Effects panel here, onto the track header or onto its mixer strip."))
                    .color(t.text_dim),
            );
        }
        for (k, e) in tr.effects.iter().enumerate() {
            let p = format!("effectControls.track.fx.{k}");
            let def = e.def();
            let name = def.map_or(e.effect.as_str(), |d| d.name).to_string();
            ui.horizontal(|ui| {
                let r = ui.strong(&name);
                elems.push((p.clone(), r.rect, name.clone()));
                let mut on = e.enabled;
                let r = ui.checkbox(&mut on, tl!("On"));
                elems.push((format!("{p}.enabled"), r.rect, if on { "On".into() } else { "Bypassed".into() }));
                if r.changed() {
                    acts.push(("mixer.setInsert".into(), json!({"strip": id.0, "slot": k, "enabled": on})));
                }
                let mut post = e.post_fader;
                let r = ui.checkbox(&mut post, tl!("Post-Fader"));
                elems.push((format!("{p}.postFader"), r.rect, "Post-Fader".into()));
                if r.changed() {
                    acts.push(("mixer.setInsert".into(), json!({"strip": id.0, "slot": k, "postFader": post})));
                }
                if crate::panels::audio_fx_editor::has_editor(&e.effect) {
                    let r = ui.button(tl!("Edit…"));
                    elems.push((format!("{p}.edit"), r.rect, "Edit…".into()));
                    if r.clicked() {
                        open_editor = Some(crate::panels::audio_fx_editor::FxTarget::Insert { strip: id.0, slot: k });
                    }
                }
                let r = ui.button(tl!("Remove"));
                elems.push((format!("{p}.remove"), r.rect, "Remove".into()));
                if r.clicked() {
                    acts.push(("mixer.removeInsert".into(), json!({"strip": id.0, "slot": k})));
                }
            });
            let Some(def) = def else { continue };
            egui::Grid::new(("track-ec-grid", id.0, k)).num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                for pd in &def.params {
                    let cur = e.param(pd.id).map_or(&pd.default, |x| &x.value);
                    let eid = format!("{p}.{}", pd.id);
                    let commit = |v: Value| ("mixer.setInsert".to_string(), json!({"strip": id.0, "slot": k, "params": {pd.id: v}}));
                    match (&pd.kind, cur) {
                        (ParamKind::Float { min, max, soft_min, soft_max, unit, decimals }, v) => {
                            ui.label(pd.label);
                            let saved = v.as_f64().unwrap_or(0.0);
                            // the value while dragging lives here; the command goes out once, on release
                            let draft_id = egui::Id::new(("track-ec-draft", id.0, k, pd.id));
                            let mut x = ui.data(|d| d.get_temp::<f64>(draft_id)).unwrap_or(saved);
                            let span = (soft_max - soft_min).abs();
                            let speed = if span.is_finite() && span > 0.0 { span / 300.0 } else { 0.1 };
                            let (lo, hi) = if min <= max { (*min, *max) } else { (*max, *min) };
                            let r = ui
                                .add(egui::DragValue::new(&mut x).range(lo..=hi).speed(speed).max_decimals(usize::from(*decimals)).suffix(format!(" {unit}")));
                            elems.push((eid, r.rect, format!("{x:.*}", usize::from(*decimals))));
                            if r.dragged() {
                                ui.data_mut(|d| d.insert_temp(draft_id, x));
                            }
                            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                                ui.data_mut(|d| d.remove::<f64>(draft_id));
                                if x.is_finite() && x != saved {
                                    acts.push(commit(json!(x)));
                                }
                            }
                            ui.end_row();
                        }
                        (ParamKind::Bool, ParamValue::Bool(b)) => {
                            ui.label(pd.label);
                            let mut b = *b;
                            let r = ui.checkbox(&mut b, "");
                            elems.push((eid, r.rect, b.to_string()));
                            if r.changed() {
                                acts.push(commit(json!(b)));
                            }
                            ui.end_row();
                        }
                        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
                            ui.label(pd.label);
                            let mut c = *c as usize;
                            let before = c;
                            let shown = opts.get(c).copied().unwrap_or("");
                            let r = egui::ComboBox::from_id_salt((&eid, "combo")).selected_text(shown).show_ui(ui, |ui| {
                                for (i, o) in opts.iter().enumerate() {
                                    ui.selectable_value(&mut c, i, *o);
                                }
                            });
                            elems.push((eid, r.response.rect, shown.to_string()));
                            if c != before {
                                acts.push(commit(json!(c)));
                            }
                            ui.end_row();
                        }
                        _ => {}
                    }
                }
            });
            ui.add_space(6.0);
            let y = ui.cursor().top() - 3.0;
            ui.painter().line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], Stroke::new(1.0, t.separator));
        }
    });
    // an audio effect dropped here goes in the first free slot
    let target = rect.intersect(ui.clip_rect());
    if let Some(effect) = crate::panels::mixer::dragged_insert(ui)
        && ui.ctx().pointer_hover_pos().is_some_and(|q| target.contains(q))
    {
        ui.painter().rect_stroke(target.shrink(1.0), 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
        elems.push(("effectControls.track.fxDrop".into(), target, "Drop to add a track effect".into()));
        if ui.ctx().input(|i| i.pointer.any_released()) {
            acts.push(("mixer.addInsert".into(), json!({"strip": id.0, "effect": effect})));
            crate::panels::clear_drag(ui);
        }
    }
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    if let Some(target) = open_editor {
        crate::panels::audio_fx_editor::open(app, target);
    }
    for (cmd, params) in acts {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
}
