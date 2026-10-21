//! The Grading Coach card (fork GRADE-PLAN G2.12): the current lesson step in a small card at the
//! bottom right, and a spotlight outline around the control the step is about.
//!
//! All lesson state lives in the engine (`coach.*` commands, `Session::coach`); this only draws
//! it, runs Back / Skip / Exit / Next through the engine, and reports `element` steps as done when
//! their control is on screen this frame. Automation ids: `coach.card`, `coach.back`, `coach.skip`,
//! `coach.exit`, `coach.next`, `coach.close` (the "lesson complete" card) and `coach.spotlight`.
//!
//! Above the card, "Ask the coach" (G2.12 part 2, [`ask_box`]): a question answered from the
//! fork's guide by the engine (`coach.ask`), with steps whose controls were checked against the
//! automation ids this UI knows ([`crate::coach_ids`]); a clicked step is outlined by the same
//! spotlight.
//!
//! Between the two, a context hint (G2.12 part 3, [`hint_card`]): the engine decides when one
//! shows (`Session::coach`'s `hint`); this draws it and reports the controls hints wait for
//! when they are on screen. Ids: `coach.hint.card`, `coach.hint.dismiss`, `coach.hint.lesson`,
//! `coach.hint.off`.
//!
//! The Lessons, Ask the coach and Assistant buttons ([`bar_buttons`], FORK-PLAN §1a.78) sit in the
//! Resolve page bar left of Home, and on the right of the Premiere workflow's header.
//!
//! The lesson list ([`lessons_list`]) opens from the Lessons button or Help ▸ Grading Lessons…
//! (`coach.showLessons`). Ids: `coach.lessons.open`, `coach.lessons`,
//! `coach.lessons.close`, `coach.lessons.item.{lesson id}` (its Start button).

use egui::{Align2, CornerRadius, Frame, Id, LayerId, Margin, Order, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::coach::Expect;
use serde_json::{Value, json};

use crate::FilmcraftApp;

/// The card's width in points.
pub const WIDTH: f32 = 340.0;
/// At most this many `element` steps finish in one frame (consecutive steps already on screen).
const MAX_SEEN_PER_FRAME: usize = 8;

/// Element steps whose control is on screen this frame are done; outside a lesson, a control a
/// context hint waits for is reported once it is on screen.
fn report_elements(app: &mut FilmcraftApp) {
    let watched = app.session.coach_hint_elements();
    if let Some(id) = watched.into_iter().find(|id| app.auto.elements.iter().any(|e| e.id == *id)) {
        app.session.coach_element_seen(id);
    }
    for _ in 0..MAX_SEEN_PER_FRAME {
        let Some(Expect::Element(id)) = app.session.coach.step().map(|s| s.expect.clone()) else { return };
        if !app.auto.elements.iter().any(|e| e.id == id) {
            return;
        }
        app.session.coach_element_seen(&id);
    }
}

/// The current step's targets, once per step: a panel that scrolls calls this, scrolls the first
/// of its controls named here into view (`Ui::scroll_to_rect`) and then [`scrolled`], so the
/// highlighted control is never hidden below the fold. `None` once done (the user may scroll away).
/// The same for the control of an "Ask the coach" step that was clicked (while no lesson step
/// has a target).
pub fn scroll_request(app: &FilmcraftApp, ctx: &egui::Context) -> Option<Vec<String>> {
    if let Some(step) = app.session.coach.step().filter(|s| !s.targets.is_empty()) {
        let done: Option<u64> = ctx.data(|d| d.get_temp(Id::new("coach-scrolled")));
        return (done != Some(app.session.coach.revision)).then(|| step.targets.clone());
    }
    let id = app.ui.coach_ask.spotlight.clone()?;
    let done: Option<u64> = ctx.data(|d| d.get_temp(Id::new("coach-ask-scrolled")));
    (done != Some(app.ui.coach_ask.clicks)).then(|| vec![id])
}

/// The step's control was scrolled into view (see [`scroll_request`]).
pub fn scrolled(app: &FilmcraftApp, ctx: &egui::Context) {
    if app.session.coach.step().is_some_and(|s| !s.targets.is_empty()) {
        let rev = app.session.coach.revision;
        ctx.data_mut(|d| d.insert_temp(Id::new("coach-scrolled"), rev));
    } else {
        let clicks = app.ui.coach_ask.clicks;
        ctx.data_mut(|d| d.insert_temp(Id::new("coach-ask-scrolled"), clicks));
    }
}

fn run(app: &mut FilmcraftApp, cmd: &str) {
    if let Err(e) = app.session.execute(cmd, json!({})) {
        app.ui.status = e.to_string();
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    report_elements(app);
    let (card, spotlit) = lesson_card(app, ctx);
    let mut above = card.map(|r| r.height() + 8.0).unwrap_or(0.0);
    if let Some(r) = hint_card(app, ctx, above) {
        above += r.height() + 8.0;
    }
    ask_box(app, ctx, above);
    // a clicked answer step's control, while no lesson step outlines one
    if !spotlit && let Some(id) = app.ui.coach_ask.spotlight.clone() {
        spotlight(app, ctx, std::slice::from_ref(&id));
    }
}

/// Outline the first of `targets` that is on screen (this frame's rect, else last frame's) and
/// register it as `coach.spotlight`; returns the control's rect. Nothing is drawn over a modal
/// window (Settings, recovery…): the control under it can't be used anyway.
fn spotlight(app: &mut FilmcraftApp, ctx: &egui::Context, targets: &[String]) -> Option<egui::Rect> {
    let t = app.tokens;
    let screen = ctx.content_rect();
    let rect_of = |e: &crate::automation::Element| egui::Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]));
    let on_screen = |r: &egui::Rect| r.is_positive() && r.is_finite();
    // (the panel holding a control clips it: one scrolled out of its panel isn't on screen)
    let panel_of = |r: egui::Rect| {
        app.auto
            .elements
            .iter()
            .filter(|e| e.id.starts_with("panel.") && rect_of(e).intersects(r))
            .map(rect_of)
            .min_by(|a, b| a.area().total_cmp(&b.area()))
            .unwrap_or(screen)
    };
    let target = targets.iter().find_map(|id| {
        let found = app.auto.elements.iter().find(|e| &e.id == id).or_else(|| app.auto.previous.iter().find(|e| &e.id == id));
        let r = found.map(rect_of).filter(on_screen)?;
        let clip = panel_of(r).expand(4.0).intersect(screen);
        on_screen(&r.intersect(clip)).then(|| (id.clone(), r, clip))
    });
    let target_rect = target.as_ref().map(|(_, r, _)| *r);
    let modal = ctx.memory(|m| m.top_modal_layer()).is_some();
    if let Some((id, r, clip)) = target.filter(|_| !modal) {
        let r = r.expand(3.0);
        let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("coach-spotlight"))).with_clip_rect(clip);
        p.rect_filled(r, 4.0, t.accent.gamma_multiply(0.12));
        p.rect_stroke(r, 4.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
        app.auto.add("coach.spotlight", r.intersect(clip), &id);
    }
    target_rect
}

