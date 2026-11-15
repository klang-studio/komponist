//! The Sampler in the app (WP-87): the device card with its pads, waveform and knobs, sample drop
//! and the file browser, links in the project file (absolute and relative), "Sammeln und
//! speichern", missing samples with "Suchen …", and the pad selection that follows the step
//! sequencer and the input.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{app_with, config, scratch, step_until};
use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange, TransportAction};
use komponist_app::device_host::{DeviceHost as _, PluginHost};
use komponist_app::ui_state::{DockTab, StepCursor};
use komponist_app::{Action, App, PluginAction, ProjectAction, UiAction};
use komponist_builtin::Internal;
use komponist_builtin::sampler::{PadStatus, SampleLink, links, testkit};
use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack};
use komponist_model::project::DEFAULT_PROJECT_NAME;
use komponist_model::{DeviceId, Project, Tick, TickRange, TrackId, persist};

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

/// The CPU read-out changes from run to run.
fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    press(h, pos, PointerButton::Primary, true);
    press(h, pos, PointerButton::Primary, false);
    h.run_steps(3);
}

/// A project with one sampler track; `kit` links the synthesized kit in `dir` to the pads.
fn project(dir: &Path, kit: bool) -> Project {
    let spec = NewTrack {
        name: Some("Drums".into()),
        hue: None,
        instrument: Some(Internal::Sampler.plugin_ref()),
        index: None,
        arm_exclusive: false,
    };
    let (mut p, _) = edits::add_track(&Project::new(DEFAULT_PROJECT_NAME), spec).unwrap();
    if kit {
        let files = testkit::write_kit(dir).unwrap();
        let mut state: Option<Vec<u8>> = None;
        for (pad, path) in files.iter().enumerate() {
            state = Some(links::write(
                state.as_deref(),
                pad,
                Some(&SampleLink::new(path, None)),
            ));
        }
        let track = Arc::make_mut(&mut p.tracks[0]);
        Arc::make_mut(&mut track.channel.devices[0]).state = state.map(Arc::from);
    }
    p
}

fn drums(app: &App) -> (TrackId, DeviceId) {
    let t = &app.project().tracks[0];
    (t.id, t.channel.devices[0].id)
}

fn show_devices(app: &mut App) {
    let (track, _) = drums(app);
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
}

/// Steps until the sampler's pads are all loaded (or missing/failed).
fn settled(app: &mut App) {
    let (_, device) = drums(app);
    step_until(app, "the pads loaded", |a| {
        a.engine()
            .host()
            .sampler(device)
            .is_some_and(|h| h.views().iter().all(|v| v.status != PadStatus::Loading))
    });
}

fn headless_with_kit(dir: &Path) -> App {
    let mut app = App::headless(project(dir, true));
    show_devices(&mut app);
    settled(&mut app);
    app
}

fn pad_states(app: &App) -> Vec<PadStatus> {
    let (_, device) = drums(app);
    app.engine()
        .host()
        .sampler(device)
        .expect("a sampler")
        .views()
        .into_iter()
        .map(|v| v.status)
        .collect()
}

fn state_of(app: &App) -> Option<Arc<[u8]>> {
    app.project().tracks[0].channel.devices[0].state.clone()
}

#[test]
fn the_card_shows_the_pads_the_waveform_and_the_knobs_of_the_selected_pad() {
    let dir = scratch("sampler-card");
    let app = headless_with_kit(&dir);
    let mut h = harness(app);
    h.run_steps(4);
    // The pads carry the kit's names and notes.
    for name in testkit::NAMES {
        h.get_by_role_and_label(Role::Button, &format!("Pad {name}"));
    }
    // Pad 1 is selected: its waveform with the real sample, its knobs.
    h.get_by_role_and_label(Role::Group, "Pad 1 · Kick");
    h.get_by_role_and_label(Role::Slider, "Tune 1");
    h.get_by_role_and_label(Role::Slider, "Sample-Start Position");
    // The snapshot is taken before any click: a click sounds the pad, and its glow and playhead
    // depend on timing.
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("sampler-card-1x");
    results.extend_harness(&mut h);
    results.unwrap();
    // A click selects pad 2 (and sounds it).
    let snare = h
        .get_by_role_and_label(Role::Button, "Pad Snare")
        .rect()
        .center();
    h.state_mut().clear_dispatched();
    click(&mut h, snare);
    h.get_by_role_and_label(Role::Group, "Pad 2 · Snare");
    h.get_by_role_and_label(Role::Slider, "Tune 2");
    assert!(
        h.state()
            .dispatched()
            .any(|a| matches!(a, Action::Plugin(PluginAction::AuditionPad { pad: 1, .. })))
    );
}

