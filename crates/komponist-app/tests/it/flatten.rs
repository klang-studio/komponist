//! Flattening through the app's normal action and the arrangement context menu.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use egui::accesskit::Role;
use egui::{Event as UiEvent, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::device_host::DeviceHost;
use komponist_app::project_io::{load_project, save_project};
use komponist_app::ui_state::{FocusArea, StatusKind};
use komponist_app::{Action, App, EditAction, ProjectAction};
use komponist_engine::testing::{TestMidiFx, TestPanic};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_i18n::Lang;
use komponist_ipc::RenderMode;
use komponist_ipc::device_assets::{AssetKind, AssetStatus, Slot, Snapshot};
use komponist_model::edits::{self, NewBus, NewClip, NewClipContent, NewDevice, NewNote, NewTrack};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{
    ChannelRef, ClipContent, Device, DeviceId, DeviceRole, PluginFormat, PluginRef, Project,
    Sidechain, Tap, Tick, TrackId, TrackKind,
};
use komponist_plugins::{
    Catalog, DeviceSnapshots, DeviceStatus, PluginManagerEvent, ProcessorSetup,
};

use crate::common::{
    SYNTH, app_with, clap_plugin, fixture_dirs, instrument_project, native_fixture,
    scan_fixture_host,
};
use crate::export::{read, step_until};

const FIRST: Tick = Tick(1920);
const SECOND: Tick = Tick(4800);
const LENGTH: Tick = Tick(480);
const TAIL: usize = 115_200; // 2.4 s at 48 kHz
const ECHO_GAP: usize = 192_000; // 4 s at 48 kHz

fn plugin(id: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: id.into(),
        name: id.into(),
        vendor: "Test".into(),
        version: "1".into(),
        module: "".into(),
    }
}

pub(super) fn clip(
    project: &Project,
    track: TrackId,
    start: Tick,
    pitch: u8,
    length: Tick,
) -> Project {
    edits::add_clip(
        project,
        track,
        NewClip {
            start,
            length,
            name: "Notes".into(),
            content: NewClipContent::Midi {
                notes: vec![NewNote {
                    start: Tick::ZERO,
                    length,
                    pitch,
                    velocity: 100,
                }],
                take_name: String::new(),
            },
        },
    )
    .unwrap()
    .0
}

fn fixture(midi_effect: Option<PluginRef>) -> (Project, TrackId, DeviceId) {
    let (project, side) = edits::add_track(
        &Project::new("Flatten"),
        NewTrack {
            name: Some("Control".into()),
            instrument: Some(plugin("instrument")),
            hue: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, track) = edits::add_track(
        &project,
        NewTrack {
            name: Some("Lead".into()),
            instrument: Some(plugin("instrument")),
            arm_exclusive: true,
            hue: None,
            index: None,
        },
    )
    .unwrap();
    let project = if let Some(plugin) = midi_effect {
        edits::insert_device(
            &project,
            ChannelRef::Track(track),
            0,
            NewDevice {
                role: DeviceRole::MidiEffect,
                plugin,
                state: None,
                sidechain: None,
                preset: None,
            },
        )
        .unwrap()
        .0
    } else {
        project
    };
    let device_count = project.track(track).unwrap().channel.devices.len();
    let (project, effect) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        device_count,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("tail"),
            state: Some(Arc::from(&b"before"[..])),
            sidechain: Some(Sidechain {
                source: side,
                tap: Tap::Pre,
            }),
            preset: None,
        },
    )
    .unwrap();
    let project = clip(&project, side, Tick(960), 80, Tick(7000));
    let project = clip(&project, track, FIRST, 61, LENGTH); // The MIDI stage maps this C♯ to C in the full-chain tests.
    let project = clip(&project, track, SECOND, 62, LENGTH);
    let (project, bus) = edits::add_bus(&project, NewBus::default()).unwrap();
    let project =
        edits::set_output(&project, &[track], komponist_model::TrackOutput::Bus(bus)).unwrap();
    let (project, send) = edits::add_bus(&project, NewBus::default()).unwrap();
    let (mut project, _) = edits::add_send(&project, track, send).unwrap();
    let source = project.tracks.iter_mut().find(|t| t.id == track).unwrap();
    let channel = &mut Arc::make_mut(source).channel;
    channel.volume_db = -12.0;
    channel.pan = 0.75;
    channel.mute = true;
    Arc::make_mut(&mut project.master).mute = true;
    assert!(
        matches!(project.track(track).unwrap().output, komponist_model::TrackOutput::Bus(id) if id == bus)
    );
    (project, track, effect)
}