/// How far above the window's bottom edge the coach's cards sit: clear of the status bar and the
/// 30 pt page bar G1.8 had; the Resolve workflow's taller page bar (fork RS3) lifts them by the
/// difference.
fn bottom_offset(app: &FilmcraftApp) -> f32 {
    let taller = if app.resolve_workflow() { (crate::panels::pages::HEIGHT - 30.0).max(0.0) } else { 0.0 };
    -48.0 - taller
}

/// The lesson card (or the "lesson complete" card): its rect, and whether a lesson step's
/// control is outlined.
fn lesson_card(app: &mut FilmcraftApp, ctx: &egui::Context) -> (Option<egui::Rect>, bool) {
    let t = app.tokens;
    let screen = ctx.content_rect();
    // above the status bar and the page bar
    let offset = vec2(-16.0, bottom_offset(app));
    let frame = Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.accent)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(12));

    let Some(run_state) = app.session.coach.active.clone() else {
        // the "lesson complete" card, until closed
        let Some(done) = app.session.coach.finished.clone() else { return (None, false) };
        let mut close = false;
        let area = egui::Area::new(Id::new("coach-card")).order(Order::Foreground).anchor(Align2::RIGHT_BOTTOM, offset).show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(WIDTH.min(screen.width() - 32.0).max(120.0));
                ui.label(RichText::new(tl!("Lesson complete")).strong().color(t.text));
                ui.label(RichText::new(&done.title).color(t.text_dim));
                ui.add_space(6.0);
                let r = ui.button(tl!("Close"));
                app.auto.add_response("coach.close", &r, "Close");
                close = r.clicked();
            });
        });
        app.auto.add("coach.card", area.response.rect, &done.title);
        let rect = area.response.rect;
        if close {
            run(app, "coach.exit");
        }
        return (Some(rect), false);
    };
    let Some(step) = run_state.current().cloned() else { return (None, false) };

    // the spotlight: an outline around the first target control on screen
    let target_rect = spotlight(app, ctx, &step.targets);

    let n = run_state.step + 1;
    let total = run_state.lesson.steps.len();
    let manual = step.expect == Expect::Manual;
    let mut clicked: Option<&'static str> = None;
    let area = egui::Area::new(Id::new("coach-card")).order(Order::Foreground).anchor(Align2::RIGHT_BOTTOM, offset).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(WIDTH.min(screen.width() - 32.0).max(120.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(&run_state.lesson.title).strong().color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(tlf!("Step {n} of {total}", n = n, total = total)).small().color(t.text_faint));
                });
            });
            let tags: Vec<&str> = run_state.lesson.provenance.iter().map(|p| p.tag()).collect();
            ui.label(RichText::new(tags.join(" ")).small().color(t.text_faint));
            ui.add_space(4.0);
            ui.label(RichText::new(&step.text).color(t.text));
            if !step.why.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{}: {}", tl!("Why"), step.why)).color(t.text_dim));
            }
            if !step.targets.is_empty() && target_rect.is_none() {
                ui.add_space(4.0);
                ui.label(RichText::new(tl!("The highlighted control isn't on screen yet.")).small().italics().color(t.text_faint));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let r = ui.add_enabled(run_state.step > 0, egui::Button::new(tl!("Back")));
                app.auto.add_response("coach.back", &r, "Back");
                if r.clicked() {
                    clicked = Some("coach.back");
                }
                let r = ui.button(tl!("Skip"));
                app.auto.add_response("coach.skip", &r, "Skip");
                if r.clicked() {
                    clicked = Some("coach.skip");
                }
                let r = ui.button(tl!("Exit"));
                app.auto.add_response("coach.exit", &r, "Exit");
                if r.clicked() {
                    clicked = Some("coach.exit");
                }
                if manual {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.button(RichText::new(tl!("Next")).strong());
                        app.auto.add_response("coach.next", &r, "Next");
                        if r.clicked() {
                            clicked = Some("coach.next");
                        }
                    });
                }
            });
        });
    });
    app.auto.add("coach.card", area.response.rect, &run_state.lesson.title);
    if let Some(cmd) = clicked {
        run(app, cmd);
    }
    (Some(area.response.rect), target_rect.is_some())
}

