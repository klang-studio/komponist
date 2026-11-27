//! Computer-keyboard notes retain their target across selection, arming and document changes.
use std::sync::mpsc::{self, Sender};

use egui::Key;
use egui_kittest::Harness;
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::device_host::{DeviceHost, DeviceStateHost, ParameterHost, ProcessorHost};
use komponist_app::engine_sync::EngineSync;
use komponist_app::settings::Settings;
use komponist_app::{Action, App, EditAction, ProjectAction, TransportAction, UiAction};
use komponist_engine::testing::{EventLog, Logged, TestSine};
use komponist_model::edits::{self, ExclusiveFlag, NewTrack};
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginRef, Project, TrackId};
use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};
use komponist_processor::{EventKind, Processor};

struct SineHost {
    catalog: Catalog,
    logs: Sender<EventLog>,
}
impl ProcessorHost for SineHost {
    fn create_processor(
        &mut self,
        _: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        let (mut sine, log) = TestSine::logged(4096);
        sine.activate(setup.sample_rate, setup.max_block).unwrap();
        let _ = self.logs.send(log);
        Some(Box::new(sine))
    }

    fn release(&mut self, _: DeviceId) {}

    fn status(&self, _: DeviceId) -> DeviceStatus {
        DeviceStatus::Ok
    }

    fn latency(&self, _: DeviceId) -> u32 {
        0
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        Vec::new()
    }
}

impl ParameterHost for SineHost {}

impl DeviceStateHost for SineHost {}

impl DeviceHost for SineHost {
    fn role(&self, _: &PluginRef) -> Option<DeviceRole> {
        Some(DeviceRole::Instrument)
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

fn rig() -> (Harness<'static, App>, [TrackId; 2], Vec<EventLog>) {
    let plugin = komponist_model::fixtures::demo_project().tracks[1]
        .channel
        .devices[0]
        .plugin
        .clone();
    let mut p = Project::new("Keyboard targets");
    let mut tracks = [TrackId(0); 2];
    for track in &mut tracks {
        let (next, id) = edits::add_track(
            &p,
            NewTrack {
                instrument: Some(plugin.clone()),
                name: None,
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        p = next;
        *track = id;
    }
    let (tx, rx) = mpsc::channel();
    let mut engine = EngineSync::null(Box::new(SineHost {
        catalog: Catalog::default(),
        logs: tx,
    }));
    engine.hold_engine().unwrap();
    let mut app = App::new(Document::new(p), engine, Settings::default(), None);
    app.ui_mut().keyboard_midi = true;
    select(&mut app, tracks[0]);
    let h = crate::acceptance_support::harness(app);
    let logs = rx.try_iter().collect();
    (h, tracks, logs)
}
fn select(app: &mut App, track: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
}
fn run(h: &mut Harness<'_, App>) {
    h.run_steps(2);
    assert!(h.state_mut().engine_mut().run_held(256, &[], 0));
}
fn key(h: &mut Harness<'_, App>, key: Key, down: bool) {
    if down {
        h.key_down(key);
    } else {
        h.key_up(key);
    }
    run(h);
}
fn notes(log: &mut EventLog) -> Vec<(u8, bool)> {
    log.drain()
        .into_iter()
        .filter_map(|entry| match entry {
            Logged::Event {
                kind: EventKind::NoteOn { key, .. },
                ..
            } => Some((key, true)),
            Logged::Event {
                kind: EventKind::NoteOff { key, .. },
                ..
            } => Some((key, false)),
            _ => None,
        })
        .collect()
}

#[test]
fn selection_and_arm_changes_release_the_original_instrument() {
    let (mut h, tracks, mut logs) = rig();
    assert_eq!(logs.len(), 2);
    key(&mut h, Key::A, true);
    select(h.state_mut(), tracks[1]);
    key(&mut h, Key::S, true);
    key(&mut h, Key::A, false);
    key(&mut h, Key::S, false);
    assert_eq!(notes(&mut logs[0]), [(60, true), (60, false)]);
    assert_eq!(notes(&mut logs[1]), [(62, true), (62, false)]);
    select(h.state_mut(), tracks[0]);
    key(&mut h, Key::D, true);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetExclusive {
            track: tracks[1],
            flag: ExclusiveFlag::Arm,
        }));
    key(&mut h, Key::D, false);
    assert_eq!(notes(&mut logs[0]), [(64, true), (64, false)]);
    assert!(notes(&mut logs[1]).is_empty());
}

#[test]
fn disabling_the_layer_or_deleting_a_target_clears_held_keys() {
    let (mut h, tracks, mut logs) = rig();
    key(&mut h, Key::A, true);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::ToggleKeyboardMidi));
    run(&mut h);
    assert_eq!(notes(&mut logs[0]), [(60, true), (60, false)]);
    key(&mut h, Key::A, false);
    assert!(notes(&mut logs[1]).is_empty());
    h.state_mut()
        .dispatch(Action::Ui(UiAction::ToggleKeyboardMidi));
    key(&mut h, Key::A, true);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::RemoveTracks(vec![tracks[0]])));
    run(&mut h);
    select(h.state_mut(), tracks[1]);
    h.state_mut().clear_dispatched();
    key(&mut h, Key::A, false);
    assert!(
        !h.state()
            .dispatched()
            .any(|a| matches!(a, Action::Transport(TransportAction::LiveNote { .. })))
    );
    assert!(notes(&mut logs[1]).is_empty());
}

#[test]
fn opening_a_project_that_reuses_ids_does_not_receive_old_key_releases() {
    let (mut h, _, _) = rig();
    let mut replacement = (**h.state().project()).clone();
    replacement.name = "Replacement".into();
    let dir = crate::acceptance_support::scratch("keyboard-document");
    let path = dir.join("replacement.komponist");
    komponist_app::project_io::save_project(&path, &replacement).unwrap();
    key(&mut h, Key::A, true);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenPath(path)));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().project().name != "Replacement" {
        assert!(std::time::Instant::now() < until);
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    h.state_mut().clear_dispatched();
    key(&mut h, Key::A, false);
    assert!(
        !h.state()
            .dispatched()
            .any(|a| matches!(a, Action::Transport(TransportAction::LiveNote { .. })))
    );
}