enum TestProcessor {
    Instrument(f32),
    Tail {
        value: f32,
        remaining: usize,
        delay: usize,
    },
    Broken,
    /// Panics in the one-frame render that ends a flatten (outside the render's
    /// `assert_no_alloc`, whose allocator aborts on a panic's allocation).
    Panic(TestPanic),
}

impl Processor for TestProcessor {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }
    fn latency(&self) -> u32 {
        0
    }
    fn reset(&mut self) {
        match self {
            Self::Instrument(level) => *level = 0.0,
            Self::Tail {
                value, remaining, ..
            } => {
                *value = 0.0;
                *remaining = 0;
            }
            Self::Broken | Self::Panic(_) => {}
        }
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        match self {
            Self::Instrument(level) => {
                let mut events = events.iter().peekable();
                for i in 0..ctx.frames as usize {
                    while events
                        .peek()
                        .is_some_and(|event| event.offset as usize == i)
                    {
                        match events.next().unwrap().kind {
                            EventKind::NoteOn { key, .. } => *level = f32::from(key) / 100.0,
                            EventKind::NoteOff { .. } | EventKind::AllNotesOff => *level = 0.0,
                            _ => {}
                        }
                    }
                    io.output[0][i] = *level;
                    io.output[1][i] = *level;
                }
                ProcessStatus::Continue
            }
            Self::Tail { .. } => {
                for channel in &mut io.output {
                    channel.fill(0.0);
                }
                ProcessStatus::Continue
            }
            Self::Panic(panic) if ctx.offline && ctx.frames == 1 => panic.process(ctx, events, io),
            Self::Panic(_) => {
                for channel in &mut io.output {
                    channel.fill(0.0);
                }
                ProcessStatus::Continue
            }
            Self::Broken if ctx.offline => ProcessStatus::Failed,
            Self::Broken => ProcessStatus::Continue,
        }
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        side: [&[f32]; 2],
    ) -> ProcessStatus {
        let Self::Tail {
            value,
            remaining,
            delay,
        } = self
        else {
            return self.process(ctx, events, io);
        };
        for (i, &side_sample) in side[0][..ctx.frames as usize].iter().enumerate() {
            let dry = io.input[0][i] * side_sample;
            let sample = if dry != 0.0 {
                *value = dry;
                *remaining = TAIL + *delay;
                if *delay == 0 { dry } else { 0.0 }
            } else if *remaining > 0 {
                *remaining -= 1;
                if *remaining < TAIL {
                    *value * (*remaining as f32 / TAIL as f32) * 0.25
                } else {
                    0.0
                }
            } else {
                0.0
            };
            io.output[0][i] = sample;
            io.output[1][i] = sample;
        }
        ProcessStatus::Continue
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fixture {
    Ready,
    Broken,
    Panic,
    Loading,
    WasNotSnapshotted,
    DelayedEcho,
}

struct Host {
    backend: Box<dyn DeviceHost>,
    state: DeviceId,
    fixture: Fixture,
    assets: Option<Snapshot>,
    bindings_pending: bool,
    global_mode_changes: Arc<AtomicUsize>,
}

impl DeviceHost for Host {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        match device.plugin.id.as_str() {
            "instrument" => Some(Box::new(TestProcessor::Instrument(0.0))),
            "transpose" => Some(Box::new(TestMidiFx::transposing(-1))),
            "tail" if self.fixture == Fixture::Broken => Some(Box::new(TestProcessor::Broken)),
            "tail" if self.fixture == Fixture::Panic => {
                Some(Box::new(TestProcessor::Panic(TestPanic)))
            }
            "tail" => Some(Box::new(TestProcessor::Tail {
                value: 0.0,
                remaining: 0,
                delay: if self.fixture == Fixture::DelayedEcho {
                    ECHO_GAP
                } else {
                    0
                },
            })),
            _ => self.backend.create_processor(device, setup),
        }
    }
    fn release(&mut self, device: DeviceId) {
        self.backend.release(device);
    }
    fn status(&self, device: DeviceId) -> DeviceStatus {
        if self.fixture == Fixture::Loading && self.state == device {
            DeviceStatus::Loading
        } else {
            self.backend.status(device)
        }
    }
    fn latency(&self, device: DeviceId) -> u32 {
        self.backend.latency(device)
    }
    fn asset_slots(&self, device: DeviceId) -> Option<&Snapshot> {
        self.assets.as_ref().filter(|_| device == self.state)
    }
    fn render_asset_bindings_pending(&self, device: DeviceId) -> bool {
        self.bindings_pending && device == self.state
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.backend.poll()
    }
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.backend.role(plugin)
    }
    fn catalog(&self) -> &Catalog {
        self.backend.catalog()
    }
    fn snapshot_selected_states(
        &mut self,
        ids: &[DeviceId],
        timeout: std::time::Duration,
    ) -> Result<DeviceSnapshots, DeviceId> {
        let mut snapshots = self.backend.snapshot_selected_states(ids, timeout)?;
        if ids.contains(&self.state) {
            if self.fixture == Fixture::WasNotSnapshotted {
                return Err(self.state);
            }
            snapshots.push((
                self.state,
                Arc::<[u8]>::from(&b"latest plugin settings"[..]).into(),
            ));
        }
        Ok(snapshots)
    }
    fn set_render_mode(&mut self, _: RenderMode) {
        self.global_mode_changes.fetch_add(1, Ordering::Relaxed);
    }
    fn set_selected_render_mode(&mut self, ids: &[DeviceId], mode: RenderMode) {
        self.backend.set_selected_render_mode(ids, mode);
    }
    fn bridge_pid(&self, device: DeviceId) -> Option<u32> {
        self.backend.bridge_pid(device)
    }
    fn shutdown(&mut self) {
        self.backend.shutdown();
    }
}