// ---------- the bar buttons ----------

/// Lessons, Ask the coach and Assistant as pills ending at `right` (centred on `cy`), right to
/// left; returns the left edge of the first. Each lights while what it opens is open. Automation
/// ids: `coach.lessons.open`, `coach.ask.open`, `assistant.open`.
pub fn bar_buttons(app: &mut FilmcraftApp, ui: &mut egui::Ui, right: f32, cy: f32) -> f32 {
    let t = app.tokens;
    let assistant_on = crate::panels::assistant::is_open(app);
    let items = [
        ("assistant.open", tl!("Assistant"), assistant_on, tl!("Chat with the assistant: a panel you can float anywhere or dock beside any panel.")),
        ("coach.ask.open", tl!("Ask the coach"), app.ui.coach_ask.open, tl!("Ask the guide how to do something.")),
        ("coach.lessons.open", tl!("Lessons"), app.ui.coach_ask.lessons, tl!("Step-by-step lessons.")),
    ];
    let mut x = right;
    let mut clicked = None;
    for (id, label, on, tip) in items {
        let galley = ui.painter().layout_no_wrap(label.to_string(), crate::theme::Tokens::ui(11.5), t.text);
        let w = galley.size().x + 22.0;
        let r = Rect::from_min_size(pos2(x - w, cy - 11.0), vec2(w, 22.0));
        let resp = ui.interact(r, Id::new(("coach-bar", id)), Sense::click()).on_hover_text(tip);
        let fill = if on {
            t.hover
        } else if resp.hovered() {
            t.hover.gamma_multiply(0.6)
        } else {
            t.panel_bg
        };
        ui.painter().rect(r, CornerRadius::same(11), fill, Stroke::new(1.0, if on { t.accent } else { t.control_border }), StrokeKind::Inside);
        let ink = if on || resp.hovered() { t.text } else { t.text_dim };
        ui.painter().galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - galley.size().y / 2.0), galley, ink);
        app.auto.add_response(id, &resp, label);
        if resp.clicked() {
            clicked = Some(id);
        }
        x = r.min.x - 6.0;
    }
    match clicked {
        Some("assistant.open") => crate::panels::assistant::toggle(app, ui.ctx()),
        Some("coach.ask.open") => {
            if app.ui.coach_ask.open {
                close_ask(app);
            } else {
                app.ui.coach_ask.open = true;
                app.ui.coach_ask.lessons = false;
            }
        }
        Some(_) => {
            app.ui.coach_ask.lessons = !app.ui.coach_ask.lessons;
            if app.ui.coach_ask.lessons {
                app.ui.coach_ask.open = false;
            }
        }
        None => {}
    }
    x + 6.0
}

/// Close the Ask box: its spotlight, follow-up memory and running assistant answer go with it.
fn close_ask(app: &mut FilmcraftApp) {
    app.ui.coach_ask.open = false;
    app.ui.coach_ask.spotlight = None;
    app.ui.coach_ask.last_exchange = None;
    stop_assist(app);
}

// ---------- Ask the coach ----------

/// Ask the engine (`coach.ask`); the answer replaces the last one.
fn ask(app: &mut FilmcraftApp) {
    let q = app.ui.coach_ask.input.trim().to_string();
    if q.is_empty() {
        return;
    }
    app.ui.coach_ask.spotlight = None;
    match app.session.execute("coach.ask", json!({ "question": q })) {
        Ok(v) => app.ui.coach_ask.answer = Some(v),
        Err(e) => app.ui.status = e.to_string(),
    }
    if app.session.prefs.color.coach.assistant {
        start_assist(app, &q);
    }
}

