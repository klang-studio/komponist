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
use komponist_app::{Action, App, PluginAction, ProjectAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, PluginFormat, Project, TrackId};
use komponist_plugins::DeviceStatus;

use crate::common::{
    assert_authored_plain, author_base, author_parameter, project_with_snapshot,
    set_authored_param as set_param, shipped_plugin, step_until, wait_for_layout,
};

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

/// A project with one hexa track and the dock on „Geräte“.
fn app_with_track() -> (App, TrackId) {
    let mut app = App::headless(Project::new("Intern"));
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    let device = app.project().tracks[0].channel.devices[0].id;
    wait_for_layout(&mut app, device);
    (app, track)
}

fn shipped(app: &App, id: &str) -> komponist_model::PluginRef {
    let id = format!("org.klang-studio.komponist.{id}");
    shipped_plugin(app.engine().host(), &id)
        .unwrap_or_else(|| panic!("build {id} with scripts/build-test-plugins.sh"))
}

fn add(app: &mut App, id: &str, target: DropTarget) {
    let device = DeviceId(app.project().next_id);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(app, id),
        target,
    }));
    if app.project().find_device(device).is_some() {
        wait_for_layout(app, device);
    }
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

#[test]
fn the_browser_lists_shipped_effects_and_a_double_click_adds_one() {
    let (mut app, track) = app_with_track();
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    let mut h = harness(app);
    for name in ["EQ", "Delay", "Hall", "Limiter"] {
        assert!(
            h.query_all_by_role_and_label(Role::ListItem, name)
                .next()
                .is_some()
        );
    }
    // Category folders are closed; search opens the matching rows.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetBrowserQuery("limiter".into())));
    h.run_steps(2);
    let row = h
        .get_all_by_role_and_label(Role::ListItem, "Limiter")
        .next()
        .unwrap()
        .rect();
    double_click(&mut h, row.center());
    let devices = chain(h.state(), track);
    assert_eq!(devices.last().unwrap().1, "Limiter");
    let (_, _, d) = h.state().project().find_device(devices[1].0).unwrap();
    assert_eq!(d.role, DeviceRole::Effect);
    assert_eq!(d.plugin.format, PluginFormat::Clap);
    assert_eq!(h.state().engine().host().status(d.id), DeviceStatus::Ok);
    // The new card is on the chain.
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Limiter"));
}

