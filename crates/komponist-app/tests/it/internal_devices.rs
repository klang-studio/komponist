//! The internal effects (WP-85: EQ, Delay, Hall, Limiter) in the app: the browser lists them as
//! INTERN, a double-click adds one, the card shows the layout and the parameters, a knob turn is
//! saved in the project file, the mixer insert jumps to the device, and snapshots of the frames.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::{BrowserSegment, DockTab};
use komponist_app::views::devices::model::names;
use komponist_app::{Action, App, PluginAction, ProjectAction, UiAction};
use komponist_builtin::Internal;
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, PluginFormat, Project, TrackId};
use komponist_plugins::DeviceStatus;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn harness(app: App) -> Harness<'static, App> {
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

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        press(h, pos, true);
        press(h, pos, false);
        h.step();
    }
    h.run_steps(2);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, false);
    h.run_steps(3);
}

fn select_track(app: &mut App, track: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
}

/// A project with one pisynth track and the dock on „Geräte“.
fn app_with_track() -> (App, TrackId) {
    let mut app = App::headless(Project::new("Intern"));
    let pisynth = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: pisynth,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    (app, track)
}

fn add(app: &mut App, kind: Internal, target: DropTarget) {
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: kind.plugin_ref(),
        target,
    }));
}

fn chain(app: &App, track: TrackId) -> Vec<(DeviceId, String)> {
    app.project()
        .track(track)
        .unwrap()
        .channel
        .devices
        .iter()
        .map(|d| (d.id, d.plugin.name.clone()))
        .collect()
}

fn set_param(app: &mut App, device: DeviceId, kind: Internal, key: &str, value: f64) {
    let id = kind.wire(&ParamId::new(key)).expect("known parameter");
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}

fn values(app: &App, device: DeviceId) -> Vec<f64> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    let kind = Internal::from_plugin(&d.plugin).unwrap();
    kind.decode_state(d.state.as_deref()).unwrap()
}

#[test]
fn the_browser_lists_the_effects_as_intern_and_a_double_click_adds_one() {
    let (mut app, track) = app_with_track();
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    let mut h = harness(app);
    for name in ["EQ", "Delay", "Hall", "Limiter"] {
        h.get_by_role_and_label(Role::ListItem, name);
    }
    // Category folders are closed; search opens the matching rows.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetBrowserQuery("limiter".into())));
    h.run_steps(2);
    let row = h.get_by_role_and_label(Role::ListItem, "Limiter").rect();
    double_click(&mut h, row.center());
    let devices = chain(h.state(), track);
    assert_eq!(devices.last().unwrap().1, "Limiter");
    let (_, _, d) = h.state().project().find_device(devices[1].0).unwrap();
    assert_eq!(d.role, DeviceRole::Effect);
    assert_eq!(d.plugin.format, PluginFormat::Builtin);
    assert_eq!(h.state().engine().host().status(d.id), DeviceStatus::Ok);
    // The new card is on the chain.
    h.get_by_role_and_label(Role::Group, &names::card("Limiter"));
}

#[test]
fn a_limiter_goes_on_the_master_chain_like_any_device() {
    let (mut app, _) = app_with_track();
    add(
        &mut app,
        Internal::Limiter,
        DropTarget::Chain {
            channel: ChannelRef::Master,
            index: 0,
        },
    );
    let master = &app.project().master.devices;
    assert_eq!(master.len(), 1);
    assert_eq!(master[0].plugin.id, "limiter");
    assert_eq!(
        app.engine().host().status(master[0].id),
        DeviceStatus::Ok,
        "the processor was created"
    );
    assert!(app.engine().host().latency(master[0].id) == 0);
}