fn app(project: Project, folder: &Path, effect: DeviceId, fixture: Fixture) -> App {
    app_on(
        Box::new(App::headless_plugin_host()),
        project,
        folder,
        effect,
        fixture,
    )
}

fn app_on(
    backend: Box<dyn DeviceHost>,
    project: Project,
    folder: &Path,
    effect: DeviceId,
    fixture: Fixture,
) -> App {
    app_with_host(
        project,
        folder,
        Host {
            backend,
            state: effect,
            fixture,
            assets: None,
            bindings_pending: false,
            global_mode_changes: Arc::default(),
        },
    )
}

fn app_with_host(project: Project, folder: &Path, host: Host) -> App {
    let mut app = App::headless(project)
        .with_device_host(Box::new(host))
        .with_recording_directory(folder);
    app.engine_mut().hold_engine().unwrap();
    app.step();
    app
}

pub(super) fn flatten(app: &mut App, track: TrackId) {
    app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
    step_until(app, "flatten", |app| !app.is_exporting());
}

fn audio_path(app: &App, track: TrackId) -> PathBuf {
    let new_track = app.project().track(track).unwrap();
    assert_eq!(new_track.kind, TrackKind::Audio, "{:?}", app.ui().status);
    assert_eq!(new_track.clips.len(), 1);
    let ClipContent::Audio(audio) = &new_track.clips[0].content else {
        panic!("audio clip")
    };
    audio.file.abs.clone()
}

#[test]
fn flatten_isolates_the_complete_chain_and_undo_restores_the_live_state() {
    let dir = tempfile::tempdir().unwrap();
    assert_complete_chain(
        dir.path(),
        plugin("transpose"),
        Box::new(App::headless_plugin_host()),
        0.61,
        false,
    );
}

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --current-projects"]
fn flatten_processes_verified_clap_scale_and_restores_its_state() {
    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("package executable");
    let dir = tempfile::tempdir().unwrap();
    let host = crate::bundled_current::scan(Path::new(&launcher), dir.path());
    let scale = crate::bundled_current::shipped(&host, "scale");
    assert!(komponist_plugins::requires_bundled(&scale));
    assert_complete_chain(dir.path(), scale, Box::new(host), 0.62, true);
}

