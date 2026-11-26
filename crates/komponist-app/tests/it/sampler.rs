//! The Sampler in the app (WP-87): the device card with its pads, waveform and knobs, sample drop
//! and the file browser, links in the project file (absolute and relative), "Sammeln und
//! speichern", missing samples with "Suchen …", and the pad selection that follows the step
//! sequencer and the input.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{PATIENCE, app_with, authored_project, scratch, step_until};
use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange, TransportAction};
use komponist_app::device_host::PluginHost;
use komponist_app::ui_state::{DockTab, StepCursor};
use komponist_app::{Action, App, PluginAction, ProjectAction, UiAction};
use komponist_audiofile::fixtures as testkit;
use komponist_audiofile::{FilePool, PeakState};
use komponist_i18n::Lang;
use komponist_ipc::device_assets::AssetStatus;
use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack, StepRowSpec};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{DeviceId, Project, Tick, TickRange, TrackId, persist};
use komponist_plugins::DeviceStatus;

/// The first of the sampler bundle's eight pad notes. `show_authored_pads` checks it against the
/// bundle's own pad bank.
const FIRST_PAD_NOTE: u8 = 36;

fn note_of_pad(pad: usize) -> u8 {
    FIRST_PAD_NOTE + pad as u8
}

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
        .get_by_role_and_label(Role::Status, h.state().lang().transport_cpu())
        .rect();
    h.mask(cpu.expand(2.0));
}

fn authored_snapshot_harness(mut app: App, lang: Lang) -> Harness<'static, App> {
    // Hold audio before the first frame so the live bridge cannot add late blocks during
    // the capture. The sampler control messages still reach the bridge.
    app.engine_mut().hold_engine().expect("engine held");
    harness(app.with_lang(lang))
}

