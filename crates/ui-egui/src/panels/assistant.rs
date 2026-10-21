//! The Assistant panel (fork AI3, FORK-PLAN §1a.78): a chat with the local assistant model,
//! answered from the fork's guide (`coach.assist` / `coach.assistResult` / `coach.assistForget`).
//!
//! The Assistant button in the page bar / header ([`crate::panels::coach::bar_buttons`]) opens it
//! floating at the bottom right ([`toggle`]); like any panel it can then be dragged anywhere, or
//! docked by its tab between or beside other panels (the layout is saved with the workspace).
//!
//! Each question follows up on the last answered one (`previousQuestion` / `previousAnswer`) until
//! New chat. Lines whose control FilmCraft knows are clickable and outline that control (the same
//! spotlight as Ask the coach); when the model can't answer, the keyword coach's checked answer
//! is shown with the reason.
//!
//! Automation ids: `assistant.input`, `assistant.send`, `assistant.stop`, `assistant.new`,
//! `assistant.turn.{n}` (1-based; the question), `assistant.line.{n}.{m}` (answer `n`, line `m`),
//! `assistant.lesson.{n}` (Show me), `assistant.rate.{n}.*` (rating and sharing,
//! [`super::share_example`]).

use egui::{CornerRadius, Frame, Margin, Rect, RichText, vec2};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::state::ChatTurn;

/// Questions kept in the chat (older ones drop off the top).
pub const MAX_TURNS: usize = 40;
/// Longest question (chars), as in the Ask box.
const MAX_QUESTION: usize = 600;

/// The panel is in the layout (docked or floating).
pub fn is_open(app: &FilmcraftApp) -> bool {
    app.ui.dock.contains(PanelKind::Assistant) || app.ui.floating.iter().any(|g| g.panels.contains(&PanelKind::Assistant))
}

/// The Assistant button: close the panel when it's open, else open it floating at the bottom right
/// (a tab beside the first docked panel if it can't float).
pub fn toggle(app: &mut FilmcraftApp, ctx: &egui::Context) {
    use craft_ui::docking::Action;
    let panel = PanelKind::Assistant;
    if is_open(app) {
        if let Err(e) = app.apply_dock(Action::Close { panel }) {
            app.ui.status = e;
        }
        forget_running(app);
        return;
    }
    // (floating needs the panel in the layout: it joins a shown tab group for a moment, and that
    // group's tab is shown again after)
    let mut docked = Vec::new();
    app.ui.dock.panels(&mut docked);
    let Some(anchor) = docked.iter().copied().find(|p| app.ui.dock.is_visible(*p)).or_else(|| docked.first().copied()) else {
        app.ui.status = crate::i18n::t("There's no panel to open the Assistant beside.").to_string();
        return;
    };
    if let Err(e) = app.apply_dock(Action::Open { panel, anchor: Some(anchor) }) {
        app.ui.status = e;
        return;
    }
    let s = ctx.content_rect();
    let w = 400.0_f32.min(s.width() - 32.0).max(220.0);
    let h = 520.0_f32.min(s.height() - 140.0).max(220.0);
    // above the page bar / status bar, left of the column the coach's cards and Ask box open in
    // (bottom right, `coach::WIDTH` wide) when the window has room
    let right = if s.width() >= w + crate::panels::coach::WIDTH + 64.0 { s.max.x - crate::panels::coach::WIDTH - 32.0 } else { s.max.x - 16.0 };
    let rect = [right - w, s.max.y - h - 96.0, w, h];
    if let Err(e) = app.apply_dock(Action::Float { panel, rect }) {
        app.ui.status = e;
        return;
    }
    app.ui.dock.activate(anchor);
}

/// Stop the answer being written, if any (the panel closed, or New chat).
fn forget_running(app: &mut FilmcraftApp) {
    if let Some(id) = app.ui.assistant.running.take() {
        // an id that is already gone is fine: nothing is running for it
        let _ = app.session.execute("coach.assistForget", json!({ "id": id }));
    }
}

/// Send the typed question (a follow-up on the last answered one).
fn send(app: &mut FilmcraftApp) {
    let q: String = app.ui.assistant.input.trim().chars().take(MAX_QUESTION).collect();
    if q.is_empty() {
        return;
    }
    forget_running(app);
    let mut p = json!({ "question": q, "page": app.ui.workspace, "context": "auto" });
    if let Some((pq, pa)) = last_exchange(&app.ui.assistant.turns) {
        p["previousQuestion"] = json!(pq);
        p["previousAnswer"] = json!(pa);
    }
    let mut turn = ChatTurn { question: q, result: None };
    match app.session.execute("coach.assist", p) {
        Ok(v) => app.ui.assistant.running = v["id"].as_u64(),
        Err(e) => turn.result = Some(json!({ "state": "error", "error": e.to_string() })),
    }
    let chat = &mut app.ui.assistant;
    chat.turns.push(turn);
    if chat.turns.len() > MAX_TURNS {
        let extra = chat.turns.len() - MAX_TURNS;
        chat.turns.drain(..extra);
    }
    chat.input.clear();
}

