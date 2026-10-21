//! Fork RS3 (RESOLVE-UI-PLAN): the Resolve workflow's page chrome and page layouts, driven
//! through the control channel: each page's workspace, panels, toolbar and page bar; the
//! toolbar's panel toggles; the Deliver page's render panels; page layouts saved like any
//! workspace; and the Premiere workflow keeping its header, workspaces and Export mode.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the window (1920×1080, like the
//! reference screenshots) with wgpu and write `pages-*.png`.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::state::Mode;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<PathBuf>,
}

impl Driver {
    /// The demo project in `workflow` (set through the Settings switch, as the user does).
    fn demo(workflow: Workflow) -> Self {
        let mut s = Session::default();
        s.prefs.workflow = Workflow::Premiere;
        s.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1920.0, 1080.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        let name = if workflow == Workflow::Resolve { "resolve" } else { "premiere" };
        d.exec("settings.setWorkflow", json!({"workflow": name}));
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
            assert!(self.harness.state().ui_error.is_none(), "the app caught a UI panic: {:?}", self.harness.state().ui_error);
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn menu(&mut self, id: &str, params: Value) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id, "params": params}));
        self.frames(3);
        r
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|x| x == id)
    }
    /// [x, y, w, h] of an element.
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("{id} not on screen")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap(), r[1].as_f64().unwrap(), r[2].as_f64().unwrap(), r[3].as_f64().unwrap()]
    }
    fn workspace(&self) -> String {
        self.harness.state().ui.workspace.clone()
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("pages-{name}.png"))).unwrap();
    }
}

