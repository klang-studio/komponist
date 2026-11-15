//! The inspector (WP-43) through `egui_kittest`: the clip section (fields, loop, takes after loop
//! recording, step clips, the take being recorded), the track section (colour, instrument,
//! input, channel), the devices list (status, bypass, open, "zu spät"), the compact channel strip,
//! the project with nothing selected, and snapshots next to the S01 inspector.

use crate::fake_host;

use std::path::PathBuf;

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::{DockTab, Panel};
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_i18n::{Lang, de};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipId, Project, Tick, TrackId, edits};

use fake_host::FakeHost;

const W: f32 = 1920.0;
const H: f32 = 1080.0;
const BAR: i64 = 3840;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    h
}

/// The demo as S01 shows it: Lead A loops over 2.0.0.
fn s01_project() -> Project {
    let p = demo_project();
    let lead_a = p.tracks[2].clips[0].id;
    edits::set_clip_loop(&p, lead_a, Some(Tick(2 * BAR)), Tick(2 * BAR)).unwrap()
}

/// S01: Lead A selected and open in the editor.
fn s01() -> (App, ClipId, TrackId) {
    let mut app = App::headless(s01_project()).with_device_host(Box::new(FakeHost::new()));
    let clip = app.project().tracks[2].clips[0].id;
    let track = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    (app, clip, track)
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

fn section<'h>(h: &'h Harness<'_, App>, title: &'h str) -> Node<'h> {
    h.get_by_role_and_label(Role::Group, title)
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

fn settle(h: &mut Harness<'_, App>) {
    h.run_steps(30);
}

fn click_with(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    press(h, pos, true, modifiers);
    press(h, pos, false, modifiers);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    click_with(h, pos, Modifiers::NONE);
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        press(h, pos, true, Modifiers::NONE);
        press(h, pos, false, Modifiers::NONE);
        h.step();
    }
    h.run_steps(2);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    settle(h);
    h.hover_at(from);
    h.step();
    press(h, from, true, Modifiers::NONE);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, false, Modifiers::NONE);
    h.run_steps(3);
}

/// The rect of node `role` / `label` in section `title`.
fn in_section(h: &Harness<'_, App>, title: &str, role: Role, label: &str) -> egui::Rect {
    section(h, title).get_by_role_and_label(role, label).rect()
}

fn click_in(h: &mut Harness<'_, App>, title: &str, role: Role, label: &str) {
    let at = in_section(h, title, role, label).center();
    click(h, at);
}

fn double_click_in(h: &mut Harness<'_, App>, title: &str, role: Role, label: &str) {
    let at = in_section(h, title, role, label).center();
    double_click(h, at);
}

fn value_in(h: &Harness<'_, App>, title: &str, role: Role, label: &str) -> String {
    section(h, title)
        .get_by_role_and_label(role, label)
        .value()
        .unwrap_or_default()
}

/// Opens the text entry of `label` in `title` with a double-click, types `text`, Enter.
fn enter(h: &mut Harness<'_, App>, title: &str, role: Role, label: &str, text: &str) {
    let at = in_section(h, title, role, label).center();
    double_click(h, at);
    let entry = format!("{label} Eingabe");
    h.get_by_role_and_label(Role::TextInput, &entry)
        .type_text(text);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
}

/// Chooses `option` in dropdown `label` of section `title`.
fn choose(h: &mut Harness<'_, App>, title: &str, label: &str, option: &str) {
    let at = in_section(h, title, Role::ComboBox, label).center();
    click(h, at);
    let item = h.get_by_role_and_label(Role::ListBoxOption, option).rect();
    click(h, item.center());
}

fn clip_of(app: &App, id: ClipId) -> komponist_model::Clip {
    (**app.project().find_clip(id).unwrap().1).clone()
}

// ------------------------------------------------------------------------------------------------
// Clip section (S01): name, Start, Länge, Loop, Loop-Länge, Transponieren, Velocity.