#[test]
fn an_empty_sampler_names_its_pads_and_offers_the_load_menu() {
    let dir = scratch("sampler-empty");
    let mut app = App::headless(project(&dir, false));
    show_devices(&mut app);
    let mut h = harness(app);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Pad Pad 1");
    h.get_by_role_and_label(Role::Group, "Pad 1 · kein Sample");
    // The right-click menu: load, remove (disabled without a sample).
    let pad = h
        .get_by_role_and_label(Role::Button, "Pad Pad 3")
        .rect()
        .center();
    h.hover_at(pad);
    h.step();
    press(&mut h, pad, PointerButton::Secondary, true);
    press(&mut h, pad, PointerButton::Secondary, false);
    h.run_steps(3);
    h.get_by_role_and_label(Role::MenuItem, "Sample laden …");
    assert!(
        h.get_by_role_and_label(Role::MenuItem, "Sample entfernen")
            .accesskit_node()
            .is_disabled()
    );
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("sampler-empty-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

/// A file dropped on a pad, as the windowing integration reports it.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn a_sample_dropped_on_a_pad_is_linked_and_loaded() {
    let dir = scratch("sampler-drop");
    let mut app = App::headless(project(&dir, false));
    show_devices(&mut app);
    let mut h = harness(app);
    h.run_steps(3);
    let wav = dir.join("Donner.wav");
    testkit::write_wav(&wav, &vec![0.5; 4800], 48_000).unwrap();
    let target = h
        .get_by_role_and_label(Role::Button, "Pad Pad 3")
        .rect()
        .center();
    h.hover_at(target);
    h.step();
    h.input_mut()
        .dropped_files
        .push(Arc::new(Dropped(wav.clone())));
    h.run_steps(3);
    let (_, device) = drums(h.state());
    let state = state_of(h.state());
    let linked = links::read(state.as_deref());
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].0, 2, "pad 3");
    assert_eq!(linked[0].1.abs, wav);
    assert_eq!(linked[0].1.name, "Donner");
    step_until(h.state_mut(), "the sample loaded", |a| {
        a.engine().host().sampler(device).is_some_and(|s| {
            s.view(2).status == PadStatus::Ready && (s.view(2).length_s - 0.1).abs() < 1e-6
        })
    });
    h.run_steps(3);
    // The pad shows its name, the step rows follow it.
    h.get_by_role_and_label(Role::Button, "Pad Donner");
    let names = h.state().engine().host().note_names(device).to_vec();
    assert_eq!(
        names
            .iter()
            .map(|n| (n.key, n.name.as_str()))
            .collect::<Vec<_>>(),
        [(38, "Donner")]
    );
}

#[test]
fn the_file_browser_loads_a_sample_into_the_pad() {
    let dir = scratch("sampler-browser");
    let mut app = App::headless(project(&dir, false));
    show_devices(&mut app);
    // The browser opens in the project's folder: save the project there first.
    let file = dir.join("Song.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    show_devices(&mut app);
    let wav = dir.join("Klatsch.wav");
    testkit::write_wav(&wav, &vec![0.5; 2400], 48_000).unwrap();
    let (_, device) = drums(&app);
    let mut h = harness(app);
    h.run_steps(3);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::PickSample { device, pad: 4 }));
    h.run_steps(3);
    h.get_by_role_and_label(Role::List, "Dateien");
    // Only folders and WAV files are listed.
    assert!(
        h.query_by_role_and_label(Role::ListItem, "Song.komponist")
            .is_none()
    );
    let row = h
        .get_by_role_and_label(Role::ListItem, "Klatsch.wav")
        .rect()
        .center();
    h.hover_at(row);
    h.step();
    for _ in 0..2 {
        press(&mut h, row, PointerButton::Primary, true);
        press(&mut h, row, PointerButton::Primary, false);
        h.step();
    }
    h.run_steps(3);
    let state = state_of(h.state());
    let linked = links::read(state.as_deref());
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].0, 4);
    // The project has a file: the link also holds the path relative to it.
    assert_eq!(linked[0].1.rel, Path::new("Klatsch.wav"));
    assert!(h.query_by_role_and_label(Role::List, "Dateien").is_none());
}