fn assert_complete_chain(
    dir: &Path,
    midi_effect: PluginRef,
    backend: Box<dyn DeviceHost>,
    second_note: f32,
    packaged: bool,
) {
    let (project, track, effect) = fixture(Some(midi_effect));
    let midi_effect = project.track(track).unwrap().channel.devices[0].id;
    let count = Arc::new(AtomicUsize::new(0));
    let mut app = app_with_host(
        project.clone(),
        &dir.join("render"),
        Host {
            backend,
            state: effect,
            fixture: Fixture::Ready,
            assets: None,
            bindings_pending: false,
            global_mode_changes: Arc::clone(&count),
        },
    );
    step_until(&mut app, "the MIDI stage ready", |app| {
        app.engine().device_status(midi_effect) == DeviceStatus::Ok
    });
    if packaged {
        assert!(app.engine().host().bridge_pid(midi_effect).is_none());
    }
    flatten(&mut app, track);
    assert_eq!(
        count.load(Ordering::Relaxed),
        0,
        "flatten must never switch the whole host"
    );

    let path = audio_path(&app, track);
    let old = project.track(track).unwrap();
    let new = app.project().track(track).unwrap();
    assert_eq!(project.track_index(track), app.project().track_index(track));
    assert_eq!(new.name, old.name);
    assert_eq!(new.hue, old.hue);
    assert_eq!(new.output, old.output);
    assert_eq!(new.sends, old.sends);
    let mut expected_channel = old.channel.clone();
    expected_channel.devices.clear();
    assert_eq!(new.channel, expected_channel);
    assert!(!new.arm && !new.monitor);
    assert_eq!(new.clips[0].start, FIRST);

    let wav = read(&path);
    assert_eq!(wav.spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(wav.spec.channels, 2);
    let rate = f64::from(wav.spec.sample_rate);
    let relative = |tick| {
        (project.tempo_map().tick_to_sample(tick, rate)
            - project.tempo_map().tick_to_sample(FIRST, rate)) as usize
    };
    let left = |frame: usize| wav.samples[frame * 2];
    assert!(
        (left(128) - 0.60 * 0.80).abs() < 1e-5,
        "sidechain, MIDI effect and instrument"
    );
    assert!(
        (left(relative(SECOND) + 128) - second_note * 0.80).abs() < 1e-5,
        "second clip at its original offset"
    );
    let end = relative(SECOND + LENGTH);
    assert!(
        left(end + 2 * wav.spec.sample_rate as usize) > 0.005,
        "tail past the normal WAV export default"
    );
    assert!(wav.frames as usize > end + TAIL);

    assert_eq!(app.doc().undo_label(), Some("undo-flatten-track"));
    app.dispatch(Action::Edit(EditAction::Undo));
    let restored = app.project().track(track).unwrap();
    assert_eq!(restored.kind, TrackKind::Midi);
    assert_eq!(restored.clips, old.clips);
    assert_eq!(restored.channel.devices.len(), old.channel.devices.len());
    if packaged {
        assert!(
            app.project()
                .find_device(midi_effect)
                .unwrap()
                .2
                .state
                .is_some()
        );
    }
    assert_eq!(
        app.project()
            .find_device(effect)
            .unwrap()
            .2
            .state
            .as_deref(),
        Some(b"latest plugin settings".as_slice())
    );
    assert!(app.doc().undo_label().is_none());
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(audio_path(&app, track), path);
    assert!(!app.engine().is_detached());
}

#[test]
fn a_failed_render_leaves_the_original_project_history_and_no_wav() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let mut app = app(project.clone(), dir.path(), effect, Fixture::Broken);
    flatten(&mut app, track);
    assert_eq!(app.project().as_ref(), &project);
    assert!(!app.doc().is_dirty());
    assert!(app.doc().undo_label().is_none());
    assert!(dir.path().read_dir().unwrap().next().is_none());
    assert_eq!(app.ui().status.as_ref().unwrap().kind, StatusKind::Error);
    assert!(!app.engine().is_detached());
}