#[test]
fn resolve_pages_open_their_layouts_with_toolbar_and_page_bar() {
    let mut d = Driver::demo(Workflow::Resolve);
    // the Resolve header replaces the Premiere one; the page bar has the app name, the seven
    // pages, Home and Settings
    assert!(d.ids("header.").is_empty(), "no Premiere header: {:?}", d.ids("header."));
    assert_eq!(d.ids("page.").len(), 7, "{:?}", d.ids("page."));
    for id in ["pageBar.app", "pageBar.home", "pageBar.settings", "pageToolbar.title"] {
        assert!(d.has(id), "{id}");
    }
    // the Cut page (fork RS4, `cut_page_ui.rs`); Fusion is the Compositor's page (fork C1, `comp_ui.rs`)
    assert!(d.has("page.cut"), "the Cut page");
    // the window opens on the Edit page (the Premiere Editing workspace is the Edit page here)
    assert_eq!(d.workspace(), "Edit Page");
    let pages: [(&str, &str, &[&str], &[&str]); 6] = [
        ("media", "Media Page", &["MediaBrowser", "Source", "Project", "Metadata"], &["MediaBrowser", "Project", "AudioMeters", "Metadata", "Inspector"]),
        ("cut", "Cut Page", &["Project", "Program", "AudioMeters", "CutTimeline"], &["Project", "Effects", "quickExport", "AudioTrackMixer", "Inspector"]),
        (
            "edit",
            "Edit Page",
            &["Project", "Source", "Program", "Timeline"],
            &["Project", "Effects", "Markers", "EffectControls", "quickExport", "AudioTrackMixer", "Metadata", "Inspector"],
        ),
        (
            "color",
            "Color Grading",
            &["Gallery", "Program", "ColorNodes", "ColorClips", "ColorPrimaries"],
            &["Gallery", "Project", "ColorClips", "ColorNodes", "LumetriScopes", "Effects"],
        ),
        (
            "fairlight",
            "Fairlight Page",
            &["AudioMeters", "Timeline", "AudioTrackMixer"],
            &["Project", "Effects", "Markers", "AudioTrackMixer", "AudioMeters", "Metadata", "Inspector"],
        ),
        ("deliver", "Deliver Page", &["RenderSettings", "Program", "RenderQueue", "Timeline"], &["RenderSettings", "RenderQueue"]),
    ];
    for (page, ws, panels, toolbar) in pages {
        d.click(&format!("page.{page}"));
        assert_eq!(d.workspace(), ws, "page {page}");
        assert_eq!(d.harness.state().ui.mode, Mode::Edit);
        for p in panels {
            assert!(d.has(&format!("panel.{p}")), "{page}: panel {p} on screen");
        }
        let shown = d.ids("pageToolbar.");
        let want: Vec<String> = toolbar.iter().map(|k| format!("pageToolbar.{k}")).chain(["pageToolbar.title".to_string()]).collect();
        let mut got = shown.clone();
        got.sort();
        let mut w = want.clone();
        w.sort();
        assert_eq!(got, w, "{page}: toolbar");
        d.shot(&format!("resolve-{page}"));
    }
    // the coach's buttons sit in the page bar left of Home, clear of the pages (FORK-PLAN §1a.78)
    d.click("page.color");
    let (lessons, ask, assistant, home) = (d.rect("coach.lessons.open"), d.rect("coach.ask.open"), d.rect("assistant.open"), d.rect("pageBar.home"));
    let deliver = d.rect("page.deliver");
    for (name, r) in [("Lessons", lessons), ("Ask the coach", ask), ("Assistant", assistant)] {
        assert!(r[1] >= home[1] - 8.0 && r[1] + r[3] <= home[1] + home[3] + 8.0, "{name} {r:?} on the page bar's row {home:?}");
        assert!(r[0] + r[2] <= home[0], "{name} {r:?} left of Home {home:?}");
        assert!(r[0] >= deliver[0] + deliver[2], "{name} {r:?} clear of the pages {deliver:?}");
    }
    assert!(lessons[0] + lessons[2] <= ask[0] && ask[0] + ask[2] <= assistant[0], "Lessons, Ask the coach, Assistant in order");
    // the Assistant opens floating over the page (the coach's box opens above the bar)
    d.click("assistant.open");
    d.shot("color-assistant-floating");
    d.click("coach.ask.open");
    d.shot("color-ask-open");
    d.click("coach.ask.open");
    d.click("assistant.open");
    // the Edit page's layout: the media pool on the left, source and timeline viewers side by
    // side, the timeline below them
    d.click("page.edit");
    let (pool, src, prg, tl) = (d.rect("panel.Project"), d.rect("panel.Source"), d.rect("panel.Program"), d.rect("panel.Timeline"));
    assert!(pool[0] < src[0] && src[0] < prg[0], "pool | source | program");
    assert!(tl[1] > src[1] + src[3] - 1.0 && tl[0] >= pool[0] + pool[2] - 1.0, "timeline under the viewers, right of the pool");
    // Deliver: Add to Render Queue queues the timeline, Render is there
    d.click("page.deliver");
    assert!(d.has("export.render") && d.has("export.addToQueue") && d.has("export.fileName"));
    d.click("export.addToQueue");
    let q = d.exec("export.queue.list", json!({}));
    assert_eq!(q["items"].as_array().map(Vec::len), Some(1), "{q}");
    assert!(d.ids("export.queue.item.").iter().any(|i| i.starts_with("export.queue.item.")), "the queue panel lists it");
    // File ▸ Export ▸ Media (the Export mode) is the Deliver page in this workflow
    d.click("page.edit");
    d.menu("mode.export", json!({}));
    assert_eq!(d.workspace(), "Deliver Page");
    assert_eq!(d.harness.state().ui.mode, Mode::Edit);
    // Resolve's page keys land on the pages too (Shift+4 Edit, Shift+7 Fairlight, Shift+8 Deliver)
    d.exec("shortcuts.loadPreset", json!({"name": "DaVinci Resolve Compatible"}));
    for (key, ws) in [("Shift+3", "Cut Page"), ("Shift+4", "Edit Page"), ("Shift+7", "Fairlight Page"), ("Shift+2", "Media Page"), ("Shift+8", "Deliver Page")]
    {
        d.ok("ui.key", json!({"key": key}));
        d.frames(3);
        assert_eq!(d.workspace(), ws, "{key}");
    }
}

#[test]
fn resolve_toolbar_toggles_panels_and_page_layouts_save_like_workspaces() {
    let mut d = Driver::demo(Workflow::Resolve);
    d.click("page.edit");
    // Inspector: not on the Edit page until toggled; it opens on the right
    assert!(!d.has("panel.Inspector"));
    d.click("pageToolbar.Inspector");
    assert!(d.has("panel.Inspector"), "Inspector shown");
    let (insp, prg) = (d.rect("panel.Inspector"), d.rect("panel.Program"));
    assert!(insp[0] >= prg[0] + prg[2] - 1.0, "Inspector right of the viewers: {insp:?} {prg:?}");
    d.shot("resolve-edit-inspector");
    // Effects is a tab behind the Media Pool: its toggle brings it forward, Media Pool's back
    d.click("pageToolbar.Effects");
    assert!(d.has("panel.Effects") && !d.has("panel.Project"));
    d.click("pageToolbar.Project");
    assert!(d.has("panel.Project") && !d.has("panel.Effects"));
    // Media Pool: hide it, then back as a column on the left
    d.click("pageToolbar.Project");
    assert!(!d.has("panel.Project"), "Media Pool hidden");
    d.click("pageToolbar.Project");
    assert!(d.has("panel.Project"));
    assert!(d.rect("panel.Project")[0] < d.rect("panel.Source")[0], "back on the left");
    // the layout is a workspace: save the change, visit another page, come back: still there
    d.menu("window.workspace.saveChanges", json!({}));
    d.click("page.color");
    d.click("page.edit");
    assert!(d.has("panel.Inspector"), "saved layout kept the Inspector");
    // deleting the saved changes brings back the built-in page layout
    d.menu("window.workspace.delete", json!({"name": "Edit Page"}));
    d.frames(2);
    assert_eq!(d.workspace(), "Edit Page");
    assert!(!d.has("panel.Inspector"), "built-in Edit page again");
    // Window ▸ Workspaces lists the pages, not the Premiere workspaces they stand for
    let names: Vec<String> = filmcraft_ui_egui::menus::menu_items(d.harness.state())
        .into_iter()
        .filter(|i| i.path == ["Window", "Workspaces"] && i.id.starts_with("window.workspace.") && i.checked.is_some())
        .map(|i| i.id)
        .collect();
    assert!(names.contains(&"window.workspace.editpage".to_string()) && !names.contains(&"window.workspace.editing".to_string()), "{names:?}");
    // Window ▸ Workspaces ▸ Editing (a Premiere name) opens the Edit page
    d.menu("window.workspace.editing", json!({}));
    assert_eq!(d.workspace(), "Edit Page");
}