/// Ask the assistant model too (`coach.assist`, fork AI3.5); a question still being answered is
/// stopped first (one answer at a time on this PC).
fn start_assist(app: &mut FilmcraftApp, q: &str) {
    stop_assist(app);
    let mut p = json!({ "question": q, "page": app.ui.workspace, "context": "auto" });
    // a follow-up: the last checked exchange goes with it
    if let Some((pq, pa)) = &app.ui.coach_ask.last_exchange {
        p["previousQuestion"] = json!(pq);
        p["previousAnswer"] = json!(pa);
    }
    match app.session.execute("coach.assist", p) {
        Ok(v) => {
            app.ui.coach_ask.assist_id = v["id"].as_u64();
            app.ui.coach_ask.assist_question = q.to_string();
        }
        Err(e) => app.ui.status = e.to_string(),
    }
}

/// Stop the assistant's answer (`coach.assistForget`) and drop what it wrote.
fn stop_assist(app: &mut FilmcraftApp) {
    if let Some(id) = app.ui.coach_ask.assist_id.take() {
        // an id that is already gone is fine: nothing is running for it
        let _ = app.session.execute("coach.assistForget", json!({ "id": id }));
    }
    app.ui.coach_ask.assist = None;
}

/// Read the assistant's answer so far (`coach.assistResult`, no waiting); repaint while it writes.
fn poll_assist(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(id) = app.ui.coach_ask.assist_id else { return };
    match app.session.execute("coach.assistResult", json!({ "id": id })) {
        Ok(v) => {
            match v["state"].as_str() {
                Some("pending") => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
                // done, or forgotten elsewhere (control channel): stop polling
                _ => app.ui.coach_ask.assist_id = None,
            }
            // a checked model answer is what the next question may follow up on
            if v["state"] == "done" && v["source"] == "model" {
                let lines: Vec<&str> = v["lines"].as_array().map(|l| l.iter().filter_map(|x| x["text"].as_str()).collect()).unwrap_or_default();
                app.ui.coach_ask.last_exchange = Some((app.ui.coach_ask.assist_question.clone(), lines.join("\n")));
            }
            if v["state"] != "unknown" {
                // the id goes with the answer: rating and sharing name it (AI5)
                let mut v = v;
                v["id"] = json!(id);
                app.ui.coach_ask.assist = Some(v);
            }
        }
        Err(e) => {
            app.ui.coach_ask.assist_id = None;
            app.ui.status = e.to_string();
        }
    }
}

