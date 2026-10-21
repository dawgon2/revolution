//! Workflow terminology overlay (RESOLVE-UI-PLAN RS2b): while the Resolve workflow is active,
//! `Language::tr` consults `terms-resolve.tsv` (English source string → Resolve wording) before the
//! language catalog, so menus, panel titles and labels use Resolve's words ("Timeline" for
//! "Sequence"). Whole strings only. The file is embedded at compile time and parsed tolerantly:
//! a malformed line is skipped, never an error at run time (a test requires the bundled file to
//! have none).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

use super::Language;

/// One overlay row: the English wording plus optional wording per other interface language.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Term {
    pub english: String,
    /// (language code, wording), e.g. `("es", "…")`
    pub other: Vec<(String, String)>,
}

/// Parse an overlay file: `source<TAB>english[<TAB>code=text…]`. Returns the rows and the
/// malformed lines (skipped).
pub fn parse(text: &str) -> (Vec<(String, Term)>, Vec<String>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        match (cols.next(), cols.next()) {
            (Some(src), Some(en)) if !src.is_empty() && !en.is_empty() => {
                let mut term = Term { english: en.to_string(), other: Vec::new() };
                let mut ok = true;
                for col in cols {
                    match col.split_once('=') {
                        Some((code, text)) if !text.is_empty() && Language::parse(code).is_some() => {
                            term.other.push((code.to_string(), text.to_string()));
                        }
                        _ => ok = false,
                    }
                }
                if ok {
                    rows.push((src.to_string(), term));
                } else {
                    errors.push(format!("line {}: bad `code=text` column", n + 1));
                }
            }
            _ => errors.push(format!("line {}: expected `source<TAB>english`", n + 1)),
        }
    }
    (rows, errors)
}

static RESOLVE: OnceLock<HashMap<String, Term>> = OnceLock::new();

fn resolve_terms() -> &'static HashMap<String, Term> {
    RESOLVE.get_or_init(|| parse(include_str!("terms-resolve.tsv")).0.into_iter().collect())
}

thread_local! {
    /// Whether the Resolve overlay applies to what this thread draws. Per thread, like the
    /// language: `FilmcraftApp::frame` and `menus::menu_items` set it from the workflow preference.
    static RESOLVE_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Turn the Resolve terminology overlay on or off for this thread.
pub fn set_resolve(active: bool) {
    RESOLVE_ACTIVE.with(|c| c.set(active));
}

/// Is the Resolve overlay on for this thread?
pub fn resolve_active() -> bool {
    RESOLVE_ACTIVE.with(Cell::get)
}

/// The overlay's wording for `text` in `lang`, if the overlay is on and has a row that applies
/// (English always has the row's wording; other languages only where the row has their column).
pub fn lookup(lang: Language, text: &str) -> Option<&'static str> {
    if !resolve_active() {
        return None;
    }
    let term = resolve_terms().get(text)?;
    match lang {
        Language::En => Some(term.english.as_str()),
        other => term.other.iter().find(|(code, _)| code == other.code()).map(|(_, t)| t.as_str()),
    }
}