#[test]
fn premiere_keeps_its_header_workspaces_and_export_mode() {
    let mut d = Driver::demo(Workflow::Premiere);
    for id in ["header.home", "header.mode.edit", "header.mode.export", "header.workspaces", "header.quickExport"] {
        assert!(d.has(id), "{id}");
    }
    assert!(d.ids("page.").is_empty() && d.ids("pageBar.").is_empty() && d.ids("pageToolbar.").is_empty(), "no Resolve chrome");
    // the coach's buttons and the Assistant on the header's right, clear of the workspace name
    let (assistant, name) = (d.rect("assistant.open"), d.rect("header.workspaceName"));
    for id in ["coach.lessons.open", "coach.ask.open", "assistant.open"] {
        let r = d.rect(id);
        assert!(r[1] + r[3] <= 40.0, "{id} {r:?} in the header");
        assert!(r[0] >= name[0] + name[2], "{id} {r:?} right of the workspace name {name:?}");
    }
    assert!(assistant[0] + assistant[2] <= d.rect("header.workspaces")[0], "left of the Workspaces button");
    // the header's own icons stop left of the window buttons (they once slid under them when
    // the full-screen preference and the window disagreed)
    if let Some(chrome) = ["window.fullScreen", "window.minimize"].into_iter().find(|id| d.has(id)) {
        let (fs, first) = (d.rect("header.fullscreen"), d.rect(chrome));
        assert!(fs[0] + fs[2] <= first[0], "header icons {fs:?} clear of the window buttons {first:?}");
    }
    d.shot("premiere-header");
    assert_eq!(d.workspace(), "Editing");
    assert_eq!(d.harness.state().ui.dock, filmcraft_ui_egui::dock::workspace("Editing"), "Premiere's Editing layout");
    d.shot("premiere-editing");
    // Export mode stays the Export mode
    d.menu("mode.export", json!({}));
    assert_eq!(d.harness.state().ui.mode, Mode::Export);
    assert!(d.has("export.button") && !d.has("export.render"));
    d.click("header.mode.edit");
    // the Window menu: Premiere's workspaces, no page layouts, no Resolve-only panels
    let items = filmcraft_ui_egui::menus::menu_items(d.harness.state());
    let ws: Vec<&str> = items.iter().filter(|i| i.path == ["Window", "Workspaces"]).map(|i| i.label.as_str()).collect();
    for w in filmcraft_ui_egui::dock::WORKSPACES {
        assert!(ws.contains(&w), "{w} in {ws:?}");
    }
    assert!(!ws.iter().any(|w| w.ends_with(" Page")), "{ws:?}");
    assert!(!items.iter().any(|i| i.label == "Render Settings" || i.label == "Render Queue"), "no Resolve-only panels in Window");
    // a round trip through Resolve comes back to Premiere's Editing layout and header
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    d.frames(4);
    assert_eq!(d.workspace(), "Edit Page");
    d.exec("settings.setWorkflow", json!({"workflow": "premiere"}));
    d.frames(4);
    assert_eq!(d.workspace(), "Editing");
    assert_eq!(d.harness.state().ui.dock, filmcraft_ui_egui::dock::workspace("Editing"));
    assert!(d.has("header.mode.edit") && d.ids("pageToolbar.").is_empty());
}

