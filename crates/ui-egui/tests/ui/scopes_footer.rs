//! The Lumetri Scopes footer in narrow panels and at 2× UI scale (XPS item 15: at 2× the footer
//! squeezed "Rec. 709" against the wrench and the Clamp Signal checkbox).

use egui::{Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, panels::scopes, theme};
use serde_json::json;

fn harness(width: f32, scale: f32) -> Harness<'static, FilmcraftApp> {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut h = Harness::builder().with_size(vec2(width, 300.0)).with_pixels_per_point(scale).build_ui_state(
        |ui, app: &mut FilmcraftApp| {
            app.auto.begin_frame();
            scopes::show(app, ui, ui.max_rect());
        },
        FilmcraftApp::new(session),
    );
    theme::install(&h.ctx, &h.state().tokens);
    h.run_steps(4);
    h
}

fn element(h: &Harness<'_, FilmcraftApp>, id: &str) -> Rect {
    let e = h.state().auto.elements.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("missing {id}"));
    let [x, y, w, h] = e.rect;
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

#[test]
fn scopes_footer_fits_narrow_panels_at_any_scale() {
    let mut failures = Vec::new();
    for width in [500.0, 320.0, 260.0, 220.0, 160.0] {
        for scale in [1.0, 2.0] {
            let h = harness(width, scale);
            let ids = ["scopes.space", "scopes.wrench", "scopes.clamp", "scopes.scale"];
            let rects = ids.map(|id| element(&h, id));
            for (i, a) in rects.iter().enumerate() {
                if a.left() < 0.0 || a.right() > width {
                    failures.push(format!("{width} {scale}x: {} outside the panel: {a:?}", ids[i]));
                }
                for (j, b) in rects.iter().enumerate().skip(i + 1) {
                    let x = a.intersect(*b);
                    if x.width() > 0.0 && x.height() > 0.0 {
                        failures.push(format!("{width} {scale}x: {} {a:?} overlaps {} {b:?}", ids[i], ids[j]));
                    }
                }
            }
            // the colour space is read in full wherever the narrowed scale box and the bare checkbox leave it room
            let full = h.ctx.fonts_mut(|f| f.layout_no_wrap("Rec. 709".into(), filmcraft_ui_egui::theme::Tokens::ui(12.0), egui::Color32::WHITE).size().x);
            if width >= 220.0 && rects[0].width() + 0.5 < full {
                failures.push(format!("{width} {scale}x: \"Rec. 709\" cut to {:.1} of {full:.1} points", rects[0].width()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