fn snapshot_authored_pad(h: &mut Harness<'_, App>, name: &str) {
    // Bridge scheduling warnings depend on the test machine's realtime permissions.
    h.state_mut().ui_mut().status = None;
    h.step();
    mask_cpu(h);
    h.snapshot(name);
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

fn authored_app(dir: &Path, kit: bool) -> Option<App> {
    let (project, host) = authored_project(dir, kit)?;
    let mut app = app_with(project, host);
    show_authored_pads(&mut app, kit);
    Some(app)
}

fn show_authored_pads(app: &mut App, kit: bool) {
    show_devices(app);
    let (_, device) = drums(app);
    let status = if kit {
        AssetStatus::Ready
    } else {
        AssetStatus::Empty
    };
    step_until(app, "native pad bank and samples", |app| {
        let host = app.engine().host();
        host.status(device) == DeviceStatus::Ok
            && host.native_layout(device).is_some_and(|layout| {
                layout
                    .pad_bank
                    .as_ref()
                    .is_some_and(|bank| bank.pads.len() == testkit::NAMES.len())
            })
            && host.asset_slots(device).is_some_and(|assets| {
                assets.slots.len() == testkit::NAMES.len()
                    && assets.slots.iter().all(|slot| slot.status == status)
            })
    });
    let host = app.engine().host();
    let layout = host.native_layout(device).unwrap();
    let bank = layout.pad_bank.as_ref().unwrap();
    assert!(
        bank.pads
            .iter()
            .enumerate()
            .all(|(pad, p)| p.note == note_of_pad(pad)),
        "the sampler bundle's pad notes"
    );
    assert!(host.verified_bundle(device).is_none());
    assert!(host.bridge_pid(device).is_some());
}

fn wait_for_pad_sample(app: &mut App, slot: u32) {
    let (_, device) = drums(app);
    step_until(app, "the pad sample loaded", |app| {
        app.engine()
            .host()
            .asset_slots(device)
            .is_some_and(|assets| {
                assets
                    .slots
                    .iter()
                    .any(|asset| asset.id == slot && asset.status == AssetStatus::Ready)
            })
    });
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

#[test]
fn the_general_sampler_exports_a_tempo_synced_note_from_the_first_beat() {
    use crate::export;
    use komponist_app::actions::{BitDepth, ExportRange};
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
    use komponist_model::params::{Scale, to_normalized};
    use komponist_model::time::Tempo;

    const PLUGIN: &str = "org.klang-studio.komponist.general-sampler";
    let host = App::headless_plugin_host();
    if common::shipped_plugin(&host, PLUGIN).is_none() {
        assert_ne!(
            std::env::var("KOMPONIST_REQUIRE_FIXTURES").as_deref(),
            Ok("1")
        );
        eprintln!("skipping: build the verified CLAP fixtures with scripts/build-test-plugins.sh");
        return;
    }
    let prepared = host
        .manager()
        .prepare_bundled_device(
            PLUGIN,
            None,
            &[("sync", 1.0), ("mode", 0.0), ("source_bpm", 0.4)],
            &["gain"],
        )
        .unwrap();
    let (gain, initial_gain) = prepared.targets["gain"].clone();

    let dir = scratch("general-sampler-export");
    let rate = 48_000;
    let path = dir.join("Tone.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..rate {
        let phase = std::f32::consts::TAU * 220.0 * frame as f32 / rate as f32;
        writer.write_sample(0.25 * phase.sin()).unwrap();
    }
    writer.finalize().unwrap();
    let mut project = Project::new("Sampler");
    project.tempo = Tempo::new(150.0);
    let (project, track) = edits::add_track(
        &project,
        NewTrack {
            name: Some("Sampler".into()),
            hue: None,
            instrument: Some(prepared.plugin.clone()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (mut project, _) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(3840),
            name: "Phrase".into(),
            content: NewClipContent::Midi {
                notes: vec![edits::NewNote {
                    start: Tick::ZERO,
                    length: Tick(1920),
                    pitch: 72,
                    velocity: 127,
                }],
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let slot = &mut Arc::make_mut(&mut project.tracks[0]).channel.devices[0];
    let device = slot.id;
    let mut sampler = prepared.into_device(device);
    sampler.asset_links.insert(0, AssetLink::new(&path, None));
    *slot = Arc::new(sampler);
    let owner = AutomationOwner::Track(track);
    let (project, lane) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Param {
            device,
            param: gain,
        },
        "Gain".into(),
    )
    .unwrap();
    let project = edits::automation::set_lane_points(
        &project,
        owner,
        lane,
        Points::new(vec![
            Point {
                tick: Tick::ZERO,
                value: initial_gain,
                shape: Shape::Hold,
            },
            Point::linear(
                Tick(960),
                to_normalized(Scale::Db, -70.0, 12.0, -12.0) as f32,
            ),
        ]),
    )
    .unwrap();
    let mut app = App::headless(project);
    step_until(&mut app, "sampler loaded its audio asset", |app| {
        let host = app.engine().host();
        host.status(device) == DeviceStatus::Ok
            && host.asset_slots(device).is_some_and(|assets| {
                assets
                    .slots
                    .iter()
                    .any(|slot| slot.id == 0 && slot.status == AssetStatus::Ready)
            })
    });
    let settings = komponist_app::actions::ExportSettings {
        bit_depth: BitDepth::Float32,
        normalize: false,
        sample_rate: rate,
        tail_seconds: 0.0,
        ..export::export_settings(&dir, ExportRange::Song)
    };
    let output = settings.folder.join(&settings.file_name);
    export::export(&mut app, settings);
    let wav = export::read(&output);
    let (frames, _) = wav.samples.as_chunks::<2>();
    let audible = |frame: &[f32; 2]| frame[0].abs() > 1e-3;
    let first = frames.iter().position(audible).unwrap();
    let last = frames.iter().rposition(audible).unwrap();
    assert!(first < rate as usize / 20, "first audible frame: {first}");
    assert!(
        (last as i64 - 38_400).abs() < 500,
        "last audible frame: {last}"
    );
    let peak = |start: usize, end: usize| {
        wav.samples[2 * start..2 * end]
            .iter()
            .fold(0.0_f32, |peak, value| peak.max(value.abs()))
    };
    let ratio = peak(6_000, 15_000) / peak(24_000, 33_000);
    assert!((3.0..5.0).contains(&ratio), "automated gain ratio: {ratio}");
}

#[test]
fn the_card_shows_the_pads_the_waveform_and_the_knobs_of_the_selected_pad() {
    let mut results = SnapshotResults::new();
    for (lang, snapshot) in [
        (Lang::De, "sampler-card-1x"),
        (Lang::En, "sampler-card-1x-en"),
    ] {
        let dir = scratch(snapshot);
        let Some(app) = authored_app(&dir, true) else {
            return;
        };
        let (_, device) = drums(&app);
        let mut h = authored_snapshot_harness(app, lang);
        assert_kit_pad_controls(&h);
        let source = FilePool::global().open(&dir.join("Kick.wav")).unwrap();
        let deadline = Instant::now() + PATIENCE;
        while !matches!(source.peaks_state(), PeakState::Ready(_)) {
            h.step();
            assert!(Instant::now() < deadline, "Klopfer waveform did not load");
            std::thread::sleep(Duration::from_millis(5));
        }
        h.run_steps(3);
        // A pad's glow and playhead are timing-dependent, so capture before auditioning it.
        snapshot_authored_pad(&mut h, snapshot);
        results.extend_harness(&mut h);

        select_snare(&mut h);
        assert!(h.state().dispatched().any(|action| matches!(
            action, Action::Plugin(PluginAction::AuditionNote { device: id, key })
                if *id == device && *key == note_of_pad(1)
        )));
    }
    results.unwrap();
}

fn assert_kit_pad_controls(h: &Harness<'_, App>) {
    let lang = h.state().lang();
    for name in testkit::NAMES {
        h.get_by_role_and_label(Role::Button, &lang.ui_pad_name(name));
    }
    h.get_by_role_and_label(Role::Group, &lang.devices_waveform_title(1, "Kick"));
    h.get_by_role_and_label(Role::Slider, &lang.param_sampler_tune(1));
    let start = format!(
        "{} {}",
        lang.ui_graph_sample_start(),
        lang.ui_graph_position()
    );
    h.get_by_role_and_label(Role::Slider, &start);
}

fn select_snare(h: &mut Harness<'_, App>) {
    let lang = h.state().lang();
    let snare = h
        .get_by_role_and_label(Role::Button, &lang.ui_pad_name("Snare"))
        .rect()
        .center();
    h.state_mut().clear_dispatched();
    click(h, snare);
    h.get_by_role_and_label(Role::Group, &lang.devices_waveform_title(2, "Snare"));
    h.get_by_role_and_label(Role::Slider, &lang.param_sampler_tune(2));
}

#[test]
fn an_empty_sampler_names_its_pads_and_offers_the_load_menu() {
    let mut results = SnapshotResults::new();
    for (lang, snapshot) in [
        (Lang::De, "sampler-empty-1x"),
        (Lang::En, "sampler-empty-1x-en"),
    ] {
        let dir = scratch(snapshot);
        let Some(app) = authored_app(&dir, false) else {
            return;
        };
        let mut h = authored_snapshot_harness(app, lang);
        assert_empty_pad_menu(&mut h, 3);
        snapshot_authored_pad(&mut h, snapshot);
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

fn assert_empty_pad_menu(h: &mut Harness<'_, App>, number: usize) {
    let lang = h.state().lang();
    h.get_by_role_and_label(Role::Button, &lang.ui_pad_name(lang.devices_pad(1)));
    h.get_by_role_and_label(
        Role::Group,
        &lang.devices_waveform_title(1, lang.devices_no_sample()),
    );
    let pad = h
        .get_by_role_and_label(Role::Button, &lang.ui_pad_name(lang.devices_pad(number)))
        .rect()
        .center();
    h.hover_at(pad);
    h.step();
    press(h, pad, PointerButton::Secondary, true);
    press(h, pad, PointerButton::Secondary, false);
    h.run_steps(3);
    h.get_by_role_and_label(Role::MenuItem, lang.devices_load_sample());
    assert!(
        h.get_by_role_and_label(Role::MenuItem, lang.devices_remove_sample())
            .accesskit_node()
            .is_disabled()
    );
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
    let Some((project, host)) = authored_project(&dir, false) else {
        return;
    };
    let (project, clip) = add_step_clip(
        project,
        vec![StepRowSpec {
            name: "Clap".into(),
            note: note_of_pad(2),
        }],
    );
    let mut app = app_with(project, host);
    show_authored_pads(&mut app, false);
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
    let linked = &h.state().project().tracks[0].channel.devices[0].asset_links;
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[&2].abs, wav);
    assert_eq!(linked[&2].name().as_deref(), Some("Donner"));
    wait_for_pad_sample(h.state_mut(), 2);
    h.run_steps(3);
    // The pad shows its name, the step rows follow it.
    h.get_by_role_and_label(Role::Button, "Pad Donner");
    h.get_by_role_and_label(Role::Group, "Pad 3 · Donner");
    h.state_mut().dispatch(Action::Ui(UiAction::OpenClip(clip)));
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, &h.state().lang().step_seq_row("Donner"));
}

#[test]
fn the_file_browser_loads_a_sample_into_the_pad() {
    let dir = scratch("sampler-browser");
    let Some((project, host)) = authored_project(&dir, false) else {
        return;
    };
    // The browser opens in the project's folder: save the project there first.
    let file = dir.join("Song.komponist");
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    let mut app = app_with(project, host);
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    show_authored_pads(&mut app, false);
    let wav = dir.join("Klatsch.wav");
    testkit::write_wav(&wav, &vec![0.5; 2400], 48_000).unwrap();
    let mut h = harness(app);
    assert_empty_pad_menu(&mut h, 5);
    let load = h
        .get_by_role_and_label(Role::MenuItem, "Sample laden …")
        .rect()
        .center();
    click(&mut h, load);
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
    click(&mut h, row);
    assert_eq!(
        h.get_by_role_and_label(Role::ListItem, "Klatsch.wav")
            .accesskit_node()
            .is_selected(),
        Some(true)
    );
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    let linked = &h.state().project().tracks[0].channel.devices[0].asset_links;
    assert_eq!(linked.len(), 1);
    // The project has a file: the link also holds the path relative to it.
    assert_eq!(linked[&4].rel, Path::new("Klatsch.wav"));
    wait_for_pad_sample(h.state_mut(), 4);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Pad Klatsch");
    assert!(h.query_by_role_and_label(Role::List, "Dateien").is_none());
}

#[test]
fn collect_and_save_also_copies_the_files_of_audio_clips() {
    let dir = scratch("audio-collect");
    let library = dir.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let take = library.join("take 1.wav");
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    std::fs::write(&take, wav).unwrap();
    let gone = library.join("gone.wav");
    let link = |path: &Path| komponist_model::AudioFile::new(path, None, 48_000, 48_000, 2);
    let (p, track) = edits::add_audio_track(&Project::new("Audio"), Default::default()).unwrap();
    let clip = |start: i64, path: &Path| NewClip {
        start: Tick(start),
        length: Tick(960),
        name: "Take".into(),
        content: NewClipContent::Audio {
            file: link(path),
            offset: 0,
        },
    };
    let (p, first) = edits::add_clip(&p, track, clip(0, &take)).unwrap();
    let inactive = library.join("take 2.wav");
    let mut other_wav = std::fs::read(&take).unwrap();
    other_wav.extend_from_slice(b"inactive take");
    std::fs::write(&inactive, other_wav).unwrap();
    let (p, _) = edits::add_audio_take(
        &p,
        first,
        komponist_model::AudioTake {
            name: "Take 2".into(),
            file: link(&inactive),
            offset: 0,
        },
    )
    .unwrap();
    let p = edits::select_audio_take(&p, first, 0).unwrap();
    let (p, _) = edits::add_clip(&p, track, clip(960, &gone)).unwrap();
    // A file that only looks like audio by its name must never be copied next to the project.
    let secret = library.join("secret.wav");
    std::fs::write(&secret, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
    let (p, _) = edits::add_clip(&p, track, clip(1920, &secret)).unwrap();
    let home = dir.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    let file = home.join("Aufnahme.komponist");
    std::fs::write(&file, persist::to_json(&p).unwrap()).unwrap();
    let mut app = App::headless(Project::new("leer"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    let folder = home.join("Aufnahme Samples");
    assert!(folder.join("take 1.wav").is_file());
    assert!(
        folder.join("take 2.wav").is_file(),
        "inactive audio take is collected"
    );
    assert!(library.join("take 1.wav").is_file(), "the original stays");
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let files: Vec<_> = saved.tracks[0]
        .clips
        .iter()
        .map(|c| match &c.content {
            komponist_model::ClipContent::Audio(a) => a.file.clone(),
            _ => unreachable!(),
        })
        .collect();
    let komponist_model::ClipContent::Audio(audio) = &saved.tracks[0].clips[0].content else {
        unreachable!()
    };
    assert_eq!(audio.takes[1].file.abs, folder.join("take 2.wav"));
    assert_eq!(files[0].abs, folder.join("take 1.wav"));
    assert_eq!(files[0].rel, Path::new("Aufnahme Samples/take 1.wav"));
    // A file that is missing keeps its link.
    assert_eq!(files[1].abs, gone);
    assert!(!folder.join("gone.wav").exists());
    assert_eq!(files[2].abs, secret);
    assert!(
        !folder.join("secret.wav").exists(),
        "not audio: not collected"
    );
}

/// The kit's sampler as an external fixture, with a step clip of one row per pad.
fn step_clip_project(dir: &Path) -> Option<(Project, PluginHost, komponist_model::ClipId)> {
    let (project, host) = authored_project(dir, true)?;
    let rows = testkit::NAMES
        .iter()
        .enumerate()
        .map(|(pad, &name)| StepRowSpec {
            name: name.into(),
            note: note_of_pad(pad),
        })
        .collect();
    let (project, clip) = add_step_clip(project, rows);
    Some((project, host, clip))
}

fn add_step_clip(p: Project, rows: Vec<StepRowSpec>) -> (Project, komponist_model::ClipId) {
    let track = p.tracks[0].id;
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
fn native_pad_selection_follows_the_step_rows_by_note() {
    let dir = scratch("native-pad-step-sync");
    let Some((project, host)) = authored_project(&dir, true) else {
        return;
    };
    let mut rows: Vec<_> = testkit::NAMES
        .iter()
        .enumerate()
        .map(|(pad, &name)| StepRowSpec {
            name: name.into(),
            note: note_of_pad(pad),
        })
        .collect();
    // Keep the same notes in a different order from the pad grid.
    rows.swap(3, 5);
    let (project, clip) = add_step_clip(project, rows);
    let mut app = app_with(project, host);
    show_authored_pads(&mut app, true);
    let (_, device) = drums(&app);
    let h = assert_pad_step_sync(app, clip, 5, 3);
    assert!(h.state().dispatched().any(|action| matches!(
        action, Action::Plugin(PluginAction::AuditionNote { device: id, key })
            if *id == device && *key == note_of_pad(5)
    )));
}

fn assert_pad_step_sync(
    mut app: App,
    clip: komponist_model::ClipId,
    hat_row: usize,
    tom_row: usize,
) -> Harness<'static, App> {
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
            row: hat_row,
            step: None,
        })));
    h.run_steps(4);
    assert_eq!(selected_pad(&h), "Pad Closed Hat");
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
            row: tom_row,
            step: None
        })
    );
    h
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

#[test]
fn pad_folgt_eingang_selects_the_pad_a_note_hit() {
    let dir = scratch("sampler-follow");
    let Some(app) = authored_app(&dir, true) else {
        return;
    };
    let (track, device) = drums(&app);
    let host = app.engine().host();
    let layout = host.native_layout(device).unwrap();
    let bank = layout.pad_bank.as_ref().unwrap();
    let id = bank.follow_param.unwrap();
    let signal = bank.activity_tap.clone().unwrap();
    let count = bank.pads.len();
    let mut h = harness(app);
    let play = |h: &mut Harness<'_, App>, pad: usize| {
        let key = note_of_pad(pad);
        h.state_mut()
            .dispatch(Action::Transport(TransportAction::LiveNote {
                track: Some(track),
                key,
                velocity: 100,
            }));
        let deadline = Instant::now() + PATIENCE;
        loop {
            h.step();
            let seen = h
                .state()
                .engine()
                .host()
                .signal_tap(device, &signal)
                .and_then(|tap| tap.pad_activity(count))
                .is_some_and(|(_, activity)| activity.hit != 0 && activity.last_pad == pad);
            if seen {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Klopfer did not report the pad hit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        h.run_steps(3);
        h.state_mut()
            .dispatch(Action::Transport(TransportAction::LiveNote {
                track: Some(track),
                key,
                velocity: 0,
            }));
    };
    // Off: a hit on pad 6 leaves the selection alone.
    assert_eq!(h.state().engine().host().param_base(device, id), Some(0.0));
    play(&mut h, 5);
    assert_eq!(selected_pad(&h), "Pad Kick");
    // On: the next hit (pad 7) selects its pad.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id,
            value: 1.0,
        }));
    step_until(h.state_mut(), "pad follow enabled", |app| {
        app.engine().host().param_base(device, id) == Some(1.0)
    });
    h.run_steps(3);
    play(&mut h, 6);
    assert_eq!(selected_pad(&h), "Pad Rim");
}

