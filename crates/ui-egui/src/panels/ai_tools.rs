//! AI Tools panel (Window ▸ AI Tools): each AI tool with whether its model is in memory and its
//! work running, a Stop button per tool and Stop All (engine `ai.status` / `ai.stop`).
//!
//! Automation ids: `aiTools.row.<tool>`, `aiTools.stop.<tool>`, `aiTools.stopAll`.

use egui::{Align2, Rect, RichText, Sense, pos2, vec2};
use filmcraft_engine::ai_tools::{self, ToolStatus};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

fn tool_label(id: &str) -> &'static str {
    match id {
        "assistant" => tl!("Assistant and AI explanations"),
        "vision" => tl!("Assistant picture model"),
        "magicMask" => tl!("Magic Mask"),
        "voice" => tl!("Voice clean-up and rendered Voice Isolation"),
        "narration" => tl!("Text to Speech voices"),
        _ => "",
    }
}

fn state_line(st: &ToolStatus) -> String {
    let model = match (&st.model, st.loaded) {
        (Some(m), true) => tlf!("Loaded: {model}", model = m),
        (None, true) => tl!("Model loaded").to_string(),
        (_, false) => tl!("Not loaded").to_string(),
    };
    if st.running > 0 { format!("{model} · {}", tlf!("{n} running", n = st.running)) } else { model }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let rows = ai_tools::status(&app.session);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))));
    let ui = &mut child;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut stop: Option<Option<&'static str>> = None;
    ui.label(
        RichText::new(tl!(
            "AI tools keep their models in memory after use. Stop one to end its work and free its memory; it loads again the next time you use it."
        ))
        .color(t.text_dim)
        .size(11.0),
    );
    ui.add_space(6.0);
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for st in &rows {
            let label = tool_label(st.id);
            let busy = st.loaded || st.running > 0;
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            ui.painter().rect_filled(r.shrink(1.0), 3.0, t.row_alt);
            ui.painter().text(r.min + vec2(8.0, 5.0), Align2::LEFT_TOP, label, Tokens::semibold(12.0), t.text);
            let line = state_line(st);
            ui.painter().text(pos2(r.min.x + 8.0, r.max.y - 5.0), Align2::LEFT_BOTTOM, &line, Tokens::ui(10.5), if busy { t.accent } else { t.text_faint });
            elems.push((format!("aiTools.row.{}", st.id), r, format!("{label}: {line}")));
            let br = Rect::from_min_size(pos2(r.max.x - 70.0, r.center().y - 11.0), vec2(62.0, 22.0));
            let resp = ui.put(br, egui::Button::new(tl!("Stop")).sense(if busy { Sense::click() } else { Sense::hover() }));
            elems.push((format!("aiTools.stop.{}", st.id), br, format!("Stop {label}")));
            if busy && resp.on_hover_text(tl!("Stop its work and free its memory")).clicked() {
                stop = Some(Some(st.id));
            }
        }
        ui.add_space(8.0);
        let any = rows.iter().any(|st| st.loaded || st.running > 0);
        let r = ui.add_enabled(any, egui::Button::new(tl!("Stop All and Free Memory")));
        elems.push(("aiTools.stopAll".into(), r.rect, "Stop All and Free Memory".into()));
        if r.clicked() {
            stop = Some(None);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(tl!("Voice Isolation's live effect model (8 MB) stays loaded: effects on clips use it while you play."))
                .color(t.text_faint)
                .size(10.5),
        );
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some(tool) = stop {
        let p = tool.map_or(json!({}), |t| json!({"tool": t}));
        match app.session.execute("ai.stop", p) {
            Ok(_) => app.ui.status = tl!("AI stopped and its memory freed").to_string(),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    // models load and jobs end on other threads
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
}