/// #211: a panic on the render worker leaves the original project, a rebuilt engine in the
/// session and the localized status.
#[test]
fn a_panic_in_the_flatten_render_rebuilds_the_engine() {
    for lang in [Lang::De, Lang::En] {
        let dir = tempfile::tempdir().unwrap();
        let (project, track, effect) = fixture(None);
        let mut app = app(project.clone(), dir.path(), effect, Fixture::Panic).with_lang(lang);
        // Single-threaded, so the render panics on the flatten worker itself.
        app.engine_mut().set_workers(Some(0)).unwrap();
        flatten(&mut app, track);
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.doc().undo_label().is_none());
        assert!(dir.path().read_dir().unwrap().next().is_none());
        assert_eq!(
            app.ui().status.as_ref().map(|s| (s.text.clone(), s.kind)),
            Some((
                lang.flatten_failed(lang.export_worker_crashed()),
                StatusKind::Error
            ))
        );
        assert!(!app.engine().is_detached());
        assert_eq!(app.engine().pending_commands(), 0);
        // The engine plays again (the transport starts).
        app.engine_mut().hold_engine().unwrap();
        app.dispatch(Action::Transport(
            komponist_app::TransportAction::TogglePlay,
        ));
        app.step();
        assert!(app.engine_mut().run_held(4_800, &[], 0));
        assert!(app.engine().playhead() > Tick::ZERO);
    }
}

#[test]
fn a_failed_live_snapshot_blocks_replacing_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let mut app = app(
        project.clone(),
        dir.path(),
        effect,
        Fixture::WasNotSnapshotted,
    );
    flatten(&mut app, track);
    assert_eq!(app.project().as_ref(), &project);
    assert!(app.doc().undo_label().is_none());
    assert!(!app.engine().is_detached());
    assert!(dir.path().read_dir().unwrap().next().is_none());
    let lang = app.lang();
    assert_eq!(
        app.ui().status.as_ref().unwrap().text,
        lang.flatten_failed(lang.flatten_state_failed("tail"))
    );
}

#[test]
fn flatten_keeps_a_wet_only_echo_after_four_seconds_of_silence() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let mut app = app(project.clone(), dir.path(), effect, Fixture::DelayedEcho);
    flatten(&mut app, track);
    let wav = read(&audio_path(&app, track));
    assert_eq!(wav.spec.sample_rate, 48_000);
    let body = project
        .tempo_map()
        .tick_to_sample(SECOND + LENGTH - FIRST, 48_000.0) as usize;
    let echo = (body + ECHO_GAP) * 2;
    assert!(wav.samples[..echo].iter().all(|&sample| sample == 0.0));
    assert!(
        wav.samples[echo..echo + 512]
            .iter()
            .any(|&sample| sample > 0.08)
    );
}

#[test]
fn flatten_checks_linked_audio_assets_without_an_authored_layout() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let slot = 42;
    let asset = AssetLink::new(&dir.path().join("corrupt.wav"), None);
    let project = edits::set_device_asset_links(&project, effect, [(slot, asset)].into()).unwrap();

    for (status, bindings_pending) in [
        (AssetStatus::Empty, false),
        (AssetStatus::Loading, false),
        (AssetStatus::Missing, false),
        (AssetStatus::Failed("Unsupported audio".into()), false),
        (AssetStatus::Ready, true),
        (AssetStatus::Ready, false),
    ] {
        let mut app = app_with_host(
            project.clone(),
            dir.path(),
            Host {
                backend: Box::new(App::headless_plugin_host()),
                state: effect,
                fixture: Fixture::Ready,
                assets: Some(Snapshot {
                    slots: vec![Slot {
                        id: slot,
                        role: "sample".into(),
                        label: "Source".into(),
                        kind: AssetKind::AudioSample,
                        status: status.clone(),
                    }],
                }),
                bindings_pending,
                global_mode_changes: Arc::default(),
            },
        );
        assert!(app.engine().host().native_layout(effect).is_none());
        app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
        match (status, bindings_pending) {
            (status, true) | (status @ (AssetStatus::Empty | AssetStatus::Loading), false) => {
                app.step();
                assert!(app.is_exporting());
                assert!(!app.engine().is_detached(), "{status:?}");
                app.dispatch(Action::Project(ProjectAction::CancelExport));
            }
            (AssetStatus::Missing | AssetStatus::Failed(_), false) => {
                step_until(&mut app, "reject the unavailable asset", |app| {
                    !app.is_exporting()
                });
                let lang = app.lang();
                assert_eq!(
                    app.ui().status.as_ref().unwrap().text,
                    lang.flatten_failed(lang.flatten_sampler_failed("tail", "corrupt"))
                );
            }
            (AssetStatus::Ready, false) => {
                step_until(&mut app, "flatten with the ready asset", |app| {
                    !app.is_exporting()
                });
                assert!(audio_path(&app, track).is_file());
                continue;
            }
        }
        assert_eq!(app.project().as_ref(), &project);
        assert!(dir.path().read_dir().unwrap().next().is_none());
    }
}