#[test]
fn a_card_shows_its_parameters_and_a_turned_knob_is_saved_in_the_project() {
    let dir = std::env::temp_dir().join(format!("komponist-wp85-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Intern.komponist");
    // Open an empty saved project first, so Save has a file and nothing asks for confirmation.
    std::fs::write(
        &path,
        komponist_model::persist::to_json(&Project::new("Intern")).unwrap(),
    )
    .unwrap();
    let mut app = App::headless(Project::new("Intern"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
    let pisynth = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: pisynth,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    add(&mut app, Internal::Hall, DropTarget::Track(track));
    let device = chain(&app, track)[1].0;
    let mut h = harness(app);
    h.run_steps(30);
    let label = names::card("Hall");
    let card = h.get_by_role_and_label(Role::Group, &label);
    let mut sliders: Vec<String> = card
        .get_all_by_role(Role::Slider)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    sliders.sort();
    for name in [
        "Größe",
        "Decay",
        "Pre-Delay",
        "Dämpfung",
        "Diffusion",
        "Breite",
        "Modul.",
        "Mix",
    ] {
        assert!(sliders.iter().any(|s| s == name), "{name} in {sliders:?}");
    }
    card.get_by_role_and_label(Role::Button, "Einfrieren");
    // Drag "Decay" up.
    let before = values(h.state(), device);
    let knob = card.get_by_role_and_label(Role::Slider, "Decay").rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let after = values(h.state(), device);
    let decay = Internal::Hall.wire(&ParamId::new("decay")).unwrap() as usize;
    assert!(
        after[decay] > before[decay] * 1.3,
        "{} → {}",
        before[decay],
        after[decay]
    );
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device: d, id, .. })
            if *d == device && *id == decay as u32
    )));
    // Saved and loaded again: the value is in the file, keyed by parameter id.
    h.state_mut().dispatch(Action::Project(ProjectAction::Save));
    let text = std::fs::read_to_string(&path).unwrap();
    let loaded = komponist_model::persist::from_json(&text).unwrap();
    let (_, _, d) = loaded.find_device(device).unwrap();
    let state: serde_json::Value = serde_json::from_slice(d.state.as_deref().unwrap()).unwrap();
    assert!((state["params"]["decay"].as_f64().unwrap() - after[decay]).abs() < 1e-9);
    assert_eq!(
        Internal::Hall.decode_state(d.state.as_deref()).unwrap(),
        after
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mixer_insert_of_an_internal_effect_jumps_to_its_card() {
    let (mut app, track) = app_with_track();
    add(&mut app, Internal::Eq, DropTarget::Track(track));
    let eq = chain(&app, track)[1].0;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Editor)));
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let name = h.state().project().tracks[0].name.clone();
    let slot = h
        .get_by_role_and_label(
            Role::Group,
            &komponist_app::views::mixer::model::names::strip(&name),
        )
        .get_by_role_and_label(
            Role::Button,
            &komponist_app::views::mixer::model::names::insert("EQ"),
        )
        .rect();
    double_click_or_click(&mut h, slot.center());
    let ui = h.state().ui();
    assert_eq!(ui.layout.dock_tab, DockTab::Devices);
    assert!(ui.selection.devices.contains(&eq));
    assert!(ui.plugin_windows.is_empty());
}

fn double_click_or_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    press(h, pos, true);
    press(h, pos, false);
    h.run_steps(3);
}

#[test]
fn snapshot_of_the_four_effect_frames() {
    let (mut app, track) = app_with_track();
    for kind in [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
    ] {
        add(&mut app, kind, DropTarget::Track(track));
    }
    let ids = chain(&app, track);
    let (eq, delay) = (ids[1].0, ids[2].0);
    // A curve that is not flat: a bell at 2.4 kHz, a low shelf, a high cut; delay at 250 ms.
    for (key, v) in [
        ("b4_gain", 7.0),
        ("b1_type", 1.0),
        ("b1_gain", -4.0),
        ("b5_type", 4.0),
        ("b5_freq", 12_000.0),
    ] {
        set_param(&mut app, eq, Internal::Eq, key, v);
    }
    set_param(&mut app, delay, Internal::Delay, "time", 250.0);
    app.dispatch(Action::Ui(UiAction::ToggleDeviceCollapsed {
        device: ids[0].0,
        all: false,
    }));
    let mut h = harness(app);
    h.run_steps(2);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    let mut results = SnapshotResults::new();
    h.snapshot("devices-internal-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// MIDI effects (WP-88)
// ------------------------------------------------------------------------------------------------

fn names_of(app: &App, track: TrackId) -> Vec<String> {
    chain(app, track).into_iter().map(|c| c.1).collect()
}

/// pisynth with the arpeggiator and the Tonleiter in front of it and an EQ behind it.
fn app_with_midi_effects() -> (App, TrackId) {
    let (mut app, track) = app_with_track();
    add(&mut app, Internal::Arpeggiator, DropTarget::Track(track));
    // Dropped behind everything, a MIDI effect still lands in the MIDI part.
    add(
        &mut app,
        Internal::Tonleiter,
        DropTarget::Chain {
            channel: ChannelRef::Track(track),
            index: 99,
        },
    );
    add(&mut app, Internal::Eq, DropTarget::Track(track));
    (app, track)
}

#[test]
fn a_dropped_midi_effect_goes_into_the_midi_part_in_front_of_the_instrument() {
    let (app, track) = app_with_midi_effects();
    assert_eq!(
        names_of(&app, track),
        ["Arpeggiator", "Tonleiter", "pisynth", "EQ"]
    );
    let channel = &app.project().track(track).unwrap().channel;
    assert_eq!(channel.midi_effect_count(), 2);
    assert_eq!(channel.instrument().unwrap().0, 2);
    assert!(
        channel.devices[..2]
            .iter()
            .all(|d| d.role == DeviceRole::MidiEffect)
    );
    // The engine's program has them as stages, their processors exist and run.
    let mut h = harness(app);
    h.run_steps(3);
    let app = h.state_mut();
    let program = app.engine().program().unwrap();
    let t = program.track(track).unwrap();
    assert_eq!((t.stage_count(), t.chain.len()), (2, 4));
    let channel = &app.project().track(track).unwrap().channel;
    for d in &channel.devices {
        assert_eq!(
            app.engine().host().status(d.id),
            DeviceStatus::Ok,
            "{}",
            d.plugin.name
        );
    }
    // A MIDI effect needs a track: dropped on the master it changes nothing.
    add(
        app,
        Internal::Arpeggiator,
        DropTarget::Chain {
            channel: ChannelRef::Master,
            index: 0,
        },
    );
    assert!(app.project().master.devices.is_empty());
    // The instrument is replaced in place, not put in front of the MIDI effects.
    let klopfer_free = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: klopfer_free,
        target: DropTarget::Track(track),
    }));
    assert_eq!(
        names_of(app, track),
        ["Arpeggiator", "Tonleiter", "pisynth", "EQ"]
    );
}