#[test]
fn a_limiter_goes_on_the_master_chain_like_any_device() {
    let (mut app, _) = app_with_track();
    add(
        &mut app,
        "limiter",
        DropTarget::Chain {
            channel: ChannelRef::Master,
            index: 0,
        },
    );
    let master = &app.project().master.devices;
    assert_eq!(master.len(), 1);
    assert_eq!(master[0].plugin.id, "org.klang-studio.komponist.limiter");
    assert_eq!(
        app.engine().host().status(master[0].id),
        DeviceStatus::Ok,
        "the processor was created"
    );
    assert!(app.engine().host().latency(master[0].id) > 0);
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
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    add(&mut app, "hall", DropTarget::Track(track));
    let device = chain(&app, track)[1].0;
    let mut h = harness(app);
    h.run_steps(30);
    let label = Lang::De.devices_card("Hall");
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
    let decay = author_parameter(h.state(), device, "decay");
    let before = author_base(h.state(), device, "decay");
    let knob = card.get_by_role_and_label(Role::Slider, "Decay").rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    step_until(h.state_mut(), "the Hall decay edit", |app| {
        author_base(app, device, "decay") > before
    });
    let after = author_base(h.state(), device, "decay");
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device: d, id, .. })
            if *d == device && *id == decay
    )));
    // Save and reopen the project through the same verified receiver.
    h.state_mut().dispatch(Action::Project(ProjectAction::Save));
    let text = std::fs::read_to_string(&path).unwrap();
    let loaded = komponist_model::persist::from_json(&text).unwrap();
    let mut reopened = App::headless(loaded);
    wait_for_layout(&mut reopened, device);
    assert!((author_base(&reopened, device, "decay") - after).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mixer_insert_of_a_shipped_effect_jumps_to_its_card() {
    let (mut app, track) = app_with_track();
    add(&mut app, "eq", DropTarget::Track(track));
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
        .get_by_role_and_label(Role::Group, &Lang::De.mixer_strip(&name))
        .get_by_role_and_label(Role::Button, &Lang::De.mixer_insert("EQ"))
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
    for kind in ["eq", "delay", "hall", "limiter"] {
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
        set_param(&mut app, eq, key, v);
    }
    set_param(&mut app, delay, "time", 250.0);
    app.dispatch(Action::Ui(UiAction::ToggleDeviceCollapsed {
        device: ids[0].0,
        all: false,
    }));
    let mut h = harness(app);
    h.run_steps(2);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
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

/// hexa with the arpeggiator and the Tonleiter in front of it and an EQ behind it.
fn app_with_midi_effects() -> (App, TrackId) {
    let (mut app, track) = app_with_track();
    add(&mut app, "arpeggiator", DropTarget::Track(track));
    // Dropped behind everything, a MIDI effect still lands in the MIDI part.
    add(
        &mut app,
        "scale",
        DropTarget::Chain {
            channel: ChannelRef::Track(track),
            index: 99,
        },
    );
    add(&mut app, "eq", DropTarget::Track(track));
    (app, track)
}

#[test]
fn a_dropped_midi_effect_goes_into_the_midi_part_in_front_of_the_instrument() {
    let (app, track) = app_with_midi_effects();
    assert_eq!(
        names_of(&app, track),
        ["Arpeggiator", "Scale", "Hexa", "EQ"]
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
        "arpeggiator",
        DropTarget::Chain {
            channel: ChannelRef::Master,
            index: 0,
        },
    );
    assert!(app.project().master.devices.is_empty());
    // The instrument is replaced in place, not put in front of the MIDI effects.
    add(app, "sampler", DropTarget::Track(track));
    assert_eq!(
        names_of(app, track),
        ["Arpeggiator", "Scale", "Klopfer", "EQ"]
    );
}

#[test]
fn the_browser_lists_the_midi_effects_in_their_own_section() {
    let (mut app, _) = app_with_track();
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    let h = harness(app);
    assert!(
        h.query_all_by_role_and_label(Role::ListItem, "Arpeggiator")
            .next()
            .is_some()
    );
    assert!(
        h.query_all_by_role_and_label(Role::ListItem, "Tonleiter")
            .next()
            .is_some()
    );
    // The effects segment lists the section after the audio effects.
    let rows = komponist_app::views::browser::model::rows(
        Lang::De,
        &komponist_app::views::browser::model::Input {
            catalog: h.state().engine().host().catalog(),
            favorites: &Default::default(),
            segment: BrowserSegment::Effects,
            query: "",
            collapsed: &Default::default(),
            project: h.state().project(),
            exports: &[],
        },
    );
    use komponist_app::views::browser::model::RowKind;
    let midi_effects: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            matches!(
                row.kind,
                RowKind::Plugin {
                    kind: komponist_ipc::PluginKind::MidiEffect,
                    ..
                }
            )
            .then_some(row.name.as_str())
        })
        .collect();
    assert_eq!(midi_effects, ["Arpeggiator", "Tonleiter"]);
    assert!(rows.iter().any(|row| row.name == "MIDI-Effekte"));
    // Search finds them in the effects segment too, and not among the instruments.
    let search = |segment, query: &str| {
        komponist_app::views::browser::model::rows(
            Lang::De,
            &komponist_app::views::browser::model::Input {
                catalog: h.state().engine().host().catalog(),
                favorites: &Default::default(),
                segment,
                query,
                collapsed: &Default::default(),
                project: h.state().project(),
                exports: &[],
            },
        )
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
    set_param(&mut app, arp, "mode", 2.0);
    set_param(&mut app, arp, "rate", 6.0);
    let project = project_with_snapshot(&mut app, arp);
    let mut reopened = App::headless(project);
    wait_for_layout(&mut reopened, arp);
    assert_authored_plain(&reopened, arp, "mode", 2.0);
    assert_authored_plain(&reopened, arp, "rate", 6.0);
    assert!(app.dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device, .. }) if *device == arp
    )));
}