#[test]
fn an_unreadable_clap_pad_blocks_flattening() {
    let dir = tempfile::tempdir().unwrap();
    let Some((host, plugin)) = native_fixture(
        dir.path(),
        "klopfer.clap",
        "org.klang-studio.komponist.sampler",
    ) else {
        return;
    };
    let device_name = plugin.name.clone();
    let (project, track, drums) = instrument_project("Sampler", plugin);
    let sample = dir.path().join("corrupt.wav");
    std::fs::write(&sample, b"unreadable sample").unwrap();
    let project =
        edits::set_device_asset_links(&project, drums, [(0, AssetLink::new(&sample, None))].into())
            .unwrap();
    let project = clip(&project, track, FIRST, 36, LENGTH);
    let output = dir.path().join("render");
    let mut app = app_with(project, host).with_recording_directory(&output);
    step_until(&mut app, "the unreadable CLAP asset", |app| {
        let host = app.engine().host();
        host.native_layout(drums).is_some()
            && host.asset_slots(drums).is_some_and(|inventory| {
                inventory
                    .slots
                    .iter()
                    .any(|slot| slot.id == 0 && matches!(slot.status, AssetStatus::Failed(_)))
            })
    });
    let project = Arc::clone(app.project());
    flatten(&mut app, track);
    let lang = app.lang();
    assert_eq!(
        app.ui().status.as_ref().unwrap().text,
        lang.flatten_failed(lang.flatten_pad_failed(&device_name, 1, "corrupt"))
    );
    assert_eq!(app.project(), &project);
    assert!(app.doc().undo_label().is_none());
    assert!(output.read_dir().unwrap().next().is_none());
}

#[test]
fn a_warp_failure_names_the_sidechain_clip_in_both_languages() {
    let dir = tempfile::tempdir().unwrap();
    let sample = dir.path().join("invalid.wav");
    let mut writer = hound::WavWriter::create(
        &sample,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for _ in 0..48_000 {
        writer.write_sample(2048.0_f32).unwrap();
    }
    writer.finalize().unwrap();
    let (project, track, effect) = fixture(None);
    let (project, sidechain) =
        edits::add_audio_track(&project, edits::NewAudioTrack::default()).unwrap();
    let (project, clip) = edits::add_clip(
        &project,
        sidechain,
        NewClip {
            start: FIRST,
            length: Tick(960),
            name: "Warp control".into(),
            content: NewClipContent::Audio {
                file: komponist_model::AudioFile::new(&sample, None, 48_000, 48_000, 1),
                offset: 0,
            },
        },
    )
    .unwrap();
    let project = edits::set_audio_clip_warp(&project, clip, true).unwrap();
    let project = edits::set_sidechain(&project, effect, Some(Sidechain::new(sidechain))).unwrap();
    for lang in [Lang::De, Lang::En] {
        let output = dir.path().join(lang.code());
        let mut app = app(project.clone(), &output, effect, Fixture::Ready).with_lang(lang);
        flatten(&mut app, track);
        assert_eq!(app.project().as_ref(), &project);
        assert!(output.read_dir().unwrap().next().is_none());
        assert_eq!(
            app.ui().status.as_ref().unwrap().text,
            lang.flatten_failed(lang.flatten_warp_failed("Warp control", lang.export_warp_input()))
        );
    }
}

#[test]
fn opening_a_project_cancels_flatten_without_waiting_on_the_ui_thread() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let path = dir.path().join("Next.komponist");
    save_project(&path, &Project::new("Next")).unwrap();
    let output = dir.path().join("render");
    let mut app = app(project.clone(), &output, effect, Fixture::Ready);
    app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
    assert!(app.engine().is_detached());
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
    assert_eq!(app.project().as_ref(), &project);
    assert!(app.is_exporting());
    step_until(&mut app, "pending project open", |app| {
        app.project_path() == Some(path.as_path())
    });
    assert!(!app.engine().is_detached());
    assert!(!app.is_exporting());
    assert!(output.read_dir().unwrap().next().is_none());
}