/// The bundled overlay's rows and parse errors (tests).
#[cfg(test)]
pub fn bundled() -> (Vec<(String, Term)>, Vec<String>) {
    parse(include_str!("terms-resolve.tsv"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::catalog::placeholders;

    /// The catalog-only translation (what Premiere shows), bypassing the overlay.
    fn plain(lang: Language, text: &str) -> String {
        lang.catalog().and_then(|c| c.plain(text)).unwrap_or(text).to_string()
    }

    fn app_in(workflow: filmcraft_engine::settings::Workflow) -> crate::FilmcraftApp {
        let mut session = filmcraft_engine::Session::default();
        session.prefs.workflow = workflow;
        crate::FilmcraftApp::new(session)
    }

    /// Source of truth for "a real source string": the Spanish catalog, which the i18n tests
    /// require to hold every `tl!` literal, every registry / settings / effect name shown through
    /// `i18n::t` and every menu label and path (`spanish_translates_dynamic_names`, the `tl!`
    /// coverage test). A key missing there is not a string the interface shows.
    #[test]
    fn the_bundled_overlay_parses_fully_and_has_no_dead_entries() {
        let (rows, errors) = bundled();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(rows.len() > 50, "overlay looks truncated: {} rows", rows.len());
        let mut seen = std::collections::HashSet::new();
        for (src, term) in &rows {
            assert!(Language::Es.has(src), "dead overlay entry (not a source string): {src:?}");
            assert!(seen.insert(src.as_str()), "duplicate overlay entry: {src:?}");
            assert_ne!(&term.english, src, "overlay entry changes nothing: {src:?}");
            let mut want = placeholders(src);
            want.sort_unstable();
            let mut got = placeholders(&term.english);
            got.sort_unstable();
            assert_eq!(want, got, "placeholders differ: {src:?}");
            for (_, other) in &term.other {
                let mut o = placeholders(other);
                o.sort_unstable();
                assert_eq!(want, o, "placeholders differ in {src:?}");
            }
        }
    }

    #[test]
    fn the_parser_skips_malformed_lines() {
        let (rows, errors) = parse("# c\n\nA\tB\nno tab\n\tx\nC\t\nD\tE\tes=F\nG\tH\txx=I\nJ\tK\tes=\n");
        let src: Vec<&str> = rows.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(src, ["A", "D"]);
        assert_eq!(errors.len(), 5, "{errors:?}");
        assert_eq!(rows[1].1.other, [("es".to_string(), "F".to_string())]);
    }

    #[test]
    fn premiere_wording_is_the_plain_catalog() {
        set_resolve(false);
        for lang in Language::ALL {
            for (src, _) in bundled().0 {
                assert_eq!(lang.tr(&src), plain(lang, &src), "{lang:?}: {src}");
            }
        }
    }

    #[test]
    fn resolve_wording_applies_in_english_and_falls_back_elsewhere() {
        set_resolve(true);
        assert_eq!(Language::En.tr("Sequence Settings…"), "Timeline Settings…");
        assert_eq!(Language::En.tr("Effect Controls"), "Keyframes");
        assert_eq!(Language::En.tr("Nest…"), "New Compound Clip…");
        // no Spanish wording in the overlay: the normal Spanish translation
        assert_eq!(Language::Es.tr("Sequence Settings…"), plain(Language::Es, "Sequence Settings…"));
        // strings the overlay doesn't know are untouched
        assert_eq!(Language::En.tr("Save"), "Save");
        assert_eq!(Language::Es.tr("Save"), plain(Language::Es, "Save"));
        set_resolve(false);
        assert_eq!(Language::En.tr("Sequence Settings…"), "Sequence Settings…");
    }

    #[test]
    fn premiere_menu_labels_are_byte_identical_to_the_catalog() {
        for lang in [Language::En, Language::Es, Language::ZhCn] {
            let mut app = app_in(filmcraft_engine::settings::Workflow::Premiere);
            app.ui.language = lang;
            // a stale flag from earlier on this thread must not leak into Premiere
            set_resolve(true);
            let shown = crate::menus::menu_items(&app);
            assert!(!resolve_active());
            let base = crate::menus::menu_items_for(&app.session);
            assert_eq!(shown.len(), base.len());
            // (`menu_items` itself rewrites a few dynamic labels, e.g. the workspace list, so the
            // comparison covers every item whose registry label the overlay has a row for)
            let keys: std::collections::HashSet<String> = bundled().0.into_iter().map(|(k, _)| k).collect();
            let mut checked = 0;
            for (it, b) in shown.iter().zip(&base) {
                if keys.contains(&b.label) {
                    assert_eq!(it.label, plain(lang, &b.label), "{lang:?}: {}", it.id);
                    checked += 1;
                }
            }
            assert!(checked > 10, "only {checked} overlay-covered menu items");
        }
    }

    #[test]
    fn resolve_menu_labels_use_resolve_words() {
        let app = app_in(filmcraft_engine::settings::Workflow::Resolve);
        let items = crate::menus::menu_items(&app);
        let label = |id: &str| items.iter().find(|i| i.id == id).map(|i| i.label.clone());
        assert_eq!(label("sequence.settings").as_deref(), Some("Timeline Settings…"));
        assert_eq!(label("clip.nest").as_deref(), Some("New Compound Clip…"));
        set_resolve(false);
    }
}