/// Copies folder `from` into `to` (recursively).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn a_project_round_trip_resolves_relative_first_and_absolute_as_the_fallback() {
    let dir = scratch("sampler-roundtrip");
    let first = dir.join("one");
    std::fs::create_dir_all(first.join("Kit")).unwrap();
    // Link the kit (inside the project's folder) and save.
    let mut app = App::headless(project(&first.join("Kit"), true));
    let file = first.join("Song.komponist");
    app.dispatch(Action::Project(ProjectAction::SaveAs));
    drop(app);
    let mut app = App::headless(project(&first.join("Kit"), true));
    // Save through the real path: the links get their relative paths for this file.
    let json = persist::to_json(app.project()).unwrap();
    std::fs::write(&file, json).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let state = saved.tracks[0].channel.devices[0].state.clone();
    let linked = links::read(state.as_deref());
    assert_eq!(linked.len(), 8);
    assert_eq!(linked[0].1.rel, Path::new("Kit/Kick.wav"));
    assert_eq!(linked[0].1.abs, first.join("Kit/Kick.wav"));
    drop(app);

    // The whole folder moves: the absolute paths are stale, the relative ones resolve.
    let second = dir.join("two");
    copy_dir(&first, &second);
    std::fs::remove_dir_all(&first).unwrap();
    let mut app = App::headless(Project::new("leer"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(
        second.join("Song.komponist"),
    )));
    show_devices(&mut app);
    settled(&mut app);
    assert!(pad_states(&app).iter().all(|s| *s == PadStatus::Ready));
    // Saving rebases: the absolute paths point to the new place now.
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved =
        persist::from_json(&std::fs::read_to_string(second.join("Song.komponist")).unwrap())
            .unwrap();
    let state = saved.tracks[0].channel.devices[0].state.clone();
    let linked = links::read(state.as_deref());
    assert_eq!(linked[0].1.abs, second.join("Kit/Kick.wav"));
    assert_eq!(linked[0].1.rel, Path::new("Kit/Kick.wav"));
    drop(app);

    // Only the absolute path is left (the relative one is stale): it is the fallback.
    let third = dir.join("three");
    std::fs::create_dir_all(&third).unwrap();
    let mut loose = saved.clone();
    let mut state = None;
    for (pad, link) in &linked {
        let moved = SampleLink {
            rel: PathBuf::from("nowhere/x.wav"),
            ..link.clone()
        };
        state = Some(links::write(state.as_deref(), *pad, Some(&moved)));
    }
    Arc::make_mut(&mut Arc::make_mut(&mut loose.tracks[0]).channel.devices[0]).state =
        state.map(Arc::from);
    let file = third.join("Loose.komponist");
    std::fs::write(&file, persist::to_json(&loose).unwrap()).unwrap();
    let mut app = App::headless(Project::new("leer"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    show_devices(&mut app);
    settled(&mut app);
    assert!(pad_states(&app).iter().all(|s| *s == PadStatus::Ready));
}

#[test]
fn collect_and_save_copies_the_samples_next_to_the_project_and_rewrites_the_links() {
    let dir = scratch("sampler-collect");
    let library = dir.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let mut app = App::headless(project(&library, true));
    let home = dir.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    let file = home.join("Hafenlicht.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    show_devices(&mut app);
    settled(&mut app);
    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    let folder = home.join("Hafenlicht Samples");
    let mut copied: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    copied.sort();
    let mut expected: Vec<String> = testkit::NAMES.iter().map(|n| format!("{n}.wav")).collect();
    expected.sort();
    assert_eq!(copied, expected);
    // The library is untouched, the document and the file point into the samples folder.
    assert!(library.join("Kick.wav").is_file());
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let linked = links::read(saved.tracks[0].channel.devices[0].state.as_deref());
    assert_eq!(linked.len(), 8);
    assert_eq!(linked[0].1.abs, folder.join("Kick.wav"));
    assert_eq!(linked[0].1.rel, Path::new("Hafenlicht Samples/Kick.wav"));
    // The project plays from its folder alone: move it, delete the library.
    std::fs::remove_dir_all(&library).unwrap();
    let moved = dir.join("moved");
    copy_dir(&home, &moved);
    std::fs::remove_dir_all(&home).unwrap();
    let mut again = App::headless(Project::new("leer"));
    again.dispatch(Action::Project(ProjectAction::OpenPath(
        moved.join("Hafenlicht.komponist"),
    )));
    show_devices(&mut again);
    settled(&mut again);
    assert!(pad_states(&again).iter().all(|s| *s == PadStatus::Ready));
}

#[test]
fn missing_samples_open_s06_with_suchen_and_a_found_one_leaves_the_list() {
    let dir = scratch("sampler-missing");
    let kit = dir.join("kit");
    std::fs::create_dir_all(&kit).unwrap();
    let home = dir.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    let p = project(&kit, true);
    let file = home.join("Song.komponist");
    std::fs::write(&file, persist::to_json(&p).unwrap()).unwrap();
    // The kit's folder moves away: Kick, Snare and every other file are missing.
    let elsewhere = home.join("kit-new");
    std::fs::rename(&kit, &elsewhere).unwrap();
    let mut app = App::headless(Project::new("leer"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    show_devices(&mut app);
    let mut h = harness(app);
    h.run_steps(4);
    h.get_by_label("8 Samples fehlen");
    h.get_by_role_and_label(Role::ListItem, "Kick SAMPLE, Spur Drums · Pad 1, Kick.wav");
    h.get_by_label("Ohne diese Samples fortfahren");
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("sampler-missing-s06-1x");
    results.extend_harness(&mut h);
    results.unwrap();
    // "Suchen …" on the first row opens the browser; the file in the new place relinks it and
    // every other missing file of that folder with it.
    let search = h
        .get_all_by_role_and_label(Role::Button, "Suchen …")
        .next()
        .expect("a Suchen button")
        .rect()
        .center();
    click(&mut h, search);
    h.run_steps(3);
    // The browser opens in the project's folder; the moved kit is the only folder there. Keys:
    // ↓ highlights a row, Enter opens it.
    let key = |h: &mut Harness<'_, App>, key: egui::Key| {
        h.key_press(key);
        h.run_steps(2);
    };
    key(&mut h, egui::Key::ArrowDown);
    key(&mut h, egui::Key::Enter);
    // In the folder: Clap, Hat offen, Hat zu, Kick …
    for _ in 0..4 {
        key(&mut h, egui::Key::ArrowDown);
    }
    key(&mut h, egui::Key::Enter);
    h.run_steps(4);
    let missing = h.state().project().tracks[0].channel.devices[0]
        .state
        .clone();
    assert!(
        links::missing(missing.as_deref(), None).is_empty(),
        "every sample in that folder was found"
    );
    // Nothing missing: S06 closed by itself.
    assert!(h.query_by_label("8 Samples fehlen").is_none());
}

fn step_clip_project(dir: &Path) -> (Project, komponist_model::ClipId) {
    let p = project(dir, true);
    let track = p.tracks[0].id;
    let rows = komponist_app::step_rows::rows_for(
        Some(&Internal::Sampler.plugin_ref()),
        &komponist_app::device_host::pad_note_names(&[]),
    );
    let clip = NewClip {
        start: Tick::ZERO,
        length: Tick(3840),
        name: "Pattern 1".into(),
        content: NewClipContent::Step { rows },
    };
    let (p, id) = edits::add_clip(&p, track, clip).unwrap();
    (p, id)
}

#[test]
fn the_pad_selection_and_the_step_row_selection_follow_each_other() {
    let dir = scratch("sampler-sync");
    let (project, clip) = step_clip_project(&dir);
    let mut app = App::headless(project);
    show_devices(&mut app);
    settled(&mut app);
    let (_, device) = drums(&app);
    // The clip stays open in the editor while the dock shows the devices.
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    h.run_steps(4);
    assert_eq!(selected_pad(&h), "Pad Kick");
    // A row header in the step sequencer selects its pad.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Step {
            clip,
            row: 3,
            step: None,
        })));
    h.run_steps(4);
    assert_eq!(selected_pad(&h), "Pad Hat zu");
    // A pad click in the device card selects its row in the sequencer.
    let tom = h
        .get_all_by_role_and_label(Role::Button, "Pad Tom")
        .next()
        .expect("pad Tom")
        .rect()
        .center();
    click(&mut h, tom);
    h.run_steps(4);
    assert_eq!(
        h.state().ui().selection.step,
        Some(StepCursor {
            clip,
            row: 5,
            step: None
        })
    );
    let _ = device;
}