#[test]
fn the_browser_lists_the_midi_effects_in_their_own_section() {
    let (mut app, _) = app_with_track();
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    let h = harness(app);
    h.get_by_role_and_label(Role::ListItem, "Arpeggiator");
    h.get_by_role_and_label(Role::ListItem, "Tonleiter");
    // The effects segment lists the section after the audio effects.
    let rows =
        komponist_app::views::browser::model::rows(&komponist_app::views::browser::model::Input {
            catalog: h.state().engine().host().catalog(),
            favorites: &Default::default(),
            segment: BrowserSegment::Effects,
            query: "",
            collapsed: &Default::default(),
            project: h.state().project(),
            exports: &[],
        });
    let texts: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
    let section = texts.iter().position(|t| *t == "MIDI-Effekte").unwrap();
    assert_eq!(&texts[section + 1..], ["Arpeggiator", "Tonleiter"]);
    // Search finds them in the effects segment too, and not among the instruments.
    let search = |segment, query: &str| {
        komponist_app::views::browser::model::rows(&komponist_app::views::browser::model::Input {
            catalog: h.state().engine().host().catalog(),
            favorites: &Default::default(),
            segment,
            query,
            collapsed: &Default::default(),
            project: h.state().project(),
            exports: &[],
        })
        .into_iter()
        .map(|r| r.name)
        .collect::<Vec<_>>()
    };
    assert!(
        search(BrowserSegment::Effects, "arpeg")
            .iter()
            .any(|n| n == "Arpeggiator")
    );
    assert!(
        !search(BrowserSegment::Instruments, "arpeg")
            .iter()
            .any(|n| n == "Arpeggiator")
    );
}

#[test]
fn a_turned_knob_of_a_midi_effect_is_saved_and_reaches_the_engine() {
    let (mut app, track) = app_with_midi_effects();
    let arp = chain(&app, track)[0].0;
    set_param(&mut app, arp, Internal::Arpeggiator, "mode", 2.0);
    set_param(&mut app, arp, Internal::Arpeggiator, "rate", 6.0);
    let v = values(&app, arp);
    let wire = |key| Internal::Arpeggiator.wire(&ParamId::new(key)).unwrap() as usize;
    assert_eq!((v[wire("mode")], v[wire("rate")]), (2.0, 6.0));
    assert!(app.dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device, .. }) if *device == arp
    )));
}