/// The assistant's part of the box: its text while it writes (with Stop), then the checked lines,
/// the sections it was given, and why the guide's answer is shown instead when it couldn't help.
/// Automation ids: `coach.ask.assistant` (the checkbox), `coach.ask.assistStop`,
/// `coach.ask.assistLine.{n}` (1-based).
fn assistant_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, action: &mut Option<AskAction>) {
    let t = app.tokens;
    let Some(a) = app.ui.coach_ask.assist.clone() else {
        if app.ui.coach_ask.assist_id.is_some() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(tl!("The assistant is reading the guide…")).small().color(t.text_dim));
            });
        }
        return;
    };
    ui.add_space(6.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Assistant")).strong().color(t.text));
        if a["state"] != "pending" && app.ui.coach_ask.last_exchange.is_some() {
            let r = ui.small_button(tl!("New topic")).on_hover_text(tl!("The next question won't follow up on this answer."));
            app.auto.add_response("coach.ask.newTopic", &r, "New topic");
            if r.clicked() {
                *action = Some(AskAction::NewTopic);
            }
        }
    });
    if a["state"] == "pending" {
        let partial = a["partial"].as_str().unwrap_or("");
        if partial.trim().is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(tl!("The assistant is reading the guide…")).small().color(t.text_dim));
            });
        } else {
            egui::ScrollArea::vertical().id_salt("coach-assist-partial").max_height(160.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                ui.label(RichText::new(partial).color(t.text_dim));
            });
            ui.label(RichText::new(tl!("Not checked against the guide yet.")).small().italics().color(t.text_faint));
        }
        let r = ui.small_button(tl!("Stop"));
        app.auto.add_response("coach.ask.assistStop", &r, "Stop");
        if r.clicked() {
            *action = Some(AskAction::AssistStop);
        }
        return;
    }
    seen_lines(app, ui, &a, "coach.ask.assistSeen");
    reference_block(app, ui, &a, "coach.ask.reference", "coach.ask.doThis");
    if a["source"] != "model" {
        let why = match a["fallback"]["kind"].as_str() {
            Some("notReady") => {
                tl!("The assistant can't run here yet (its model isn't installed or this build has none), so the guide's answer above is all there is.")
            }
            Some("ungrounded") => tl!("The assistant's answer named things Revolution doesn't have, so it was left out; the guide's answer above is checked."),
            Some("nothingFound") => tl!("The guide has nothing on this, so the assistant didn't guess."),
            _ => tl!("The assistant couldn't answer this time; the guide's answer above still stands."),
        };
        ui.label(RichText::new(why).small().color(t.text_dim));
        if let Some(id) = a["id"].as_u64() {
            super::share_example::rate_row(app, ui, id, &super::share_example::ASK_BOX_IDS);
        }
        return;
    }
    if a["dontKnow"].as_bool() == Some(true) {
        let r = ui.label(RichText::new(tl!("The assistant says the guide doesn't cover this; the guide's closest answer is above.")).small().color(t.text_dim));
        app.auto.add_response("coach.ask.assistDontKnow", &r, "doesn't cover");
    }
    let lines = a["lines"].as_array().cloned().unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("coach-assist-lines").max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
        for (i, l) in lines.iter().enumerate() {
            let text = l["text"].as_str().unwrap_or("");
            // "General advice:" lines aren't from the guide: dimmer
            let color = if l["general"].as_bool() == Some(true) { t.text_dim } else { t.text };
            let r = ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            app.auto.add_response(&format!("coach.ask.assistLine.{}", i + 1), &r, text);
        }
    });
    let cited: Vec<&str> = a["cited"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let sources: Vec<&str> = a["sources"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let shown = if cited.is_empty() { &sources } else { &cited };
    if !shown.is_empty() {
        ui.label(RichText::new(tlf!("Sources: {list}", list = shown.join(", "))).small().color(t.text_faint));
    }
    // "Show me": the lesson for what the assistant said, unless the guide's answer above offers it
    if let (Some(id), Some(title)) = (a["lesson"]["id"].as_str(), a["lesson"]["title"].as_str()) {
        let above = app.ui.coach_ask.answer.as_ref().and_then(|g| g["lesson"]["id"].as_str().map(|x| x == id)).unwrap_or(false);
        if !above {
            let r = ui.link(tlf!("Show me: {title}", title = title));
            app.auto.add_response("coach.ask.assistLesson", &r, title);
            if r.clicked() {
                *action = Some(AskAction::Lesson(id.to_string()));
            }
        }
    }
    let removed = a["removedLines"].as_u64().unwrap_or(0);
    if removed > 0 {
        ui.label(
            RichText::new(tlf!("{n} lines left out: they named controls, commands or shortcuts the guide doesn't have.", n = removed))
                .small()
                .italics()
                .color(t.text_faint),
        );
    }
    if let Some(id) = a["id"].as_u64() {
        super::share_example::rate_row(app, ui, id, &super::share_example::ASK_BOX_IDS);
    }
}

/// What a click in the box asks for (done after drawing).
enum AskAction {
    Close,
    Ask,
    Step(usize),
    Lesson(String),
    /// Open (true) or close the lesson list.
    Lessons(bool),
    /// Stop the assistant's answer.
    AssistStop,
    /// Switch "Also ask the assistant" on or off (off also stops it).
    Assistant(bool),
    /// Forget the last exchange: the next question starts a new topic.
    NewTopic,
}

/// The "Ask the coach" box, above the lesson card: a question box that answers from the fork's
/// guide with steps naming real controls (`coach.ask`). Shown in the Color Grading workspace,
/// with a lesson, or once opened. Automation ids: `coach.ask.open`, `coach.ask.close`,
/// `coach.ask.input`, `coach.ask.go`, `coach.ask.step.{n}` (1-based; a click outlines the
/// step's control) and `coach.ask.lesson` (starts the suggested lesson).
fn ask_box(app: &mut FilmcraftApp, ctx: &egui::Context, above: f32) {
    // closed, the box is only its buttons in the page bar / header (`bar_buttons`)
    if !(app.ui.coach_ask.open || app.ui.coach_ask.lessons) {
        return;
    }
    poll_assist(app, ctx);
    let t = app.tokens;
    let screen = ctx.content_rect();
    let offset = vec2(-16.0, bottom_offset(app) - above);
    let frame = Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.control_border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10));
    let mut action: Option<AskAction> = None;
    let area = egui::Area::new(Id::new("coach-ask")).order(Order::Foreground).anchor(Align2::RIGHT_BOTTOM, offset).show(ctx, |ui| {
        if app.ui.coach_ask.lessons {
            action = lessons_list(app, ui, frame, above);
            return;
        }
        frame.show(ui, |ui| {
            let width = WIDTH.min(screen.width() - 32.0).max(120.0);
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Ask the coach")).strong().color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.small_button("×").on_hover_text(tl!("Close"));
                    app.auto.add_response("coach.ask.close", &r, "Close");
                    if r.clicked() {
                        action = Some(AskAction::Close);
                    }
                });
            });
            ui.horizontal(|ui| {
                let edit = egui::TextEdit::singleline(&mut app.ui.coach_ask.input)
                    .hint_text(tl!("e.g. how do I make the sky bluer?"))
                    .char_limit(300)
                    .desired_width((width - 64.0).max(60.0));
                let r = ui.add(edit);
                app.auto.add_response("coach.ask.input", &r, "Ask the coach");
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let go = ui.add_enabled(!app.ui.coach_ask.input.trim().is_empty(), egui::Button::new(tl!("Ask")));
                app.auto.add_response("coach.ask.go", &go, "Ask");
                if go.clicked() || enter {
                    action = Some(AskAction::Ask);
                }
            });
            // the preference `color.coach.assistant` (also in Settings ▸ Color ▸ Grading Coach)
            let mut on = app.session.prefs.color.coach.assistant;
            let r = ui.checkbox(&mut on, RichText::new(tl!("Also ask the assistant (runs on this computer)")).small());
            app.auto.add_response("coach.ask.assistant", &r, "Also ask the assistant");
            if r.changed() {
                action = Some(AskAction::Assistant(on));
            }
            let Some(answer) = app.ui.coach_ask.answer.clone() else { return };
            ui.add_space(6.0);
            let steps = answer["steps"].as_array().cloned().unwrap_or_default();
            if steps.is_empty() {
                let covered = answer["covered"].as_bool().unwrap_or(false);
                let asked_nothing = answer["message"].as_str().is_some_and(|m| m.starts_with("Ask about"));
                let none = if asked_nothing {
                    Some(tl!("Ask about a grading task.").to_string())
                } else if let Some(word) = answer["missing"].as_str().filter(|_| !covered) {
                    Some(tlf!("The guide doesn't mention “{word}”.", word = word))
                } else if !covered {
                    Some(tl!("The guide doesn't cover this yet.").to_string())
                } else if !answer["lesson"].is_object() {
                    Some(tl!("The guide has no step-by-step answer for this yet.").to_string())
                } else {
                    None
                };
                if let Some(none) = none {
                    ui.label(RichText::new(none).color(t.text_dim));
                }
                if let Some(summary) = answer["summary"].as_str().filter(|s| !s.is_empty()) {
                    ui.label(RichText::new(summary).small().color(t.text_dim));
                }
            } else {
                // a "why" / "what is it for" question: the section's why comes before the steps
                if let Some(summary) = answer["summary"].as_str().filter(|s| !s.is_empty() && answer["explain"].as_bool() == Some(true)) {
                    ui.label(RichText::new(summary).color(t.text_dim));
                    ui.add_space(4.0);
                }
                egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, st) in steps.iter().enumerate() {
                        let n = i + 1;
                        let text = st["text"].as_str().unwrap_or("");
                        let ui_id = st["ui"].as_str();
                        let lit = ui_id.is_some() && app.ui.coach_ask.spotlight.as_deref() == ui_id;
                        let color = if lit { t.accent } else { t.text };
                        let r = ui.add(egui::Label::new(RichText::new(format!("{n}. {text}")).color(color)).wrap().sense(egui::Sense::click()));
                        let r = if ui_id.is_some() {
                            r.on_hover_text(tl!("Click to highlight this control.")).on_hover_cursor(egui::CursorIcon::PointingHand)
                        } else {
                            r
                        };
                        app.auto.add_response(&format!("coach.ask.step.{n}"), &r, text);
                        if r.clicked() {
                            action = Some(AskAction::Step(i));
                        }
                        ui.add_space(2.0);
                    }
                });
            }
            if let (Some(id), Some(title)) = (answer["lesson"]["id"].as_str(), answer["lesson"]["title"].as_str()) {
                ui.add_space(4.0);
                let label = if steps.is_empty() { tlf!("Try the lesson “{title}”", title = title) } else { tlf!("Lesson: {title}", title = title) };
                let r = ui.link(label);
                app.auto.add_response("coach.ask.lesson", &r, title);
                if r.clicked() {
                    action = Some(AskAction::Lesson(id.to_string()));
                }
            }
            // the section the steps come from (a "Why" section's steps come from its "Steps")
            let k = if steps.is_empty() { 0 } else { answer["stepsSource"].as_u64().and_then(|k| usize::try_from(k).ok()).unwrap_or(0) };
            if let Some(src) = answer["sources"].get(k).or_else(|| answer["sources"].get(0)) {
                let tags: Vec<&str> = src["provenance"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let from = tlf!("From the guide: {heading}", heading = src["heading"].as_str().unwrap_or(""));
                ui.label(RichText::new(format!("{from} ({}) {}", src["page"].as_str().unwrap_or(""), tags.join(" "))).small().color(t.text_faint));
            }
            let removed = answer["removed"].as_u64().unwrap_or(0);
            if removed > 0 {
                ui.label(
                    RichText::new(match (answer["removedControls"].as_u64().unwrap_or(0) > 0, answer["removedCommands"].as_u64().unwrap_or(0) > 0) {
                        (true, false) => tlf!("{n} steps left out: they name controls that aren't available here.", n = removed),
                        (false, true) => tlf!("{n} steps left out: they name commands that aren't available here.", n = removed),
                        _ => tlf!("{n} steps left out: they name controls or commands that aren't available here.", n = removed),
                    })
                    .small()
                    .italics()
                    .color(t.text_faint),
                );
            }
            assistant_view(app, ui, &mut action);
        });
    });
    if app.ui.coach_ask.lessons {
        app.auto.add("coach.lessons", area.response.rect, "Lessons");
    } else if app.ui.coach_ask.open {
        app.auto.add("coach.ask", area.response.rect, "Ask the coach");
    }
    match action {
        Some(AskAction::Lessons(open)) => app.ui.coach_ask.lessons = open,
        Some(AskAction::Close) => close_ask(app),
        Some(AskAction::NewTopic) => app.ui.coach_ask.last_exchange = None,
        Some(AskAction::AssistStop) => stop_assist(app),
        Some(AskAction::Assistant(on)) => {
            if let Err(e) = app.session.execute("prefs.set", json!({ "key": "color.coach.assistant", "value": on })) {
                app.ui.status = e.to_string();
            }
            if !on {
                app.ui.coach_ask.last_exchange = None;
                stop_assist(app);
            }
        }
        Some(AskAction::Ask) => ask(app),
        Some(AskAction::Step(i)) => {
            let st = app.ui.coach_ask.answer.as_ref().and_then(|a| a["steps"].get(i)).cloned().unwrap_or_default();
            if let Some(id) = st["ui"].as_str() {
                let same = app.ui.coach_ask.spotlight.as_deref() == Some(id);
                app.ui.coach_ask.spotlight = (!same).then(|| id.to_string());
                app.ui.coach_ask.clicks = app.ui.coach_ask.clicks.wrapping_add(1);
            } else if let Some(cmd) = st["command"].as_str() {
                app.ui.status = tlf!("This step runs the command {cmd}.", cmd = cmd);
            }
        }
        Some(AskAction::Lesson(id)) => {
            app.ui.coach_ask.spotlight = None;
            app.ui.coach_ask.lessons = false;
            if let Err(e) = app.session.execute("coach.start", json!({ "lesson": id })) {
                app.ui.status = e.to_string();
            }
        }
        None => {}
    }
}