#[test]
fn midi_effects_reorder_inside_their_part_and_never_behind_the_instrument() {
    let (mut app, track) = app_with_midi_effects();
    app.ui_mut().track_macros.collapsed.insert(track);
    let mut h = harness(app);
    h.run_steps(5);
    let head = |h: &Harness<'_, App>, name: &str| {
        let r = h
            .get_by_role_and_label(Role::Group, &Lang::De.devices_card(name))
            .rect();
        // The head, where a card is grabbed.
        pos2(
            r.left() + 30.0,
            r.top() + komponist_tokens::size::PANEL_HEADER / 2.0,
        )
    };
    // Tonleiter before Arpeggiator.
    let arp = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Arpeggiator"))
        .rect();
    let from = head(&h, "Tonleiter");
    drag(&mut h, from, pos2(arp.left() + 10.0, arp.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Scale", "Arpeggiator", "Hexa", "EQ"]
    );
    // Dragged behind the instrument (hexa's card is wide, so EQ may lie off screen), a MIDI
    // effect stops at the instrument.
    let synth = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Hexa"))
        .rect();
    let from = head(&h, "Tonleiter");
    drag(&mut h, from, pos2(synth.right() - 10.0, synth.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Arpeggiator", "Scale", "Hexa", "EQ"]
    );
    // An effect dragged in front of the instrument stops behind it; the instrument stays.
    let arp = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Arpeggiator"))
        .rect();
    let from = head(&h, "EQ");
    drag(&mut h, from, pos2(arp.left() + 10.0, arp.center().y));
    assert_eq!(
        names_of(h.state(), track),
        ["Arpeggiator", "Scale", "Hexa", "EQ"]
    );
}

#[test]
fn snapshot_of_a_chain_with_midi_effects_left_of_the_instrument() {
    let (mut app, track) = app_with_midi_effects();
    let ids = chain(&app, track);
    set_param(&mut app, ids[0].0, "mode", 2.0);
    set_param(&mut app, ids[0].0, "octaves", 2.0);
    set_param(&mut app, ids[1].0, "scale", 1.0);
    set_param(&mut app, ids[1].0, "root", 9.0);
    let mut h = harness(app);
    h.run_steps(4);
    // The order the signal runs in, left to right.
    let x = |h: &Harness<'_, App>, name: &str| {
        h.get_by_role_and_label(Role::Group, &Lang::De.devices_card(name))
            .rect()
            .left()
    };
    let xs: Vec<f32> = ["Arpeggiator", "Tonleiter", "Hexa", "EQ"]
        .iter()
        .map(|n| x(&h, n))
        .collect();
    assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
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
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let plain = app.project().tracks[1].id;
    add(
        &mut app,
        "limiter",
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
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::TogglePlay,
    ));
    step_until(&mut app, "the limiter active during playback", |app| {
        app.transport() == komponist_engine::TransportState::Playing
    });
    let lookahead = app.engine().host().latency(limiter);
    assert!(lookahead > 0, "the default limiter has lookahead");
    assert_eq!(pdc(&app, plain), lookahead);
    for (value, delay) in [(0.0, 0), (1.0, lookahead)] {
        set_param(&mut app, limiter, "lookahead", value);
        step_until(
            &mut app,
            "the limiter latency reflected in the track delays",
            |app| app.engine().host().latency(limiter) == delay && pdc(app, plain) == delay,
        );
        assert_eq!(pdc(&app, with_limiter), 0);
        assert_eq!(app.transport(), komponist_engine::TransportState::Playing);
    }
}

