//! Fork RS2b: switching Settings ▸ Workflow relabels menus and panel titles live, with command ids
//! and automation ids unchanged.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_engine::Session;
use filmcraft_engine::settings::Workflow;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::menu_items;
use serde_json::json;

fn frames(h: &mut Harness<'_, FilmcraftApp>, n: usize) {
    for _ in 0..n {
        h.step();
        assert!(h.state().ui_error.is_none(), "the app caught a UI panic: {:?}", h.state().ui_error);
    }
}

fn menu_label(h: &Harness<'_, FilmcraftApp>, id: &str) -> Option<String> {
    menu_items(h.state()).into_iter().find(|i| i.id == id).map(|i| i.label)
}

#[test]
fn workflow_switch_relabels_menus_and_panels_live() {
    let mut session = Session::default();
    session.prefs.workflow = Workflow::Premiere;
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(10_000).build_eframe(move |_| app);
    frames(&mut h, 3);
    assert_eq!(menu_label(&h, "sequence.settings").as_deref(), Some("Sequence Settings…"));
    assert!(h.query_by_label("Effect Controls").is_some(), "Premiere: the Effect Controls tab");
    assert!(h.query_by_label("Keyframes").is_none());

    h.state_mut().session.execute("settings.setWorkflow", json!({"workflow": "resolve"})).unwrap();
    frames(&mut h, 3);
    // (the Edit page shows the Inspector only when it is toggled on, fork RS3)
    h.state_mut().show_panel(filmcraft_ui_egui::dock::PanelKind::EffectControls);
    frames(&mut h, 3);
    assert_eq!(menu_label(&h, "sequence.settings").as_deref(), Some("Timeline Settings…"), "same command id, Resolve's word");
    assert!(h.query_by_label("Keyframes").is_some(), "Resolve: Effect Controls is the Keyframes tab (fork RS4)");
    assert!(h.query_by_label("Effect Controls").is_none());

    h.state_mut().session.execute("settings.setWorkflow", json!({"workflow": "premiere"})).unwrap();
    frames(&mut h, 3);
    assert_eq!(menu_label(&h, "sequence.settings").as_deref(), Some("Sequence Settings…"));
    assert!(h.query_by_label("Effect Controls").is_some(), "back to Premiere wording");
}