// ---------- the lesson list ----------

/// The lesson list (in place of the Ask box): each lesson's title, a tick when finished, its
/// one-line summary and a Start button (`coach.lessons.item.{id}`).
fn lessons_list(app: &mut FilmcraftApp, ui: &mut egui::Ui, frame: Frame, above: f32) -> Option<AskAction> {
    let t = app.tokens;
    let width = WIDTH.min(ui.ctx().content_rect().width() - 32.0).max(120.0);
    let mut action = None;
    frame.show(ui, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Lessons")).strong().color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.small_button("×").on_hover_text(tl!("Close"));
                app.auto.add_response("coach.lessons.close", &r, "Close");
                if r.clicked() {
                    action = Some(AskAction::Lessons(false));
                }
            });
        });
        ui.add_space(4.0);
        // (an area anchored at the bottom only gets the room it had last frame: ask for the
        // list's height outright, within the window)
        // (minus the cards stacked below it)
        let height = (ui.ctx().content_rect().height() - 200.0 - above).clamp(120.0, 360.0);
        egui::ScrollArea::vertical().max_height(height).min_scrolled_height(height).auto_shrink([false, true]).show(ui, |ui| {
            for l in filmcraft_engine::coach::bundled() {
                let done = app.session.prefs.color.coach.completed.contains(&l.id);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&l.title).strong().color(t.text));
                    if done {
                        ui.label(RichText::new("✔").color(t.accent)).on_hover_text(tl!("Completed"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if done { tl!("Again") } else { tl!("Start") };
                        let r = ui.button(label);
                        app.auto.add_response(&format!("coach.lessons.item.{}", l.id), &r, &l.title);
                        if r.clicked() {
                            action = Some(AskAction::Lesson(l.id.clone()));
                        }
                    });
                });
                if !l.summary.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&l.summary).small().color(t.text_dim)).wrap());
                }
                ui.add_space(6.0);
            }
        });
    });
    action
}