// ------------------------------------------------------------------------------------------------
// The EQ's live spectrum (WP-92): the tap runs only while the EQ is on screen and „Analyse“ is on

/// How many samples the EQ of `app` has copied into its tap so far.
fn tapped(app: &App, device: DeviceId) -> u64 {
    app.engine()
        .host()
        .signal_tap(device, "spectrum")
        .unwrap()
        .samples()
        .written()
}

#[test]
fn the_eq_taps_its_output_only_while_it_is_shown_and_analyse_is_on() {
    use komponist_engine::{MidiInEvent, MidiPort};
    const SECOND: usize = 48_000;
    let (mut app, track) = app_with_track();
    add(&mut app, "eq", DropTarget::Track(track));
    let eq = chain(&app, track)[1].0;
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    let note = MidiInEvent {
        frame: 0,
        port: MidiPort(1),
        data: [0x90, 57, 120],
    };
    let play = |h: &mut Harness<'_, App>, frames: usize, events: &[MidiInEvent]| {
        for chunk in 0..frames.div_ceil(4800) {
            let events = if chunk == 0 { events } else { &[] };
            assert!(h.state_mut().engine_mut().run_held(4800, events, 0));
            h.run_steps(2);
        }
    };

    // The card is on screen but nobody has asked yet: the first blocks copy nothing.
    assert_eq!(tapped(h.state(), eq), 0);
    // Shown: the view renews the lease every frame, the audio thread copies.
    play(&mut h, SECOND / 2, &[note]);
    assert!(tapped(h.state(), eq) > 0, "visible EQ is tapped");

    // „Analyse“ off: the tap stops at once.
    set_param(h.state_mut(), eq, "analyse", 0.0);
    h.run_steps(2);
    play(&mut h, 4800, &[]);
    let off = tapped(h.state(), eq);
    play(&mut h, SECOND / 2, &[]);
    assert_eq!(tapped(h.state(), eq), off, "analyse off stops the tap");

    // Back on, then hidden (another dock tab): the lease runs out within half a second of audio.
    set_param(h.state_mut(), eq, "analyse", 1.0);
    h.run_steps(2);
    play(&mut h, 4800, &[]);
    assert!(tapped(h.state(), eq) > off, "analyse on again");
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    h.run_steps(3);
    play(&mut h, SECOND, &[]);
    let hidden = tapped(h.state(), eq);
    play(&mut h, SECOND / 2, &[]);
    assert_eq!(tapped(h.state(), eq), hidden, "a hidden EQ costs nothing");
}

// ------------------------------------------------------------------------------------------------
// The compressor „presse“ (WP-93)

#[test]
fn the_browser_lists_presse_under_dynamics_and_a_double_click_adds_it() {
    let (mut app, track) = app_with_track();
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetBrowserQuery("Presse".into())));
    h.run_steps(2);
    let row = h.get_by_role_and_label(Role::ListItem, "Presse").rect();
    double_click(&mut h, row.center());
    let devices = chain(h.state(), track);
    assert_eq!(devices.last().unwrap().1, "Presse");
    let (_, _, d) = h.state().project().find_device(devices[1].0).unwrap();
    assert_eq!(d.plugin.format, PluginFormat::Clap);
    assert_eq!(d.plugin.id, "org.klang-studio.komponist.presse");
    assert_eq!(h.state().engine().host().status(d.id), DeviceStatus::Ok);
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Presse"));
    // The card has the two graphs and the meter.
    h.get_by_role_and_label(Role::Group, "Kennlinie");
    h.get_by_role_and_label(Role::Group, "Reduktion \u{b7} 4 s");
    h.get_by_role_and_label(Role::Slider, "Threshold");
}