/// The last question the model answered and its checked lines (what a follow-up refers to).
fn last_exchange(turns: &[ChatTurn]) -> Option<(String, String)> {
    let t = turns.iter().rev().find(|t| t.result.as_ref().is_some_and(|r| r["state"] == "done" && r["source"] == "model"))?;
    let r = t.result.as_ref()?;
    let lines: Vec<&str> = r["lines"].as_array().map(|l| l.iter().filter_map(|x| x["text"].as_str()).collect()).unwrap_or_default();
    Some((t.question.clone(), lines.join("\n")))
}

/// Read the running answer (`coach.assistResult`, no waiting); repaint while it's written.
fn poll(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(id) = app.ui.assistant.running else { return };
    let result = app.session.execute("coach.assistResult", json!({ "id": id }));
    let chat = &mut app.ui.assistant;
    match result {
        Ok(v) => {
            if v["state"] == "pending" {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            } else {
                // done, or forgotten elsewhere (control channel): stop polling
                chat.running = None;
            }
            if v["state"] != "unknown"
                && let Some(t) = chat.turns.last_mut()
            {
                // the id goes with the answer: rating and sharing name it (AI5)
                let mut v = v;
                v["id"] = json!(id);
                t.result = Some(v);
            }
        }
        Err(e) => {
            chat.running = None;
            if let Some(t) = chat.turns.last_mut() {
                t.result = Some(json!({ "state": "error", "error": e.to_string() }));
            }
        }
    }
}

/// What a click in the panel asks for (done after drawing).
enum Click {
    Send,
    Stop,
    New,
    Spotlight(String),
    Lesson(String),
    /// Settings ▸ Color, where the model is downloaded (`assistant.setup.<n>`).
    Setup,
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    poll(app, ui.ctx());
    let t = app.tokens;
    let mut click = None;
    let busy = app.ui.assistant.running.is_some();
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut ui;
    // the question box at the bottom, the chat above it
    let input_h = 64.0;
    let chat_h = (ui.available_height() - input_h).max(40.0);
    // New chat on the right; the note takes what's left (cut short in a narrow panel)
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let r = ui.add_enabled(!app.ui.assistant.turns.is_empty(), egui::Button::new(RichText::new(tl!("New chat")).small()));
        let r = r.on_hover_text(tl!("Clear the chat; the next question starts a new topic."));
        app.auto.add_response("assistant.new", &r, "New chat");
        if r.clicked() {
            click = Some(Click::New);
        }
        let note = tl!("Answers come from Revolution's guide and run on this computer.");
        ui.add(egui::Label::new(RichText::new(note).small().color(t.text_faint)).truncate()).on_hover_text(note);
    });
    egui::ScrollArea::vertical().id_salt("assistant-chat").max_height(chat_h - 22.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
        if app.ui.assistant.turns.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(tl!("Ask how to do something in Revolution: grading, editing, audio, compositing, export…")).color(t.text_dim));
            ui.label(RichText::new(tl!("e.g. how do I make the sky bluer without changing skin?")).small().italics().color(t.text_faint));
        }
        let turns = app.ui.assistant.turns.clone();
        for (i, turn) in turns.iter().enumerate() {
            let n = i + 1;
            ui.add_space(6.0);
            // the question, as a bubble on the right
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let r = Frame::new()
                    .fill(t.hover)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.set_max_width((ui.available_width() * 0.85).max(80.0));
                        ui.add(egui::Label::new(RichText::new(&turn.question).color(t.text)).wrap());
                    })
                    .response;
                app.auto.add_response(&format!("assistant.turn.{n}"), &r, &turn.question);
            });
            ui.add_space(4.0);
            answer(app, ui, n, turn.result.as_ref(), &mut click);
        }
    });
    ui.add_space(4.0);
    ui.separator();
    ui.horizontal(|ui| {
        let send_w = 64.0;
        let edit = egui::TextEdit::multiline(&mut app.ui.assistant.input)
            .hint_text(tl!("Ask the assistant…"))
            .char_limit(MAX_QUESTION)
            .desired_rows(2)
            .desired_width((ui.available_width() - send_w - 8.0).max(60.0));
        let r = ui.add(edit);
        app.auto.add_response("assistant.input", &r, "Ask the assistant");
        // Enter sends, Shift+Enter is a new line
        let enter = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        if enter {
            // drop the newline the key typed
            let trimmed = app.ui.assistant.input.trim_end_matches(['\n', '\r']).to_string();
            app.ui.assistant.input = trimmed;
        }
        ui.vertical(|ui| {
            if busy {
                let r = ui.add_sized(vec2(send_w, 24.0), egui::Button::new(tl!("Stop")));
                app.auto.add_response("assistant.stop", &r, "Stop");
                if r.clicked() {
                    click = Some(Click::Stop);
                }
            } else {
                let ok = !app.ui.assistant.input.trim().is_empty();
                let r = ui.add_enabled(ok, egui::Button::new(tl!("Send")).min_size(vec2(send_w, 24.0)));
                app.auto.add_response("assistant.send", &r, "Send");
                if (r.clicked() || enter) && ok {
                    click = Some(Click::Send);
                }
            }
        });
    });
    match click {
        Some(Click::Send) => send(app),
        Some(Click::Stop) => {
            forget_running(app);
            if let Some(t) = app.ui.assistant.turns.last_mut()
                && t.result.as_ref().is_none_or(|r| r["state"] == "pending")
            {
                t.result = Some(json!({ "state": "stopped" }));
            }
        }
        Some(Click::New) => {
            forget_running(app);
            app.ui.assistant.turns.clear();
        }
        Some(Click::Spotlight(id)) => {
            let same = app.ui.coach_ask.spotlight.as_deref() == Some(id.as_str());
            app.ui.coach_ask.spotlight = (!same).then_some(id);
            app.ui.coach_ask.clicks = app.ui.coach_ask.clicks.wrapping_add(1);
        }
        Some(Click::Setup) => {
            if let Err(e) = super::settings::open(app, "color") {
                app.ui.status = e;
            }
        }
        Some(Click::Lesson(id)) => {
            app.ui.coach_ask.spotlight = None;
            if let Err(e) = app.session.execute("coach.start", json!({ "lesson": id })) {
                app.ui.status = e.to_string();
            }
        }
        None => {}
    }
}