/// RS3 review: a workflow switch keeps each workflow's layout as it was left, unsaved changes
/// included; File ▸ Export ▸ Media on the Deliver page keeps its layout; the Import mode is the
/// Media page in the Resolve workflow.
#[test]
fn workflow_round_trip_keeps_unsaved_layouts_and_modes_land_on_pages() {
    use filmcraft_ui_egui::dock::PanelKind;
    let mut d = Driver::demo(Workflow::Premiere);
    // Premiere: an unsaved change to Editing
    d.harness.state_mut().ui.dock.close(PanelKind::Metadata);
    d.frames(2);
    let premiere = d.harness.state().ui.dock.clone();
    assert_ne!(premiere, filmcraft_ui_egui::dock::workspace("Editing"));
    // Resolve: an unsaved change to the Edit page
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    d.frames(4);
    assert_eq!(d.workspace(), "Edit Page");
    d.click("pageToolbar.Inspector");
    assert!(d.has("panel.Inspector"));
    let resolve = d.harness.state().ui.dock.clone();
    // back to Premiere: its rearranged Editing, not the saved one
    d.exec("settings.setWorkflow", json!({"workflow": "premiere"}));
    d.frames(4);
    assert_eq!(d.workspace(), "Editing");
    assert_eq!(d.harness.state().ui.dock, premiere, "Premiere's unsaved layout survives the round trip");
    // and Resolve again: the Edit page with its Inspector
    d.exec("settings.setWorkflow", json!({"workflow": "resolve"}));
    d.frames(4);
    assert_eq!(d.workspace(), "Edit Page");
    assert_eq!(d.harness.state().ui.dock, resolve, "Resolve's unsaved layout survives too");
    // Deliver: hide the Render Queue, then Ctrl+M's command keeps that layout
    d.click("page.deliver");
    d.click("pageToolbar.RenderQueue");
    assert!(!d.has("panel.RenderQueue"));
    d.menu("mode.export", json!({}));
    assert_eq!(d.workspace(), "Deliver Page");
    assert!(!d.has("panel.RenderQueue"), "already on Deliver: the layout isn't reset");
    // the Import mode is the Media page here
    d.ok("ui.set", json!({"mode": "import"}));
    d.frames(3);
    assert_eq!(d.workspace(), "Media Page");
    assert_eq!(d.harness.state().ui.mode, Mode::Edit);
    assert!(d.has("pageToolbar.MediaBrowser"), "the Media page's toolbar");
}

/// XPS item 15: on a narrow window (the 16" panel at 2×: 960 × 600 points, and smaller) the page
/// buttons shrink into the room between the app's name and the coach's buttons instead of
/// sliding under them; every page stays clickable.
#[test]
fn page_bar_fits_a_narrow_window_without_overlaps() {
    for width in [960.0f32, 800.0] {
        let mut d = Driver::demo(Workflow::Resolve);
        d.harness.set_size(egui::vec2(width, 600.0));
        d.frames(6);
        let overlap = |a: [f64; 4], b: [f64; 4]| a[0] < b[0] + b[2] - 0.5 && b[0] < a[0] + a[2] - 0.5 && a[1] < b[1] + b[3] - 0.5 && b[1] < a[1] + a[3] - 0.5;
        let pages: Vec<String> = d.ids("page.").into_iter().filter(|i| i.matches('.').count() == 1).collect();
        assert!(pages.len() >= 5, "{pages:?}");
        let mut others: Vec<[f64; 4]> = Vec::new();
        for id in ["pageBar.app", "assistant.open", "coach.ask.open", "coach.lessons.open", "pageBar.home"] {
            if d.has(id) {
                others.push(d.rect(id));
            }
        }
        assert!(others.len() >= 4, "the name, the coach's buttons and Home are on screen");
        for id in &pages {
            let r = d.rect(id);
            for o in &others {
                assert!(!overlap(r, *o), "{width}: {id} {r:?} overlaps {o:?}");
            }
        }
        d.click("page.deliver");
        assert!(
            d.workspace().to_lowercase().contains("deliver") || d.workspace().to_lowercase().contains("export"),
            "{width}: Deliver opens: {}",
            d.workspace()
        );
    }
}

/// XPS item 15: a narrow Program monitor (a small window, or the UI at 2×) never draws its
/// duration over the current timecode: it shows where it clears it, else it is left out.
#[test]
fn monitor_duration_never_covers_the_timecode() {
    for (width, height, must_show) in [(1920.0f32, 1080.0f32, true), (800.0, 490.0, false)] {
        let mut d = Driver::demo(Workflow::Resolve);
        d.harness.set_size(egui::vec2(width, height));
        d.frames(6);
        assert!(d.has("program.timecode"), "{width}: the current time is always shown");
        if d.has("program.duration") {
            let (tc, dur) = (d.rect("program.timecode"), d.rect("program.duration"));
            // (the timecode's element is its 110-point edit box; the text itself is narrower)
            assert!(dur[0] >= tc[0] + 60.0, "{width}: duration {dur:?} over the timecode {tc:?}");
        } else {
            assert!(!must_show, "{width}: room for the duration, yet it is missing");
        }
    }
}