#[test]
fn toggling_the_presse_lookahead_while_playing_updates_the_delay_compensation() {
    let (mut app, with_presse) = app_with_track();
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let plain = app.project().tracks[1].id;
    add(
        &mut app,
        "presse",
        DropTarget::Chain {
            channel: ChannelRef::Track(with_presse),
            index: 1,
        },
    );
    let presse = chain(&app, with_presse)[1].0;
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
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::TogglePlay,
    ));
    step_until(&mut app, "Presse active during playback", |app| {
        app.transport() == komponist_engine::TransportState::Playing
    });
    assert_eq!(app.engine().host().latency(presse), 0);
    assert_eq!(pdc(&app, plain), 0);
    set_param(&mut app, presse, "lookahead", 1.0);
    step_until(
        &mut app,
        "Presse publishes its new lookahead latency",
        |app| {
            let latency = app.engine().host().latency(presse);
            latency > 0 && pdc(app, plain) == latency
        },
    );
    assert_eq!(pdc(&app, with_presse), 0);
    set_param(&mut app, presse, "lookahead", 0.0);
    step_until(&mut app, "Presse reactivates with no lookahead", |app| {
        app.engine().host().latency(presse) == 0 && pdc(app, plain) == 0
    });
    assert_eq!(app.transport(), komponist_engine::TransportState::Playing);
}

/// The sample tap of `device`, for the counters.
fn tap_of(app: &App, device: DeviceId) -> std::sync::Arc<komponist_engine::SampleTap> {
    app.engine()
        .host()
        .signal_tap(device, "detector")
        .unwrap()
        .samples()
        .clone()
}

/// Plays one second of a note through a track with `presse` (threshold −40 dB, so it compresses)
/// and gives the harness and the device.
fn playing_presse() -> (Harness<'static, App>, DeviceId) {
    use komponist_engine::{MidiInEvent, MidiPort};
    let (mut app, track) = app_with_track();
    add(&mut app, "presse", DropTarget::Track(track));
    let presse = chain(&app, track)[1].0;
    for (key, v) in [("threshold", -40.0), ("ratio", 8.0), ("detector", 0.0)] {
        set_param(&mut app, presse, key, v);
    }
    app.dispatch(Action::Edit(komponist_app::EditAction::SetTrackProp {
        tracks: vec![track],
        prop: komponist_model::edits::TrackProp::Arm(true),
    }));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    let note = MidiInEvent {
        frame: 0,
        port: MidiPort(1),
        data: [0x90, 57, 127],
    };
    h.run_steps(2);
    for chunk in 0..10 {
        let events: &[MidiInEvent] = if chunk == 0 { &[note] } else { &[] };
        assert!(h.state_mut().engine_mut().run_held(4800, events, 0));
        h.run_steps(2);
    }
    (h, presse)
}

#[test]
fn the_presse_card_follows_the_signal_only_while_it_is_shown() {
    let (mut h, presse) = playing_presse();
    let tap = tap_of(h.state(), presse);
    assert!(tap.written() > 0, "a visible presse feeds its tap");
    // Hidden (another dock tab): the lease runs out within half a second of audio.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    h.run_steps(3);
    for _ in 0..10 {
        assert!(h.state_mut().engine_mut().run_held(4800, &[], 0));
        h.run_steps(1);
    }
    let hidden = tap.written();
    for _ in 0..5 {
        assert!(h.state_mut().engine_mut().run_held(4800, &[], 0));
        h.run_steps(1);
    }
    assert_eq!(tap.written(), hidden, "a hidden presse costs nothing");
}

#[test]
fn the_presse_meter_shows_the_reduction_the_engine_measured() {
    let (h, presse) = playing_presse();
    assert!(
        h.state()
            .engine()
            .host()
            .signal_tap(presse, "reduction")
            .is_some()
    );
    let text = h
        .get_by_role_and_label(Role::Meter, komponist_i18n::de::UI_METER_GR_NAME)
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(text.contains("\u{2212}"), "a reduction is shown: {text}");
}

#[test]
fn snapshot_of_the_presse_frame_while_playing() {
    let (mut h, _) = playing_presse();
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    let mut results = SnapshotResults::new();
    h.snapshot("devices-presse-live-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