#[test]
fn finishing_flatten_keeps_the_quit_prompt_and_its_save_action() {
    let dir = tempfile::tempdir().unwrap();
    let (project, track, effect) = fixture(None);
    let path = dir.path().join("Work.komponist");
    save_project(&path, &project).unwrap();
    let mut app = app(Project::new(""), dir.path(), effect, Fixture::Ready);
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
    app.step();
    app.dispatch(Action::Project(ProjectAction::ContinueWithMissing));
    app.dispatch(Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track],
        prop: edits::TrackProp::Name("Edited lead".into()),
    }));
    app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
    assert!(app.is_exporting());
    assert!(!app.close_requested());
    step_until(&mut app, "flatten finished below quit prompt", |app| {
        !app.is_exporting()
    });
    assert_eq!(app.ui().focus, FocusArea::Dialog);
    assert!(app.ui().dialog.is_none());
    assert!(audio_path(&app, track).is_file());
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1440.0, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h.get_by_value("Änderungen speichern?");
    h.get_by_label("Speichern").click();
    h.run_steps(3);
    assert!(h.state_mut().close_requested());
    assert_eq!(
        load_project(&path).unwrap().track(track).unwrap().kind,
        TrackKind::Audio
    );
}

#[test]
fn a_saved_render_reopens_from_the_project_takes_folder() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let (mut project, track, effect) = fixture(None);
    let source = Arc::make_mut(project.tracks.iter_mut().find(|t| t.id == track).unwrap());
    source.channel.mute = false;
    source.output = komponist_model::TrackOutput::Master;
    Arc::make_mut(&mut project.master).mute = false;
    let path = dir.join("Flatten.komponist");
    save_project(&path, &project).unwrap();
    let mut app = app(Project::new(""), dir, effect, Fixture::Ready);
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
    app.step();
    app.dispatch(Action::Project(ProjectAction::ContinueWithMissing));
    flatten(&mut app, track);
    let file = audio_path(&app, track);
    assert!(file.starts_with(dir.join("Takes")));
    let control = app.project().tracks[0].id;
    app.dispatch(Action::Edit(EditAction::RemoveTracks(vec![control])));
    app.dispatch(Action::Project(ProjectAction::Save));
    let loaded = load_project(&path).unwrap();
    let mut reopened = App::headless(loaded);
    reopened.engine_mut().hold_engine().unwrap();
    reopened.step();
    assert_eq!(audio_path(&reopened, track), file);
    let settings = crate::export::export_settings(dir, komponist_app::actions::ExportRange::Song);
    crate::export::export(&mut reopened, settings);
    let played = read(&reopened.exported()[0]);
    let first = project
        .tempo_map()
        .tick_to_sample(FIRST, f64::from(played.spec.sample_rate)) as usize;
    assert!(
        played.samples[..2 * first]
            .iter()
            .all(|sample| sample.abs() < 0.001)
    );
    assert!(
        played.samples[2 * first..]
            .iter()
            .any(|sample| sample.abs() > 0.01)
    );
}

fn click_at(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton) {
    h.event(UiEvent::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(UiEvent::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(3);
}

#[test]
fn first_save_moves_a_render_even_when_it_is_only_in_redo_history() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let host = scan_fixture_host(dir, &dirs);
    let (project, track, synth) = instrument_project("Unsaved render", clap_plugin(SYNTH));
    let project = clip(&project, track, FIRST, 60, LENGTH);
    let mut app = app_with(project, host).with_recording_directory(dir.join("provisional"));
    step_until(&mut app, "the test synth ready", |app| {
        app.engine().device_status(synth) == DeviceStatus::Ok
    });
    flatten(&mut app, track);
    let file = audio_path(&app, track);
    let wav = read(&file);
    assert!(wav.samples.iter().any(|sample| sample.abs() > 0.01));
    app.dispatch(Action::Edit(EditAction::Undo));
    let path = dir.join("Demo.komponist");
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1440.0, 900.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::SaveAs));
    h.run_steps(3);
    h.get_by_label("Dateiname").focus();
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Dateiname")
        .type_text(path.to_str().unwrap());
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    let app = h.state_mut();
    assert_eq!(app.project_path(), Some(path.as_path()));
    assert_eq!(app.project().track(track).unwrap().kind, TrackKind::Midi);
    assert!(!file.exists());
    app.dispatch(Action::Edit(EditAction::Redo));
    let migrated = audio_path(app, track);
    assert!(migrated.starts_with(dir.join("Takes")));
    assert_eq!(read(&migrated).samples, wav.samples);
}