#[test]
fn the_clip_fields_show_and_change_the_clip() {
    let (app, clip, _) = s01();
    let mut h = harness(app);
    let c = de::INSPECTOR_SECTION_CLIP;
    section(&h, c).get_by_role_and_label(Role::Label, "Lead A");
    section(&h, c).get_by_role_and_label(Role::Label, de::INSPECTOR_CLIP_MIDI);
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_START),
        "9.1.1"
    );
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_LENGTH),
        "2.0.0"
    );
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_LOOP_LENGTH),
        "2.0.0"
    );
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_TRANSPOSE),
        "0 st"
    );
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_VELOCITY),
        "+0 %"
    );
    let looped = section(&h, c).get_by_role_and_label(Role::CheckBox, de::INSPECTOR_CLIP_LOOP);
    assert_eq!(looped.accesskit_node().toggled(), Some(Toggled::True));

    // Start: drag up 16 px = one bar later, one undo step.
    let start = in_section(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_START);
    drag(&mut h, start.center(), start.center() - vec2(0.0, 16.0));
    assert_eq!(clip_of(h.state(), clip).start, Tick(9 * BAR));
    assert_eq!(h.state().undo_text().as_deref(), Some("Clip ändern"));
    // Länge by text entry; the clip keeps its loop.
    enter(
        &mut h,
        c,
        Role::SpinButton,
        de::INSPECTOR_CLIP_LENGTH,
        "4.0.0",
    );
    let now = clip_of(h.state(), clip);
    assert_eq!(
        (now.length, now.loop_length),
        (Tick(4 * BAR), Some(Tick(2 * BAR)))
    );
    // Loop-Länge.
    enter(
        &mut h,
        c,
        Role::SpinButton,
        de::INSPECTOR_CLIP_LOOP_LENGTH,
        "1.0.0",
    );
    assert_eq!(clip_of(h.state(), clip).loop_length, Some(Tick(BAR)));
    // Transponieren and Velocity.
    enter(
        &mut h,
        c,
        Role::SpinButton,
        de::INSPECTOR_CLIP_TRANSPOSE,
        "-5",
    );
    assert_eq!(clip_of(h.state(), clip).transpose, -5);
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_TRANSPOSE),
        "\u{2212}5 st"
    );
    enter(
        &mut h,
        c,
        Role::SpinButton,
        de::INSPECTOR_CLIP_VELOCITY,
        "20",
    );
    assert_eq!(clip_of(h.state(), clip).velocity_pct, 20);
    let velocity = in_section(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_VELOCITY);
    click_with(&mut h, velocity.center(), Modifiers::ALT);
    assert_eq!(clip_of(h.state(), clip).velocity_pct, 0);
    // Loop off: Loop-Länge is disabled; on again loops the whole clip.
    click_in(&mut h, c, Role::CheckBox, de::INSPECTOR_CLIP_LOOP);
    assert_eq!(clip_of(h.state(), clip).loop_length, None);
    let ll = section(&h, c).get_by_role_and_label(Role::SpinButton, de::INSPECTOR_CLIP_LOOP_LENGTH);
    assert!(ll.accesskit_node().is_disabled());
    click_in(&mut h, c, Role::CheckBox, de::INSPECTOR_CLIP_LOOP);
    assert_eq!(clip_of(h.state(), clip).loop_length, Some(Tick(4 * BAR)));
    // Rename by double-click on the name.
    double_click_in(&mut h, c, Role::Label, "Lead A");
    h.get_by_role_and_label(Role::TextInput, de::INSPECTOR_RENAME)
        .type_text("Hook");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(clip_of(h.state(), clip).name, "Hook");
}

/// Types `text` into the open inspector name field (empty: delete the selected text) and presses `key`.
fn type_and_end(h: &mut Harness<'_, App>, text: &str, key: egui::Key) {
    h.get_by_role_and_label(Role::TextInput, de::INSPECTOR_RENAME);
    if text.is_empty() {
        h.key_press(egui::Key::Backspace);
    } else {
        h.event(Event::Text(text.into()));
    }
    h.run_steps(1);
    h.key_press(key);
    h.run_steps(3);
}