// ------------------------------------------------------------------------------------------------
// The beat in an offline render

/// The rate of the kit's files: the render runs at it, so the samples play without resampling and
/// the onsets compare 1:1.
const RATE: u32 = testkit::RATE;
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

/// Renders `project` (one sampler track) through the app's export at [`RATE`]; returns the mono
/// mix and the first frame of each 1/16 step.
fn render(project: Project, host: PluginHost, dir: &Path) -> (Vec<f32>, Vec<usize>) {
    use komponist_app::actions::{ExportRange, ExportSettings};
    use komponist_engine::export::ExportJob;
    let mut app = app_with(project, host);
    show_authored_pads(&mut app, true);
    app.dispatch(Action::Project(ProjectAction::StartExport(
        ExportSettings {
            range: ExportRange::Song,
            sample_rate: RATE,
            tail_seconds: 0.5,
            normalize: false,
            file_name: "beat.wav".into(),
            ..komponist_app::settings::default_export("Beat", dir.join("out"))
        },
    )));
    step_until(&mut app, "end of the export", |app| !app.is_exporting());
    let mut wav = hound::WavReader::open(dir.join("out").join("beat.wav")).expect("wav");
    let spec = wav.spec();
    assert_eq!((spec.sample_rate, spec.channels), (RATE, 2));
    let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
    let samples: Vec<f32> = wav
        .samples::<i32>()
        .map(|sample| sample.unwrap() as f32 / scale)
        .collect();
    let mono = samples.chunks(2).map(|f| (f[0] + f[1]) / 2.0).collect();
    let map = &app.engine().program().expect("a program").tempo_map;
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
    let Some((mut project, host, clip)) = step_clip_project(&dir) else {
        return;
    };
    // The beat: one hit per step in `BEAT`.
    for (row, step) in BEAT {
        project = edits::set_step(&project, clip, row, step, edits::StepProp::On(true)).unwrap();
    }
    let (audio, steps) = render(project, host, &dir);
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