// ---------- context hints ----------

/// The context hint on screen (the engine chose it), `above` points over the bottom-right
/// anchor: why, what, a link to its lesson, Got it and Turn hints off. Returns its rect.
fn hint_card(app: &mut FilmcraftApp, ctx: &egui::Context, above: f32) -> Option<egui::Rect> {
    let hint = app.session.coach.hint.clone()?;
    poll_assist(app, ctx);
    let t = app.tokens;
    let screen = ctx.content_rect();
    let offset = vec2(-16.0, bottom_offset(app) - above);
    let frame = Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.accent)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10));
    let lesson = hint.lesson.as_deref().and_then(filmcraft_engine::coach::lesson);
    let mut clicked: Option<(&'static str, Value)> = None;
    let area = egui::Area::new(Id::new("coach-hint")).order(Order::Foreground).anchor(Align2::RIGHT_BOTTOM, offset).show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(WIDTH.min(screen.width() - 32.0).max(120.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Tip")).small().color(t.accent));
                ui.label(RichText::new(&hint.title).strong().color(t.text));
            });
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(&hint.why).color(t.text)).wrap());
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&hint.text).color(t.text_dim)).wrap());
            ui.add_space(6.0);
            if let Some(l) = &lesson {
                let r = ui.link(tlf!("Lesson: {title}", title = l.title.as_str()));
                app.auto.add_response("coach.hint.lesson", &r, &l.title);
                if r.clicked() {
                    clicked = Some(("coach.start", json!({ "lesson": l.id })));
                }
            }
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Got it"));
                app.auto.add_response("coach.hint.dismiss", &r, "Got it");
                if r.clicked() {
                    clicked = Some(("coach.dismissHint", json!({})));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add(egui::Button::new(RichText::new(tl!("Turn hints off")).small().color(t.text_faint)).frame(false));
                    app.auto.add_response("coach.hint.off", &r, "Turn hints off");
                    if r.clicked() {
                        clicked = Some(("coach.dismissHint", json!({"off": true})));
                    }
                });
            });
        });
    });
    let rect = area.response.rect;
    app.auto.add("coach.hint.card", rect, &hint.title);
    if let Some((cmd, params)) = clicked
        && let Err(e) = app.session.execute(cmd, params)
    {
        app.ui.status = e.to_string();
    }
    Some(rect)
}