fn f2(h: &mut Harness<'_, App>) {
    h.key_press(egui::Key::F2);
    h.run_steps(3);
}

#[test]
fn f2_renames_the_selected_clip_in_the_inspector_by_the_same_rules() {
    let (app, clip, _) = s01();
    let mut h = harness(app);
    f2(&mut h);
    type_and_end(&mut h, "Hook", egui::Key::Enter);
    assert_eq!(clip_of(h.state(), clip).name, "Hook");
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
    assert_eq!(clip_of(h.state(), clip).name, "Lead A", "one undo step");
    f2(&mut h);
    type_and_end(&mut h, "Weg", egui::Key::Escape);
    assert_eq!(clip_of(h.state(), clip).name, "Lead A", "Esc discards");
    f2(&mut h);
    type_and_end(&mut h, "", egui::Key::Enter);
    assert_eq!(clip_of(h.state(), clip).name, "Lead A", "empty keeps it");
    // Nothing is left open.
    assert!(
        h.query_by_role_and_label(Role::TextInput, de::INSPECTOR_RENAME)
            .is_none()
    );
}

#[test]
fn f2_on_a_clip_shows_a_hidden_inspector() {
    let (mut app, clip, _) = s01();
    app.dispatch(Action::Ui(UiAction::TogglePanel(Panel::Inspector)));
    let mut h = harness(app);
    assert!(!h.state().ui().layout.inspector_open);
    f2(&mut h);
    assert!(h.state().ui().layout.inspector_open);
    type_and_end(&mut h, "Hook", egui::Key::Enter);
    assert_eq!(clip_of(h.state(), clip).name, "Hook");
}

#[test]
fn f2_renames_the_track_here_when_the_arrangement_and_the_mixer_are_hidden() {
    let (mut app, _, track) = s01();
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
        clips: Vec::new(),
        mode: SelectMode::Replace,
    })));
    // Dock maximised on the editor tab: no arrangement, no mixer.
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Editor)));
    app.dispatch(Action::Ui(UiAction::ToggleDockMaximized));
    let mut h = harness(app);
    assert!(h.state().ui().layout.dock_maximized);
    f2(&mut h);
    type_and_end(&mut h, "Melodie", egui::Key::Enter);
    let p = h.state().project();
    assert_eq!(p.track(track).unwrap().name, "Melodie");
}

#[test]
fn the_takes_of_a_loop_recording_are_listed_and_selectable() {
    let (app, clip, _) = s01();
    let mut app = app;
    // Two more takes, as loop recording makes them.
    let p = edits::add_take(app.project(), clip, "Take 2".into())
        .unwrap()
        .0;
    let p = edits::add_take(&p, clip, "Take 3".into()).unwrap().0;
    let mut app2 = App::headless(p).with_device_host(Box::new(FakeHost::new()));
    app2.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    app = app2;
    let mut h = harness(app);
    let list = section(&h, de::INSPECTOR_SECTION_CLIP)
        .get_by_role_and_label(Role::ListBox, de::INSPECTOR_CLIP_TAKES);
    let rows: Vec<(String, Option<bool>)> = list
        .get_all_by_role(Role::ListBoxOption)
        .map(|n| {
            let n = n.accesskit_node();
            (n.label().unwrap_or_default(), n.is_selected())
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("Take 1".to_owned(), Some(false)),
            ("Take 2".to_owned(), Some(false)),
            ("Take 3".to_owned(), Some(true)),
        ]
    );
    // The first take: the hand-drawn one, unnamed in the model, shows as "Take 1".
    let first = list
        .get_all_by_role(Role::ListBoxOption)
        .next()
        .unwrap()
        .rect();
    click(&mut h, first.center());
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Edit(EditAction::SelectTake { clip, take: 0 }))
    );
    let komponist_model::ClipContent::Midi(m) = &clip_of(h.state(), clip).content else {
        panic!()
    };
    assert_eq!(m.active_take, 0);
}

