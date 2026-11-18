//! Flattening through the app's normal action and the arrangement context menu.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event as UiEvent, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::device_host::{BuiltinHost, DeviceHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::ui_state::StatusKind;
use komponist_app::{Action, App, EditAction, ProjectAction};
use komponist_builtin::Internal;
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_i18n::Lang;
use komponist_model::edits::{self, NewBus, NewClip, NewClipContent, NewDevice, NewNote, NewTrack};
use komponist_model::{
    ChannelRef, ClipContent, Device, DeviceId, DeviceRole, PluginFormat, PluginRef, Project,
    Sidechain, Tap, Tick, TrackId, TrackKind,
};
use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};

use crate::export::{read, scratch, step_until};

const FIRST: Tick = Tick(1920);
const SECOND: Tick = Tick(4800);
const LENGTH: Tick = Tick(480);
const TAIL: usize = 115_200; // 2.4 s at 48 kHz

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

fn clip(project: &Project, track: TrackId, start: Tick, pitch: u8, length: Tick) -> Project {
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

fn fixture() -> (Project, TrackId, DeviceId) {
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
    let (project, _) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            role: DeviceRole::MidiEffect,
            plugin: Internal::Tonleiter.plugin_ref(),
            state: None,
            sidechain: None,
        },
    )
    .unwrap();
    let (project, effect) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        2,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("tail"),
            state: Some(Arc::from(&b"before"[..])),
            sidechain: Some(Sidechain {
                source: side,
                tap: Tap::Pre,
            }),
        },
    )
    .unwrap();
    let project = clip(&project, side, Tick(960), 80, Tick(7000));
    let project = clip(&project, track, FIRST, 61, LENGTH); // Tonleiter maps this C♯ to C.
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
    Tail { value: f32, remaining: usize },
    Broken,
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
            Self::Tail { value, remaining } => {
                *value = 0.0;
                *remaining = 0;
            }
            Self::Broken => {}
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
        let Self::Tail { value, remaining } = self else {
            return self.process(ctx, events, io);
        };
        for i in 0..ctx.frames as usize {
            let dry = io.input[0][i] * side[0][i];
            let sample = if dry != 0.0 {
                *value = dry;
                *remaining = TAIL;
                dry
            } else if *remaining > 0 {
                *remaining -= 1;
                *value * (*remaining as f32 / TAIL as f32) * 0.25
            } else {
                0.0
            };
            io.output[0][i] = sample;
            io.output[1][i] = sample;
        }
        ProcessStatus::Continue
    }
}

struct Host {
    builtins: BuiltinHost,
    state: Option<DeviceId>,
    fail: bool,
    loading: bool,
}

impl DeviceHost for Host {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        match device.plugin.id.as_str() {
            "instrument" => Some(Box::new(TestProcessor::Instrument(0.0))),
            "tail" if self.fail => Some(Box::new(TestProcessor::Broken)),
            "tail" => Some(Box::new(TestProcessor::Tail {
                value: 0.0,
                remaining: 0,
            })),
            _ => self.builtins.create_processor(device, setup),
        }
    }
    fn release(&mut self, device: DeviceId) {
        self.builtins.release(device);
    }
    fn status(&self, device: DeviceId) -> DeviceStatus {
        if self.loading && self.state == Some(device) {
            DeviceStatus::Loading
        } else {
            self.builtins.status(device)
        }
    }
    fn latency(&self, device: DeviceId) -> u32 {
        self.builtins.latency(device)
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.builtins.poll()
    }
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.builtins.role(plugin)
    }
    fn catalog(&self) -> &Catalog {
        self.builtins.catalog()
    }
    fn snapshot_states(&mut self, _: std::time::Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        self.state
            .map(|id| (id, Arc::from(&b"latest plugin settings"[..])))
            .into_iter()
            .collect()
    }
}

fn app(project: Project, folder: &Path, effect: DeviceId, fail: bool, loading: bool) -> App {
    let host = Host {
        builtins: BuiltinHost::new(),
        state: Some(effect),
        fail,
        loading,
    };
    let mut app = App::headless(project)
        .with_device_host(Box::new(host))
        .with_recording_directory(folder);
    app.engine_mut().hold_engine().unwrap();
    app.step();
    app
}

fn flatten(app: &mut App, track: TrackId) {
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
    let dir = scratch("flatten-chain");
    let (project, track, effect) = fixture();
    let mut app = app(project.clone(), &dir, effect, false, false);
    flatten(&mut app, track);

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
        (left(relative(SECOND) + 128) - 0.62 * 0.80).abs() < 1e-5,
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
    assert_eq!(
        &**restored.channel.devices[2].state.as_ref().unwrap(),
        b"latest plugin settings"
    );
    assert!(app.doc().undo_label().is_none());
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(audio_path(&app, track), path);
    assert!(!app.engine().is_detached());
}

#[test]
fn a_failed_render_leaves_the_original_project_history_and_no_wav() {
    let dir = scratch("flatten-failure");
    let (project, track, effect) = fixture();
    let mut app = app(project.clone(), &dir, effect, true, false);
    flatten(&mut app, track);
    assert_eq!(app.project().as_ref(), &project);
    assert!(!app.doc().is_dirty());
    assert!(app.doc().undo_label().is_none());
    assert!(dir.read_dir().unwrap().next().is_none());
    assert_eq!(app.ui().status.as_ref().unwrap().kind, StatusKind::Error);
    assert!(!app.engine().is_detached());
}

#[test]
fn a_saved_render_reopens_from_the_project_takes_folder() {
    let dir = scratch("flatten-reopen");
    let (mut project, track, effect) = fixture();
    let source = Arc::make_mut(project.tracks.iter_mut().find(|t| t.id == track).unwrap());
    source.channel.mute = false;
    source.output = komponist_model::TrackOutput::Master;
    Arc::make_mut(&mut project.master).mute = false;
    let path = dir.join("Flatten.komponist");
    save_project(&path, &project).unwrap();
    let mut app = app(Project::new(""), &dir, effect, false, false);
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
    let settings = crate::export::export_settings(&dir, komponist_app::actions::ExportRange::Song);
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
fn first_save_moves_the_demo_render_even_when_it_is_only_in_redo_history() {
    let dir = scratch("flatten-first-save");
    let project = komponist_app::showcase::showcase_project();
    let track = project
        .tracks
        .iter()
        .find(|track| track.name == "Arp")
        .unwrap()
        .id;
    let mut app = App::headless(project).with_recording_directory(dir.join("provisional"));
    app.engine_mut().hold_engine().unwrap();
    app.step();
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
        let dir = scratch(name);
        let (project, track, effect) = fixture();
        let app = app(project.clone(), &dir, effect, false, true).with_lang(lang);
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
        assert!(dir.read_dir().unwrap().next().is_none());
        results.extend_harness(&mut h);
    }
}