/// REF: the reference picture's part of an answer (`coach.assist` `reference`, written by
/// Revolution from measurements, not the model): theirs vs yours, the grade recipe, what belongs
/// on set, a warning when a grade can't get there, what the picture model saw, and **Do this**
/// (Shot Match to the reference: one ordinary node), with automation ids `block_id` and `do_id`.
pub(crate) fn reference_block(app: &mut FilmcraftApp, ui: &mut egui::Ui, a: &Value, block_id: &str, do_id: &str) {
    let r = &a["reference"];
    if !r.is_object() {
        return;
    }
    let t = app.tokens;
    let strs = |v: &Value, n: usize| -> Vec<String> {
        v.as_array().map(|l| l.iter().filter_map(Value::as_str).take(n).map(str::to_string).collect()).unwrap_or_default()
    };
    let measured = strs(&r["measured"], 8);
    let grade: Vec<String> =
        r["grade"].as_array().map(|l| l.iter().filter_map(|s| s["text"].as_str()).take(12).map(str::to_string).collect()).unwrap_or_default();
    let on_set = strs(&r["onSet"], 8);
    let mut clicked = false;
    let resp = ui
        .vertical(|ui| {
            ui.separator();
            if !measured.is_empty() {
                ui.label(RichText::new(tl!("Reference picture (measured):")).small().strong().color(t.text_dim));
                for l in &measured {
                    ui.label(RichText::new(l).small().color(t.text_dim));
                }
            }
            if let Some(seen) = r["seen"].as_str().filter(|s| !s.is_empty()) {
                ui.label(RichText::new(tl!("Seen in the reference:")).small().strong().color(t.text_dim));
                ui.label(RichText::new(seen).small().color(t.text_dim));
            }
            ui.label(RichText::new(tl!("In the grade:")).strong().color(t.text));
            for (i, l) in grade.iter().enumerate() {
                ui.label(RichText::new(format!("{}. {l}", i + 1)).color(t.text));
            }
            ui.label(RichText::new(tl!("On set:")).strong().color(t.text));
            for l in &on_set {
                ui.label(RichText::new(format!("• {l}")).color(t.text));
            }
            if let Some(n) = r["tooDifferent"].as_str() {
                ui.label(RichText::new(n).italics().color(t.text_dim));
            }
            let b = ui.button(tl!("Do this: Shot Match to the reference"));
            app.auto.add_response(do_id, &b, "Do this: Shot Match to the reference");
            clicked = b.clicked();
        })
        .response;
    app.auto.add_response(block_id, &resp, "Reference picture");
    if clicked
        && let Some(cmd) = r["doThis"]["command"].as_str()
        && let Err(e) = app.session.execute(cmd, r["doThis"]["params"].clone())
    {
        app.ui.status = e.to_string();
    }
}

/// AI4.1d: "On this frame (measured)": the frame's measured facts that fit the question
/// (`coach.assist` `seen`; FilmCraft's own measurements, not the model's words).
pub(crate) fn seen_lines(app: &mut FilmcraftApp, ui: &mut egui::Ui, a: &Value, auto_id: &str) {
    let seen: Vec<&str> = a["seen"].as_array().map(|l| l.iter().filter_map(Value::as_str).take(3).collect()).unwrap_or_default();
    if seen.is_empty() {
        return;
    }
    let t = app.tokens;
    let r = ui
        .vertical(|ui| {
            ui.label(RichText::new(tl!("On this frame (measured):")).small().strong().color(t.text_dim));
            for l in &seen {
                ui.label(RichText::new(*l).small().color(t.text_dim));
            }
        })
        .response;
    app.auto.add_response(auto_id, &r, "On this frame (measured)");
}