#[test]
fn midi_effects_reorder_inside_their_part_and_never_behind_the_instrument() {
    let (app, track) = app_with_midi_effects();
    let mut h = harness(app);
    h.run_steps(5);
    let head = |h: &Harness<'_, App>, name: &str| {
        let r = h
            .get_by_role_and_label(Role::Group, &names::card(name))
            .rect();
        // The head, where a card is grabbed.
        pos2(
            r.left() + 30.0,
            r.top() + komponist_tokens::size::PANEL_HEADER / 2.0,
        )
    };
    // Tonleiter before Arpeggiator.
    let arp = h
        .get_by_role_and_label(Role::Group, &names::card("Arpeggiator"))
        .rect();
    let from = head(&h, "Tonleiter");
    drag(&mut h, from, pos2(arp.left() + 10.0, arp.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Tonleiter", "Arpeggiator", "pisynth", "EQ"]
    );
    // Dragged behind the instrument (pisynth's card is wide, so EQ may lie off screen), a MIDI
    // effect stops at the instrument.
    let synth = h
        .get_by_role_and_label(Role::Group, &names::card("pisynth"))
        .rect();
    let from = head(&h, "Tonleiter");
    drag(&mut h, from, pos2(synth.right() - 10.0, synth.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Arpeggiator", "Tonleiter", "pisynth", "EQ"]
    );
    // An effect dragged in front of the instrument stops behind it; the instrument stays.
    let arp = h
        .get_by_role_and_label(Role::Group, &names::card("Arpeggiator"))
        .rect();
    let from = head(&h, "EQ");
    drag(&mut h, from, pos2(arp.left() + 10.0, arp.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Arpeggiator", "Tonleiter", "pisynth", "EQ"]
    );
}

#[test]
fn snapshot_of_a_chain_with_midi_effects_left_of_the_instrument() {
    let (mut app, track) = app_with_midi_effects();
    let ids = chain(&app, track);
    set_param(&mut app, ids[0].0, Internal::Arpeggiator, "mode", 2.0);
    set_param(&mut app, ids[0].0, Internal::Arpeggiator, "octaves", 2.0);
    set_param(&mut app, ids[1].0, Internal::Tonleiter, "scale", 1.0);
    set_param(&mut app, ids[1].0, Internal::Tonleiter, "root", 9.0);
    let mut h = harness(app);
    h.run_steps(4);
    // The order the signal runs in, left to right.
    let x = |h: &Harness<'_, App>, name: &str| {
        h.get_by_role_and_label(Role::Group, &names::card(name))
            .rect()
            .left()
    };
    let xs: Vec<f32> = ["Arpeggiator", "Tonleiter", "pisynth", "EQ"]
        .iter()
        .map(|n| x(&h, n))
        .collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    let mut results = SnapshotResults::new();
    h.snapshot("devices-midi-effects-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// PDC follows a latency-changing parameter (WP-91)

#[test]
fn toggling_the_limiter_lookahead_while_playing_updates_the_delay_compensation() {
    let (mut app, with_limiter) = app_with_track();
    let pisynth = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: pisynth,
        target: DropTarget::NewTrack { index: None },
    }));
    let plain = app.project().tracks[1].id;
    add(
        &mut app,
        Internal::Limiter,
        DropTarget::Chain {
            channel: ChannelRef::Track(with_limiter),
            index: 1,
        },
    );
    let limiter = chain(&app, with_limiter)[1].0;
    let pdc = |app: &App, track: TrackId| {
        app.engine()
            .program()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.id == track)
            .unwrap()
            .pdc_delay
    };
    app.engine_mut().hold_engine().expect("engine held");
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::TogglePlay,
    ));

    // Lookahead is on by default: the limited track is late, the other one waits for it.
    app.step();
    let lookahead = app.engine().host().latency(limiter);
    assert!(lookahead > 0, "the default limiter has lookahead");
    assert_eq!(pdc(&app, with_limiter), 0);
    assert_eq!(pdc(&app, plain), lookahead);

    // Off: no latency, nothing to compensate. The change travels audio thread → meter → host
    // → recompile; one block is enough to publish it.
    set_param(&mut app, limiter, Internal::Limiter, "lookahead", 0.0);
    assert!(app.engine_mut().run_held(1024, &[], 0));
    app.step();
    assert_eq!(app.engine().host().latency(limiter), 0);
    assert_eq!(pdc(&app, plain), 0);
    assert_eq!(pdc(&app, with_limiter), 0);

    // On again: the other track follows.
    set_param(&mut app, limiter, Internal::Limiter, "lookahead", 1.0);
    assert!(app.engine_mut().run_held(1024, &[], 0));
    app.step();
    assert_eq!(pdc(&app, plain), lookahead);
    assert_eq!(pdc(&app, with_limiter), 0);
}
