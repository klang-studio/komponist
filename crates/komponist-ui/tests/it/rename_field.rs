//! The shared inline rename field (#37): select-all on start, Enter commits the trimmed name,
//! Esc cancels, an empty, blank or unchanged name cancels, the AccessKit name.

use egui::accesskit::Role;
use egui::{Id, Key, Rect, Ui, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use komponist_tokens::typography;
use komponist_ui::widgets::rename_field::{RenameField, RenameOutcome, RenameState};
use komponist_ui::{fonts, snapshot};

const OLD: &str = "Bass";

/// What the host of the field keeps: the open rename and the last non-`Editing` outcome.
struct Host {
    rename: Option<RenameState>,
    done: Option<RenameOutcome>,
}

fn harness() -> Harness<'static, Host> {
    let host = Host {
        rename: Some(RenameState::new(OLD)),
        done: None,
    };
    let mut h = snapshot::builder::<Host>(1.0)
        .with_size(vec2(240.0, 80.0))
        .build_ui_state(
            |ui: &mut Ui, host: &mut Host| {
                let Some(state) = host.rename.as_mut() else {
                    return;
                };
                let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(160.0, 20.0));
                let font = fonts::font_id(&typography::TRACK);
                let outcome =
                    RenameField::new(state, Id::unique("rename-test"), "Umbenennen", OLD, font)
                        .show(ui, rect);
                if outcome != RenameOutcome::Editing {
                    host.rename = None;
                    host.done = Some(outcome);
                }
            },
            host,
        );
    // Frame 1 creates the field, frame 2 takes the focus with everything selected.
    h.run();
    h
}

/// Types `text` over the preselected name, then presses `key`.
fn type_and_press(text: &str, key: Key) -> Host {
    let mut h = harness();
    if !text.is_empty() {
        h.get_by_role_and_label(Role::TextInput, "Umbenennen")
            .type_text(text);
        h.run();
    }
    h.key_press(key);
    h.run();
    h.into_state()
}

#[test]
fn the_field_is_a_named_text_input_with_the_name_selected() {
    let h = harness();
    h.get_by_role_and_label(Role::TextInput, "Umbenennen");
    assert_eq!(h.state().rename.as_ref().map(RenameState::text), Some(OLD));
}

#[test]
fn typing_replaces_the_selected_name_and_enter_commits() {
    let host = type_and_press("Lead", Key::Enter);
    assert_eq!(host.done, Some(RenameOutcome::Commit("Lead".into())));
    assert!(host.rename.is_none());
}

#[test]
fn the_committed_name_is_trimmed() {
    let host = type_and_press("  Lead  ", Key::Enter);
    assert_eq!(host.done, Some(RenameOutcome::Commit("Lead".into())));
}

#[test]
fn escape_cancels_even_with_a_new_name() {
    let host = type_and_press("Lead", Key::Escape);
    assert_eq!(host.done, Some(RenameOutcome::Cancel));
    assert!(host.rename.is_none());
}

#[test]
fn an_unchanged_name_keeps_the_old_one() {
    assert_eq!(
        type_and_press("", Key::Enter).done,
        Some(RenameOutcome::Cancel)
    );
    assert_eq!(
        type_and_press(OLD, Key::Enter).done,
        Some(RenameOutcome::Cancel)
    );
    // Unchanged after trimming.
    assert_eq!(
        type_and_press(" Bass ", Key::Enter).done,
        Some(RenameOutcome::Cancel)
    );
}

#[test]
fn an_empty_or_blank_name_keeps_the_old_one() {
    let mut h = harness();
    h.get_by_role_and_label(Role::TextInput, "Umbenennen")
        .type_text("   ");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().done, Some(RenameOutcome::Cancel));
}

#[test]
fn the_field_keeps_editing_until_it_loses_the_focus() {
    let mut h = harness();
    h.get_by_role_and_label(Role::TextInput, "Umbenennen")
        .type_text("Le");
    h.run();
    assert!(h.state().done.is_none());
    assert_eq!(h.state().rename.as_ref().map(RenameState::text), Some("Le"));
}