/// The name of the pad with the accent outline ("Pad Kick").
fn selected_pad(h: &Harness<'_, App>) -> String {
    h.get_all_by_role(Role::Button)
        .filter(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Pad "))
        })
        .find(|n| n.accesskit_node().is_selected() == Some(true))
        .and_then(|n| n.accesskit_node().label())
        .unwrap_or_default()
}

/// Frames in real time for `time`.
fn run_for(h: &mut Harness<'_, App>, time: Duration) {
    let end = std::time::Instant::now() + time;
    while std::time::Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn pad_folgt_eingang_selects_the_pad_a_note_hit() {
    let dir = scratch("sampler-follow");
    let project = project(&dir, true);
    let host = PluginHost::new(config(&dir));
    let mut app = app_with(project, host);
    show_devices(&mut app);
    let (track, device) = drums(&app);
    step_until(&mut app, "the sampler is running", |a| {
        a.engine().host().sampler(device).is_some()
    });
    settled(&mut app);
    let id = Internal::Sampler
        .wire(&komponist_model::params::ParamId::new("follow"))
        .unwrap();
    let mut h = harness(app);
    // Off: a hit on pad 6 leaves the selection alone.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id,
            value: 0.0,
        }));
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::LiveNote {
            track: Some(track),
            key: 41,
            velocity: 100,
        }));
    run_for(&mut h, Duration::from_millis(500));
    let handle = h.state().engine().host().sampler(device).unwrap().clone();
    assert_eq!(handle.last_hit().1, 5, "the audio thread saw the hit");
    assert_eq!(selected_pad(&h), "Pad Kick");
    // On: the next hit (pad 7) selects its pad.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id,
            value: 1.0,
        }));
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::LiveNote {
            track: Some(track),
            key: 42,
            velocity: 100,
        }));
    run_for(&mut h, Duration::from_millis(500));
    assert_eq!(selected_pad(&h), "Pad Rim");
}

