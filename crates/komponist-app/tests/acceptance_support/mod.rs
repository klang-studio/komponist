//! Shared by the `acceptance_*` tests (WP-60): a headless 1920 × 1080 app driven only through
//! AccessKit names and real pointer and key events.

#![allow(dead_code)]

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::App;
use std::path::{Path, PathBuf};

pub const W: f32 = 1920.0;
pub const H: f32 = 1080.0;

/// The app in a harness; `step_dt` is 1/60 s like a real frame.
pub fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

/// A fresh scratch folder for one test, under the cargo target's tmp.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("acceptance-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn press(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

/// A click at `pos`, half a second after the last frames so two clicks are no double-click.
pub fn click_at(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    press(h, pos, PointerButton::Primary, true);
    press(h, pos, PointerButton::Primary, false);
    h.run_steps(2);
}

pub fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        press(h, pos, PointerButton::Primary, true);
        press(h, pos, PointerButton::Primary, false);
        h.step();
    }
    h.run_steps(2);
}

/// Press at `from`, move in 8 steps, release at `to`.
pub fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, PointerButton::Primary, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, PointerButton::Primary, false);
    h.run_steps(3);
}

pub fn key(h: &mut Harness<'_, App>, key: egui::Key) {
    h.key_press(key);
    h.run_steps(2);
}

pub fn ctrl(h: &mut Harness<'_, App>, key: egui::Key) {
    h.key_press_modifiers(Modifiers::COMMAND, key);
    h.run_steps(2);
}

pub fn ctrl_shift(h: &mut Harness<'_, App>, key: egui::Key) {
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, key);
    h.run_steps(2);
}

/// Rect of the first node with `role` whose label starts with `prefix`.
pub fn rect_of(h: &Harness<'_, App>, role: Role, prefix: &str) -> Rect {
    h.get_all_by_role(role)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("no {role:?} named {prefix}…"))
        .rect()
}

/// Clicks the node `role` + exact `label`.
pub fn click(h: &mut Harness<'_, App>, role: Role, label: &str) {
    let rect = h.get_by_role_and_label(role, label).rect();
    click_at(h, rect.center());
}