#[test]
fn a_step_clip_shows_pattern_resolution_swing_and_always_loops() {
    let mut app = App::headless(demo_project());
    let beat_b = app.project().tracks[0].clips[2].id;
    app.dispatch(Action::Ui(UiAction::OpenClip(beat_b)));
    let mut h = harness(app);
    let c = de::INSPECTOR_SECTION_CLIP;
    section(&h, c).get_by_role_and_label(Role::Label, de::INSPECTOR_CLIP_STEP);
    assert_eq!(
        value_in(&h, c, Role::SpinButton, de::INSPECTOR_CLIP_PATTERN),
        "16 Steps"
    );
    assert_eq!(
        value_in(&h, c, Role::ComboBox, de::INSPECTOR_CLIP_RESOLUTION),
        "1/16"
    );
    let looped = section(&h, c).get_by_role_and_label(Role::CheckBox, de::INSPECTOR_CLIP_LOOP);
    assert_eq!(looped.accesskit_node().toggled(), Some(Toggled::True));
    assert!(looped.accesskit_node().is_disabled());
    enter(
        &mut h,
        c,
        Role::SpinButton,
        de::INSPECTOR_CLIP_PATTERN,
        "32",
    );
    enter(&mut h, c, Role::SpinButton, de::INSPECTOR_CLIP_SWING, "25");
    choose(&mut h, c, de::INSPECTOR_CLIP_RESOLUTION, "1/8");
    let komponist_model::ClipContent::Step(s) = &clip_of(h.state(), beat_b).content else {
        panic!()
    };
    assert_eq!(s.length_steps, 32);
    assert!((s.swing - 0.25).abs() < 1e-6);
    assert_eq!(s.resolution, komponist_model::StepResolution::Eighth);
}

#[test]
fn the_take_being_recorded_shows_while_recording() {
    let mut app = App::headless(komponist_model::fixtures::empty_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::ToggleCountIn,
    ));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    h.key_press(egui::Key::R);
    h.step();
    assert!(h.state_mut().engine_mut().run_held(48_000, &[], 0));
    h.run_steps(3);
    assert!(h.state().recording().is_some());
    let c = de::INSPECTOR_SECTION_CLIP;
    section(&h, c).get_by_role_and_label(Role::Label, de::INSPECTOR_CLIP_RECORDING);
    section(&h, c).get_by_role_and_label(Role::Label, "Take 1");
    section(&h, c).get_by_role_and_label(Role::Label, "Noten 0");
    section(&h, c).get_by_role_and_label(Role::Label, "Quantisieren aus");
}

// ------------------------------------------------------------------------------------------------
// Track section: colour swatches, Instrument, Eingang, Kanal, rename.

#[test]
fn the_track_section_sets_colour_instrument_input_and_channel() {
    let (app, _, lead) = s01();
    let mut h = harness(app);
    let t = de::INSPECTOR_SECTION_TRACK;
    section(&h, t).get_by_role_and_label(Role::Label, "3 / 6");
    let green = Lang::De.inspector_swatch("Grün");
    let green = section(&h, t).get_by_role_and_label(Role::RadioButton, &green);
    assert_eq!(green.accesskit_node().toggled(), Some(Toggled::True));
    click_in(
        &mut h,
        t,
        Role::RadioButton,
        &Lang::De.inspector_swatch("Rot"),
    );
    let track = |app: &App| (**app.project().track(lead).unwrap()).clone();
    assert_eq!(track(h.state()).hue, komponist_model::Hue::RED);

    assert_eq!(
        value_in(&h, t, Role::ComboBox, de::INSPECTOR_TRACK_INSTRUMENT),
        "hexa"
    );
    choose(&mut h, t, de::INSPECTOR_TRACK_INSTRUMENT, "Surge XT");
    assert_eq!(track(h.state()).channel.devices[0].plugin.name, "Surge XT");
    assert_eq!(
        value_in(&h, t, Role::ComboBox, de::INSPECTOR_TRACK_INPUT),
        de::INSPECTOR_TRACK_ALL_INPUTS
    );
    choose(&mut h, t, de::INSPECTOR_TRACK_MIDI_CHANNEL, "3");
    assert_eq!(
        track(h.state()).midi_channel,
        komponist_model::MidiChannelSel::Channel(2)
    );
    choose(
        &mut h,
        t,
        de::INSPECTOR_TRACK_MIDI_CHANNEL,
        de::INSPECTOR_TRACK_OMNI,
    );
    assert_eq!(
        track(h.state()).midi_channel,
        komponist_model::MidiChannelSel::Omni
    );
    // Rename.
    double_click_in(&mut h, t, Role::Label, "Lead");
    h.get_by_role_and_label(Role::TextInput, de::INSPECTOR_RENAME)
        .type_text("Melodie");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(track(h.state()).name, "Melodie");
}