// ------------------------------------------------------------------------------------------------
// The beat in an offline render

/// The rate of the kit's files: the render runs at it, so the samples play without resampling and
/// the onsets compare 1:1.
const RATE: u32 = testkit::RATE;
const BAR: i64 = 3840;
/// Ticks per 1/16 step.
const STEP: i64 = 240;

/// `(row, step)` hits: kick, snare, clap, closed hat, open hat, tom, rim, perc, each on its own
/// step so the onsets do not overlap.
const BEAT: [(usize, usize); 8] = [
    (0, 0),
    (3, 2),
    (1, 4),
    (3, 6),
    (2, 8),
    (5, 10),
    (6, 12),
    (7, 14),
];

struct Env<'a>(&'a PluginHost);

impl komponist_engine::compile::CompileEnv for Env<'_> {
    fn midi_port(&self, _device_name: &str) -> Option<komponist_engine::MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.latency(device)
    }
}

/// Renders `project` (one sampler track) for bar 1 plus a tail through the export pipeline at
/// [`RATE`]; returns the mono mix and the first frame of each 1/16 step.
fn render(project: &Project, dir: &Path) -> (Vec<f32>, Vec<usize>) {
    use komponist_engine::compile;
    use komponist_engine::export::{BitDepth, ExportJob, export};
    use komponist_engine::{Command, Engine, EngineConfig};
    use komponist_plugins::ProcessorSetup;
    let mut host = PluginHost::new(config(dir));
    host.set_project_dir(None);
    let (mut engine, mut handle) = Engine::new(EngineConfig {
        sample_rate: RATE as f32,
        max_block: komponist_engine::MAX_BLOCK,
        seed: 1,
    });
    let program = compile::build(project, None, &Env(&host));
    let device = project.tracks[0].channel.devices[0].clone();
    let setup = ProcessorSetup {
        sample_rate: RATE as f32,
        max_block: komponist_engine::MAX_BLOCK,
    };
    for (id, slot) in compile::slot_changes(None, &program).added {
        assert_eq!(id, device.id);
        let processor = host.create_processor(&device, setup).expect("the sampler");
        handle
            .send(Command::InsertProcessor { slot, processor })
            .expect("command queue");
    }
    handle
        .send(Command::SetProgram(Box::new(program.clone())))
        .expect("command queue");
    // The export waits until the samples are in (the device is `Loading` until then).
    let sampler = host.sampler(device.id).expect("a sampler").clone();
    assert!(sampler.wait_idle(Duration::from_secs(5)));
    host.poll();
    assert_eq!(host.status(device.id), komponist_plugins::DeviceStatus::Ok);
    engine.render(256);
    while handle.poll().is_some() {}

    let map = &program.tempo_map;
    let job = ExportJob {
        path: dir.join("beat.wav"),
        sample_rate: RATE,
        range: ExportJob::frames_of(map, TickRange::new(Tick::ZERO, Tick(BAR)), RATE),
        tail: ExportJob::tail_frames(0.5, RATE),
        bit_depth: BitDepth::Float32,
        normalize: None,
        dither: false,
    };
    export(
        &mut engine,
        &mut handle,
        &program,
        &job,
        &std::sync::atomic::AtomicBool::new(false),
        |_| {},
    )
    .expect("export");
    let mut wav = hound::WavReader::open(&job.path).expect("wav");
    assert_eq!((wav.spec().sample_rate, wav.spec().channels), (RATE, 2));
    let samples: Vec<f32> = wav.samples::<f32>().map(Result::unwrap).collect();
    let mono = samples.chunks(2).map(|f| (f[0] + f[1]) / 2.0).collect();
    let steps = (0..16)
        .map(|s| {
            let ticks = TickRange::new(Tick::ZERO, Tick(s * STEP));
            ExportJob::frames_of(map, ticks, RATE).end as usize
        })
        .collect();
    (mono, steps)
}

