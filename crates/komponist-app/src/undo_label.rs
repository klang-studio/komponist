//! Undo labels as message ids (WP-73, i18n evaluation §7.7 (internal notes), `docs/I18N.md`).
//!
//! The model keeps a step's label as an opaque `String` and stays free of texts. The app passes
//! a **message id** from `locales/*/undo.ftl` (`"undo-quantize"`) when it applies an edit or
//! begins a gesture, and resolves it only where it shows a label ([`text`], [`App::undo_text`]),
//! so a language switch relabels the steps already made.
//!
//! - A static label is the id itself: `"undo-quantize"`.
//! - A label with a name in it carries the name after a unit separator (U+001F):
//!   [`param`], [`device_param`] and [`plugin_state`] build `"undo-param\u{1f}Cutoff"`.
//! - Anything else is content or not yet migrated and shows as it is: a take's name
//!   ("Take 1"), the labels the views still send (WP-74).

use komponist_i18n::Lang;

use crate::app::App;

/// Separates the id from the name inside a label.
const SEP: char = '\u{1f}';

fn with_arg(id: &str, arg: &str) -> String {
    format!("{id}{SEP}{arg}")
}

/// A card parameter changed: "Cutoff ändern" / "Change Cutoff".
pub fn param(name: &str) -> String {
    with_arg("undo-param", name)
}

/// A parameter without a name: "hexa: Parameter ändern".
pub fn device_param(plugin: &str) -> String {
    with_arg("undo-param-unnamed", plugin)
}

/// A state the plugin reports on its own (its GUI): "Surge XT: Zustand ändern".
pub fn plugin_state(plugin: &str) -> String {
    with_arg("undo-plugin-state", plugin)
}

/// The text of a stored `label` in `lang` (module docs).
pub fn text(lang: Lang, label: &str) -> String {
    if let Some((id, arg)) = label.split_once(SEP) {
        match id {
            "undo-param" => return lang.undo_param(arg),
            "undo-param-unnamed" => return lang.undo_param_unnamed(arg),
            "undo-plugin-state" => return lang.undo_plugin_state(arg),
            _ => {}
        }
    }
    lang.by_id(label)
        .map_or_else(|| label.to_owned(), str::to_owned)
}

impl App {
    /// The label of the step Undo would take back, in the UI language.
    pub fn undo_text(&self) -> Option<String> {
        self.doc().undo_label().map(|l| text(self.lang, l))
    }

    /// The label of the step Redo would apply again, in the UI language.
    pub fn redo_text(&self) -> Option<String> {
        self.doc().redo_label().map(|l| text(self.lang, l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_ids_resolve_and_content_stays() {
        assert_eq!(text(Lang::De, "undo-quantize"), "Quantisieren");
        assert_eq!(text(Lang::En, "undo-quantize"), "Quantize");
        assert_eq!(text(Lang::En, "Take 1"), "Take 1");
        assert_eq!(text(Lang::En, "Lautstärke"), "Lautstärke");
        assert_eq!(text(Lang::De, ""), "");
    }

    #[test]
    fn labels_with_names() {
        assert_eq!(text(Lang::De, &param("Cutoff")), "Cutoff ändern");
        assert_eq!(text(Lang::En, &param("Cutoff")), "Change Cutoff");
        assert_eq!(
            text(Lang::De, &plugin_state("Surge XT")),
            "Surge XT: Zustand ändern"
        );
        assert_eq!(
            text(Lang::En, &device_param("hexa")),
            "hexa: Change Parameter"
        );
    }

    /// Every `"undo-…"` literal in the sources is a message (a typo would show the raw id).
    #[test]
    fn every_id_in_the_sources_exists() {
        fn visit(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for part in text.split('"').skip(1).step_by(2) {
                        if part.starts_with("undo-")
                            && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                        {
                            found.push((path.display().to_string(), part.to_owned()));
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        assert!(found.len() > 40, "{}", found.len());
        for (file, id) in found {
            let known = Lang::De.by_id(&id).is_some()
                || ["undo-param", "undo-param-unnamed", "undo-plugin-state"].contains(&id.as_str());
            assert!(known, "{file}: unknown undo id {id}");
        }
    }
}