// ------------------------------------------------------------------------------------------------
// Devices: status per device, bypass, open, "zu spät", "+ Effekt".

#[test]
fn the_devices_list_shows_status_and_opens_bypasses_and_flags_late_blocks() {
    let p = s01_project();
    let pad = &p.tracks[3];
    let [surge, eq, hall] = [0, 1, 2].map(|i| pad.channel.devices[i].id);
    let pad_id = pad.id;
    let host = FakeHost::new()
        .status(eq, komponist_plugins::DeviceStatus::Loading)
        .status(hall, komponist_plugins::DeviceStatus::Missing);
    let late = host.late.clone();
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![pad_id],
        mode: SelectMode::Replace,
    })));
    let mut h = harness(app);
    let d = de::INSPECTOR_SECTION_DEVICES;
    let row = |name: &str| Lang::De.inspector_device(name);
    assert_eq!(value_in(&h, d, Role::Button, &row("Surge XT")), "CLAP");
    assert_eq!(
        value_in(&h, d, Role::Button, &row("LSP Para EQ x8")),
        "CLAP"
    );
    assert_eq!(
        value_in(&h, d, Role::Button, &row("Dragonfly Hall Reverb")),
        de::INSPECTOR_DEVICE_MISSING
    );
    // A click opens the window; the power square switches bypass.
    click_in(&mut h, d, Role::Button, &row("Surge XT"));
    assert!(
        h.state()
            .ui()
            .plugin_windows
            .iter()
            .any(|w| w.device == surge)
    );
    let power = section(&h, d)
        .get_all_by_role(Role::CheckBox)
        .next()
        .unwrap()
        .rect();
    click(&mut h, power.center());
    assert!(h.state().project().find_device(surge).unwrap().2.bypass);
    // Late blocks: "zu spät" for a while, then the format again.
    late.lock().unwrap().insert(surge, 0);
    h.run_steps(2);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetDeviceBypass {
            device: surge,
            bypass: false,
        }));
    late.lock().unwrap().insert(surge, 3);
    h.run_steps(2);
    assert_eq!(
        value_in(&h, d, Role::Button, &row("Surge XT")),
        de::LATE_LABEL
    );
    h.run_steps(140);
    assert_eq!(value_in(&h, d, Role::Button, &row("Surge XT")), "CLAP");
    // "+ Effekt": the add menu below the link, for the inspected track; one undo step.
    let before = h
        .state()
        .project()
        .track(pad_id)
        .unwrap()
        .channel
        .devices
        .len();
    click_in(&mut h, d, Role::Link, de::INSPECTOR_DEVICES_ADD_EFFECT);
    h.run_steps(3);
    let search = de::DEVICES_ADD_SEARCH;
    let field = h.get_by_role_and_label(Role::TextInput, search);
    assert!(field.accesskit_node().is_focused());
    h.event(egui::Event::Text("delay".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(h.query_by_role_and_label(Role::TextInput, search).is_none());
    let devices = &h.state().project().track(pad_id).unwrap().channel.devices;
    assert_eq!(devices.len(), before + 1);
    assert!(
        devices
            .last()
            .unwrap()
            .plugin
            .name
            .to_lowercase()
            .contains("delay")
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        h.state()
            .project()
            .track(pad_id)
            .unwrap()
            .channel
            .devices
            .len(),
        before
    );
}

// ------------------------------------------------------------------------------------------------
// Compact channel strip: fader (one undo step), pan, M S, "im Mixer zeigen".

#[test]
fn the_compact_channel_strip_controls_the_track() {
    let (app, _, lead) = s01();
    let mut h = harness(app);
    let k = de::INSPECTOR_SECTION_CHANNEL;
    let fader = in_section(&h, k, Role::Slider, de::INSPECTOR_CHANNEL_VOLUME);
    drag(&mut h, fader.center(), fader.center() + vec2(0.0, 30.0));
    let db = h.state().project().track(lead).unwrap().channel.volume_db;
    assert!(db < -3.5, "{db}");
    assert_eq!(h.state().undo_text().as_deref(), Some("Lautstärke"));
    section(&h, k).get_by_role_and_label(
        Role::Label,
        &format!("Lautstärke {}", komponist_ui::widgets::fader::db_text(db)),
    );
    let knob = in_section(&h, k, Role::Slider, de::INSPECTOR_CHANNEL_PAN);
    click_with(&mut h, knob.center(), Modifiers::NONE);
    drag(&mut h, knob.center(), knob.center() + vec2(0.0, 40.0));
    assert!(h.state().project().track(lead).unwrap().channel.pan < -0.2);
    click_in(&mut h, k, Role::Button, "Mute");
    assert!(h.state().project().track(lead).unwrap().channel.mute);
    click_in(&mut h, k, Role::Button, "Solo");
    assert!(h.state().project().track(lead).unwrap().channel.solo);
    // "im Mixer zeigen": the dock's mixer tab; the channel section steps aside.
    click_in(&mut h, k, Role::Link, de::INSPECTOR_CHANNEL_SHOW_IN_MIXER);
    assert_eq!(
        h.state().ui().layout.dock_tab,
        komponist_app::ui_state::DockTab::Mixer
    );
    assert!(h.query_by_role_and_label(Role::Group, k).is_none());
}

// ------------------------------------------------------------------------------------------------
// Nothing selected: the project.

#[test]
fn with_nothing_selected_the_project_shows() {
    let mut h = harness(App::headless(demo_project()));
    let p = de::INSPECTOR_SECTION_PROJECT;
    section(&h, p).get_by_role_and_label(Role::Label, "Night Drive");
    assert_eq!(
        value_in(&h, p, Role::SpinButton, de::INSPECTOR_PROJECT_TEMPO),
        "124.00 BPM"
    );
    section(&h, p).get_by_role_and_label(Role::Label, "Taktart 4/4");
    section(&h, p).get_by_role_and_label(Role::Label, "Abtastrate 48 kHz");
    section(&h, p).get_by_role_and_label(Role::Label, "Spuren 6");
    section(&h, p).get_by_role_and_label(Role::Label, "Länge 16 Takte");
    section(&h, de::INSPECTOR_SECTION_SELECTION)
        .get_by_role_and_label(Role::Label, de::INSPECTOR_NOTHING_SELECTED);
    enter(
        &mut h,
        p,
        Role::SpinButton,
        de::INSPECTOR_PROJECT_TEMPO,
        "128",
    );
    assert_eq!(h.state().project().tempo.bpm, 128.0);
}

#[test]
fn snapshots_next_to_the_s01_inspector() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let (app, ..) = s01();
        let mut h = harness_at(app, ppp);
        h.run_steps(4);
        mask_cpu(&mut h);
        h.snapshot(format!("inspector-s01-{ppp}x"));
        results.extend_harness(&mut h);
    }
    // S03: the step clip Beat B.
    let mut app = App::headless(demo_project()).with_device_host(Box::new(FakeHost::new()));
    let beat_b = app.project().tracks[0].clips[2].id;
    app.dispatch(Action::Ui(UiAction::OpenClip(beat_b)));
    let mut h = harness_at(app, 1.0);
    h.run_steps(4);
    mask_cpu(&mut h);
    h.snapshot("inspector-s03-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