#[test]
fn the_localized_track_menu_opens_a_cancellable_flatten_dialog() {
    let mut results = SnapshotResults::new();
    for (lang, name) in [(Lang::De, "flatten-wait-de"), (Lang::En, "flatten-wait-en")] {
        let dir = tempfile::tempdir().unwrap();
        let (project, track, effect) = fixture(None);
        // The reference screenshots list the in-binary devices in the browser (#294 regenerates
        // them from the packaged catalogue).
        let app = app_on(
            Box::new(App::headless_plugin_host()),
            project.clone(),
            dir.path(),
            effect,
            Fixture::Loading,
        )
        .with_lang(lang);
        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(vec2(1440.0, 900.0))
            .with_options(SnapshotOptions::new().output_path("tests/snapshots"))
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run_steps(3);
        let row = h
            .get_all_by_role(Role::Row)
            .find(|node| {
                node.accesskit_node().label().is_some_and(|label| {
                    label.starts_with(if lang == Lang::De {
                        "Spur Lead"
                    } else {
                        "Track Lead"
                    })
                })
            })
            .unwrap()
            .rect();
        click_at(&mut h, row.center(), PointerButton::Secondary);
        let item = h
            .get_by_role_and_label(Role::MenuItem, lang.arrangement_flatten_track())
            .rect();
        click_at(&mut h, item.center(), PointerButton::Primary);
        h.get_by_label(&lang.flatten_title("Lead"));
        h.get_by_label(lang.flatten_waiting());
        if let Some(cpu) = h.query_by_role_and_label(Role::Status, lang.transport_cpu()) {
            h.mask(cpu.rect().expand(2.0));
        }
        h.snapshot(name);
        let button = h
            .get_by_role_and_label(Role::Button, lang.dialog_cancel())
            .rect();
        click_at(&mut h, button.center(), PointerButton::Primary);
        assert!(!h.state().is_exporting());
        assert_eq!(h.state().project().as_ref(), &project);
        assert!(h.state().project().track(track).unwrap().kind == TrackKind::Midi);
        assert!(dir.path().read_dir().unwrap().next().is_none());
        results.extend_harness(&mut h);
    }
}

#[test]
fn oversized_clip_automation_reports_the_limit_before_rendering() {
    use komponist_model::automation::{
        AutomationMode, AutomationOwner, AutomationTarget, Point, Points,
    };

    let (project, track, _) = fixture(None);
    let clip = project.track(track).unwrap().clips[0].id;
    let project = edits::set_clip_loop(&project, clip, Some(Tick(2)), Tick(1_000_000_000)).unwrap();
    let target = AutomationTarget::Pan;
    let project =
        edits::automation::add_clip_automation(&project, clip, target.clone(), "Pan".into())
            .unwrap();
    let project = edits::automation::set_clip_points(
        &project,
        clip,
        &target,
        Points::new(vec![
            Point::linear(Tick::ZERO, 0.4),
            Point::linear(Tick(1), 0.9),
        ]),
    )
    .unwrap();
    let owner = AutomationOwner::Track(track);
    let lane = project
        .track(track)
        .unwrap()
        .automation
        .lane(&target)
        .unwrap()
        .id;
    let project =
        edits::automation::set_lane_mode(&project, owner, lane, AutomationMode::Off).unwrap();
    for lang in [Lang::De, Lang::En] {
        let mut app = App::headless(project.clone()).with_lang(lang);
        app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
        let status = app.ui().status.as_ref().unwrap();
        assert_eq!(status.kind, StatusKind::Error);
        assert_eq!(
            status.text,
            lang.flatten_failed(lang.flatten_automation_too_large())
        );
        assert!(!app.is_exporting());
        assert_eq!(app.project().as_ref(), &project);
    }
}