/// The best normalised cross-correlation of `audio` from `at + lag` with the start of `sample`
/// over a few frames around `at`: `(correlation, lag)`.
fn correlate(audio: &[f32], at: usize, sample: &[f32]) -> (f32, i64) {
    let n = 512.min(sample.len());
    let y = &sample[..n];
    let yy: f32 = y.iter().map(|v| v * v).sum();
    (-16i64..=16)
        .filter_map(|lag| {
            let start = usize::try_from(at as i64 + lag).ok()?;
            let x = audio.get(start..start + n)?;
            let xy: f32 = x.iter().zip(y).map(|(a, b)| a * b).sum();
            let xx: f32 = x.iter().map(|v| v * v).sum();
            (xx > 0.0).then(|| (xy / (xx * yy).sqrt(), lag))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap_or((0.0, 0))
}

#[test]
fn an_offline_render_plays_the_expected_pad_exactly_on_its_step() {
    let dir = scratch("sampler-render");
    let (mut project, clip) = step_clip_project(&dir);
    // The beat: one hit per step in `BEAT`.
    for (row, step) in BEAT {
        project = edits::set_step(&project, clip, row, step, edits::StepProp::On(true)).unwrap();
    }
    let (audio, steps) = render(&project, &dir);
    let kit = testkit::kit();
    for (step, &frame) in steps.iter().enumerate() {
        let Some(&(row, _)) = BEAT.iter().find(|&&(_, s)| s == step) else {
            continue;
        };
        // The pad whose sample matches best is the one that was hit, at the step's frame.
        let (best, (corr, lag)) = kit
            .iter()
            .enumerate()
            .map(|(pad, s)| (pad, correlate(&audio, frame, s)))
            .max_by(|a, b| a.1.0.total_cmp(&b.1.0))
            .unwrap();
        assert_eq!(best, row, "step {}: pad {} expected", step + 1, row + 1);
        assert!(corr > 0.9, "step {}: correlation {corr}", step + 1);
        assert_eq!(lag, 0, "step {}: the hit starts {lag} frames off", step + 1);
    }
    // An off step starts nothing: before the first hit and in the gap after the kick's tail the
    // render is quiet.
    assert!(audio[..steps[0]].iter().all(|v| v.abs() < 1e-6));
}