/// Answer `n`: the text while it's written, then the checked lines (clickable when they name a
/// control), sources and Show me; or why the guide's own answer is shown instead.
fn answer(app: &mut FilmcraftApp, ui: &mut egui::Ui, n: usize, result: Option<&Value>, click: &mut Option<Click>) {
    let t = app.tokens;
    let Some(a) = result else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(tl!("The assistant is reading the guide…")).small().color(t.text_dim));
        });
        return;
    };
    match a["state"].as_str() {
        Some("pending") => {
            let partial = a["partial"].as_str().unwrap_or("");
            if partial.trim().is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tl!("The assistant is reading the guide…")).small().color(t.text_dim));
                });
            } else {
                ui.add(egui::Label::new(RichText::new(partial).color(t.text_dim)).wrap());
                ui.label(RichText::new(tl!("Not checked against the guide yet.")).small().italics().color(t.text_faint));
            }
            return;
        }
        Some("stopped") => {
            ui.label(RichText::new(tl!("Stopped.")).small().italics().color(t.text_faint));
            return;
        }
        Some("error") => {
            ui.label(RichText::new(a["error"].as_str().unwrap_or("")).small().color(t.text_dim));
            return;
        }
        _ => {}
    }
    super::coach::seen_lines(app, ui, a, &format!("assistant.seen.{n}"));
    super::coach::reference_block(app, ui, a, &format!("assistant.reference.{n}"), &format!("assistant.doThis.{n}"));
    if a["source"] != "model" {
        let why = match a["fallback"]["kind"].as_str() {
            Some("notReady") => tl!("The assistant can't run here yet (its model isn't installed or this build has none). The guide's answer:"),
            Some("ungrounded") => tl!("The assistant's answer named things Revolution doesn't have, so it was left out. The guide's answer:"),
            Some("nothingFound") => tl!("The guide has nothing on this, so the assistant didn't guess."),
            _ => tl!("The assistant couldn't answer this time. The guide's answer:"),
        };
        ui.label(RichText::new(why).small().color(t.text_dim));
        if a["fallback"]["kind"] == "notReady" {
            let r = ui.link(tl!("Set up the assistant's model (Settings ▸ Color)"));
            app.auto.add_response(&format!("assistant.setup.{n}"), &r, "Set up the assistant's model");
            if r.clicked() {
                *click = Some(Click::Setup);
            }
        }
        guide_steps(app, ui, n, &a["coach"], click);
        rate(app, ui, n, a);
        return;
    }
    if a["dontKnow"].as_bool() == Some(true) {
        ui.label(RichText::new(tl!("The assistant says the guide doesn't cover this; the guide's closest answer:")).small().color(t.text_dim));
        guide_steps(app, ui, n, &a["coach"], click);
    }
    let lines = a["lines"].as_array().cloned().unwrap_or_default();
    for (m, l) in lines.iter().enumerate() {
        let text = l["text"].as_str().unwrap_or("");
        let control = l["ui"].as_array().and_then(|u| u.first()).and_then(Value::as_str).map(str::to_string);
        // "General advice:" lines aren't from the guide: dimmer
        let lit = control.is_some() && app.ui.coach_ask.spotlight == control;
        let color = if lit {
            t.accent
        } else if l["general"].as_bool() == Some(true) {
            t.text_dim
        } else {
            t.text
        };
        let r = ui.add(egui::Label::new(RichText::new(text).color(color)).wrap().sense(egui::Sense::click()));
        let r = if control.is_some() { r.on_hover_text(tl!("Click to highlight this control.")).on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
        app.auto.add_response(&format!("assistant.line.{n}.{}", m + 1), &r, text);
        if r.clicked()
            && let Some(id) = control
        {
            *click = Some(Click::Spotlight(id));
        }
    }
    let cited: Vec<&str> = a["cited"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let sources: Vec<&str> = a["sources"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let shown = if cited.is_empty() { &sources } else { &cited };
    if !shown.is_empty() {
        ui.label(RichText::new(tlf!("Sources: {list}", list = shown.join(", "))).small().color(t.text_faint));
    }
    lesson_link(app, ui, n, &a["lesson"], click);
    let removed = a["removedLines"].as_u64().unwrap_or(0);
    if removed > 0 {
        ui.label(
            RichText::new(tlf!("{n} lines left out: they named controls, commands or shortcuts the guide doesn't have.", n = removed))
                .small()
                .italics()
                .color(t.text_faint),
        );
    }
    rate(app, ui, n, a);
}

/// 👍 / 👎 and Help improve the Assistant… under a finished answer (fork AI5).
fn rate(app: &mut FilmcraftApp, ui: &mut egui::Ui, n: usize, a: &Value) {
    if let Some(id) = a["id"].as_u64() {
        let (up, down) = (format!("assistant.rate.{n}.up"), format!("assistant.rate.{n}.down"));
        let (correction, share) = (format!("assistant.rate.{n}.correction"), format!("assistant.rate.{n}.share"));
        let ids = super::share_example::RowIds { up: &up, down: &down, correction: &correction, share: &share };
        super::share_example::rate_row(app, ui, id, &ids);
    }
}

/// The keyword coach's checked steps (`coach.ask`'s answer shape), clickable like the Ask box's.
fn guide_steps(app: &mut FilmcraftApp, ui: &mut egui::Ui, n: usize, coach: &Value, click: &mut Option<Click>) {
    let t = app.tokens;
    let steps = coach["steps"].as_array().cloned().unwrap_or_default();
    for (m, st) in steps.iter().enumerate() {
        let text = st["text"].as_str().unwrap_or("");
        let control = st["ui"].as_str().map(str::to_string);
        let lit = control.is_some() && app.ui.coach_ask.spotlight == control;
        let r =
            ui.add(egui::Label::new(RichText::new(format!("{}. {text}", m + 1)).color(if lit { t.accent } else { t.text })).wrap().sense(egui::Sense::click()));
        let r = if control.is_some() { r.on_hover_text(tl!("Click to highlight this control.")).on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
        app.auto.add_response(&format!("assistant.step.{n}.{}", m + 1), &r, text);
        if r.clicked()
            && let Some(id) = control
        {
            *click = Some(Click::Spotlight(id));
        }
    }
    if steps.is_empty() {
        // never an empty "The guide's answer:": its summary, else say there is none
        let none = tl!("The guide doesn't cover this yet.");
        let text = coach["summary"].as_str().filter(|s| !s.is_empty()).unwrap_or(none);
        let r = ui.label(RichText::new(text).small().color(t.text_dim));
        app.auto.add_response(&format!("assistant.noSteps.{n}"), &r, text);
    }
    lesson_link(app, ui, n, &coach["lesson"], click);
}

/// "Show me": the lesson for answer `n`, if it has one.
fn lesson_link(app: &mut FilmcraftApp, ui: &mut egui::Ui, n: usize, lesson: &Value, click: &mut Option<Click>) {
    if let (Some(id), Some(title)) = (lesson["id"].as_str(), lesson["title"].as_str()) {
        let r = ui.link(tlf!("Show me: {title}", title = title));
        app.auto.add_response(&format!("assistant.lesson.{n}"), &r, title);
        if r.clicked() {
            *click = Some(Click::Lesson(id.to_string()));
        }
    }
}
