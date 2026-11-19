//! Undo, redo and every other tracked edit that replaces a device's state reach the running
//! processor (`EngineSync::sync_device_states`): hexa (patch), an internal effect (params),
//! the Sampler (sample links) and a bridged plugin (`SetState`). The engine is held and rendered
//! by the test, so the checks listen to the processors, not only to the document.
//!
//! A knob turn, a sample link and a plugin's own snapshot are stored outside the undo history
//! (`Document::apply_untracked`), so these tests make the tracked change themselves with
//! `edits::set_device_state`, the way a preset or a paste over does.

use crate::common::*;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_app::device_host::{BuiltinHost, DeviceHost, PluginHost, is_hexa};
use komponist_app::engine_sync::{EngineSync, MidiSetup, SyncEvent};
use komponist_audio::{HostKind, StreamConfig};
use komponist_builtin::Internal;
use komponist_builtin::sampler::{SampleLink, links, testkit};
use komponist_engine::Command;
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Document, Project, TrackId};
use komponist_plugins::PluginManagerEvent;

fn null_sync(host: Box<dyn DeviceHost>) -> EngineSync {
    let config = StreamConfig {
        host: HostKind::Null,
        buffer_frames: NULL_BUFFER,
        ..StreamConfig::default()
    };
    let (sync, started) = EngineSync::new(&config, MidiSetup::None, host);
    started.expect("null backend");
    sync
}

fn set_state(doc: &mut Document, device: DeviceId, state: Vec<u8>) -> Arc<Project> {
    let before = Arc::clone(doc.project());
    let state: Arc<[u8]> = state.into();
    doc.apply("Zustand", |p| {
        edits::set_device_state(p, device, Some(Arc::clone(&state)))
    })
    .unwrap();
    before
}

/// Sends the document to the engine and moves the engine to the document after `before`.
fn follow(sync: &mut EngineSync, doc: &Document, before: &Project, dir: Option<&Path>) {
    sync.sync(doc);
    sync.sync_device_states(before, doc.project(), dir);
}

fn undo(sync: &mut EngineSync, doc: &mut Document) {
    let before = Arc::clone(doc.project());
    assert!(doc.undo());
    follow(sync, doc, &before, None);
}

fn redo(sync: &mut EngineSync, doc: &mut Document) {
    let before = Arc::clone(doc.project());
    assert!(doc.redo());
    follow(sync, doc, &before, None);
}

/// One live note on `track`, rendered for a while: the peak of the output.
fn note_peak(sync: &mut EngineSync, track: TrackId) -> f32 {
    sync.send(Command::LiveNote {
        track,
        key: 60,
        velocity: 100,
    });
    let (l, r) = sync.run_held_capture(9600, &[], 0).expect("held engine");
    sync.send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
    // The release tail, so the next note starts from silence.
    sync.run_held(96_000, &[], 0);
    l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn hexa_track(project: &Project) -> (TrackId, DeviceId) {
    project
        .tracks
        .iter()
        .find_map(|t| {
            t.channel
                .devices
                .iter()
                .find(|d| is_hexa(d))
                .map(|d| (t.id, d.id))
        })
        .expect("a hexa track")
}

fn hexa_state(preset: usize) -> Vec<u8> {
    komponist_builtin::encode_state(&komponist_builtin::preset_patch(preset).unwrap())
}

#[test]
fn undo_and_redo_of_a_hexa_patch_change_reach_the_running_processor() {
    let project = demo_project();
    let (track, synth) = hexa_track(&project);
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(BuiltinHost::new()));
    sync.sync(&doc);
    sync.hold_engine().expect("engine held");

    // A patch with a clearly different level from the first.
    let (a, b) = (0, 17);
    let before = set_state(&mut doc, synth, hexa_state(a));
    follow(&mut sync, &doc, &before, None);
    let peak_a = note_peak(&mut sync, track);
    let before = set_state(&mut doc, synth, hexa_state(b));
    follow(&mut sync, &doc, &before, None);
    let peak_b = note_peak(&mut sync, track);
    assert!(peak_a > 0.0 && peak_b > 0.0, "{peak_a} {peak_b}");
    assert!(
        (peak_a - peak_b).abs() > 0.02 * peak_a.max(peak_b),
        "the two patches must sound different: {peak_a} {peak_b}"
    );

    undo(&mut sync, &mut doc);
    assert_eq!(note_peak(&mut sync, track), peak_a, "undo: the old patch");
    redo(&mut sync, &mut doc);
    assert_eq!(note_peak(&mut sync, track), peak_b, "redo: the new patch");
}

/// Peak of `frames` frames of the held engine, in pieces of 10 ms: the quietest piece.
fn quietest(sync: &mut EngineSync, frames: usize) -> f32 {
    let (l, r) = sync.run_held_capture(frames, &[], 0).expect("held engine");
    l.chunks(480)
        .zip(r.chunks(480))
        .map(|(l, r)| l.iter().chain(r).fold(0.0_f32, |m, s| m.max(s.abs())))
        .fold(f32::INFINITY, f32::min)
}

#[test]
fn undo_and_redo_of_a_hexa_knob_keep_a_held_note_sounding() {
    // #19: the patch is diffed into parameters, so the running processor stays and a held note
    // sounds on through undo and redo; only a change no parameter reaches makes a fresh one.
    let project = demo_project();
    let (track, synth) = hexa_track(&project);
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(BuiltinHost::new()));
    sync.sync(&doc);
    sync.hold_engine().expect("engine held");
    let patch = |doc: &Document| {
        let device = doc.project().find_device(synth).unwrap().2;
        komponist_builtin::decode_state(device.state.as_deref()).unwrap()
    };
    // A sustaining patch, so the note is heard as long as it is held.
    let before = set_state(&mut doc, synth, hexa_state(0));
    follow(&mut sync, &doc, &before, None);
    let original = patch(&doc);

    sync.send(Command::LiveNote {
        track,
        key: 60,
        velocity: 100,
    });
    assert!(quietest(&mut sync, 9600) > 1e-3, "the note sounds");
    let reloads = sync.device_reloads();

    // A tracked knob change (as a paste over or a cancelled gesture would be), undo, redo.
    let volume = komponist_builtin::device::param(&komponist_model::params::ParamId::new("volume"))
        .unwrap()
        .field;
    let mut turned = original;
    volume.set(&mut turned, volume.get(&original) * 0.5);
    let before = set_state(&mut doc, synth, komponist_builtin::encode_state(&turned));
    follow(&mut sync, &doc, &before, None);
    assert!(quietest(&mut sync, 9600) > 1e-3, "sounds after the change");
    undo(&mut sync, &mut doc);
    assert_eq!(patch(&doc), original);
    assert!(quietest(&mut sync, 9600) > 1e-3, "sounds after undo");
    redo(&mut sync, &mut doc);
    assert_eq!(patch(&doc), turned);
    assert!(quietest(&mut sync, 9600) > 1e-3, "sounds after redo");
    assert_eq!(sync.device_reloads(), reloads, "no fresh processor");

    // The same patch from a fresh processor sounds the same: the parameters arrived.
    sync.send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
    sync.run_held(96_000, &[], 0);
    let held = note_peak(&mut sync, track);
    sync.reload_device(doc.project(), synth);
    assert_eq!(
        note_peak(&mut sync, track),
        held,
        "redo reached the processor"
    );

    // A preset with other filters or effects is no parameter: a fresh processor.
    let reloads = sync.device_reloads();
    let before = set_state(&mut doc, synth, hexa_state(17));
    follow(&mut sync, &doc, &before, None);
    assert_eq!(sync.device_reloads(), reloads + 1);
    undo(&mut sync, &mut doc);
    assert_eq!(sync.device_reloads(), reloads + 2);
}

#[test]
fn undo_and_redo_of_an_internal_devices_params_reach_the_running_processor() {
    let project = demo_project();
    let (track, _) = hexa_track(&project);
    let mut doc = Document::new(project);
    // A limiter on the master: its ceiling bounds the peak of everything.
    doc.apply("Limiter", |p| {
        edits::insert_device(
            p,
            ChannelRef::Master,
            0,
            NewDevice {
                role: DeviceRole::Effect,
                plugin: Internal::Limiter.plugin_ref(),
                state: None,
                sidechain: None,
            },
        )
        .map(|(p, _)| p)
    })
    .unwrap();
    let limiter = doc.project().master.devices[0].id;
    let wire = |key: &str| {
        Internal::Limiter
            .wire(&komponist_model::params::ParamId::new(key))
            .unwrap()
    };
    let state = |ceiling: f64| {
        let state = Internal::Limiter
            .with_value(None, wire("gain"), 24.0)
            .unwrap();
        Internal::Limiter
            .with_value(Some(&state), wire("ceiling"), ceiling)
            .unwrap()
    };
    let mut sync = null_sync(Box::new(BuiltinHost::new()));
    sync.sync(&doc);
    sync.hold_engine().expect("engine held");

    let before = set_state(&mut doc, limiter, state(-0.3));
    follow(&mut sync, &doc, &before, None);
    let loud = note_peak(&mut sync, track);
    let before = set_state(&mut doc, limiter, state(-12.0));
    follow(&mut sync, &doc, &before, None);
    let quiet = note_peak(&mut sync, track);
    assert!(quiet <= 0.26, "−12 dB ceiling: {quiet}");
    assert!(loud > quiet * 1.5, "loud {loud}, quiet {quiet}");

    undo(&mut sync, &mut doc);
    let again = note_peak(&mut sync, track);
    assert!(again > quiet * 1.5, "undo: the old ceiling ({again})");
    redo(&mut sync, &mut doc);
    let again = note_peak(&mut sync, track);
    assert!(again <= 0.26, "redo: the new ceiling ({again})");
}

#[test]
fn undo_and_redo_of_a_sampler_params_and_sample_links_reach_the_sampler() {
    let dir = scratch("undo-sampler");
    let project = komponist_app::samples::demo_with_kit(demo_project(), &dir).unwrap();
    let drums = project.tracks[0].channel.devices[0].id;
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(BuiltinHost::new()));
    sync.sync(&doc);
    let loaded = |sync: &EngineSync| {
        let handle = sync.host().sampler(drums).expect("the sampler");
        assert!(handle.wait_idle(PATIENCE), "samples loading");
        handle
            .views()
            .iter()
            .map(|v| v.name.clone())
            .collect::<Vec<_>>()
    };
    let names = loaded(&sync);
    assert!(names.iter().all(|n| !n.is_empty()), "{names:?}");

    // Pad 0 loses its sample (a tracked change of the state, like a paste over would be).
    let old = doc.project().find_device(drums).unwrap().2.state.clone();
    let cleared = links::write(old.as_deref(), 0, None);
    let before = set_state(&mut doc, drums, cleared);
    follow(&mut sync, &doc, &before, Some(&dir));
    let now = loaded(&sync);
    assert!(now[0].is_empty(), "{now:?}");
    assert_eq!(now[1..], names[1..]);

    // Undo links it again, redo clears it again.
    let before = Arc::clone(doc.project());
    doc.undo();
    follow(&mut sync, &doc, &before, Some(&dir));
    assert_eq!(loaded(&sync), names, "undo: the sample is back");
    let before = Arc::clone(doc.project());
    doc.redo();
    follow(&mut sync, &doc, &before, Some(&dir));
    assert!(loaded(&sync)[0].is_empty(), "redo: the pad is empty");

    // Another link on pad 0 (a different kit sample), then undo.
    let other = SampleLink::new(&testkit::write_kit(&dir).unwrap()[3], None);
    let state = doc.project().find_device(drums).unwrap().2.state.clone();
    let relinked = links::write(state.as_deref(), 0, Some(&other));
    let before = set_state(&mut doc, drums, relinked);
    follow(&mut sync, &doc, &before, Some(&dir));
    assert_eq!(loaded(&sync)[0], other.name);
    let before = Arc::clone(doc.project());
    doc.undo();
    follow(&mut sync, &doc, &before, Some(&dir));
    assert!(loaded(&sync)[0].is_empty(), "undo: the pad is empty again");
}

/// Polls `sync` until `done` accepts it and the device events of that poll.
fn wait(
    sync: &mut EngineSync,
    what: &str,
    mut done: impl FnMut(&mut EngineSync, &[PluginManagerEvent]) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let events: Vec<PluginManagerEvent> = sync
            .poll()
            .into_iter()
            .filter_map(|e| match e {
                SyncEvent::Device(e) => Some(e),
                SyncEvent::Engine(_) | SyncEvent::PluginParameterInput { .. } => None,
            })
            .collect();
        if done(sync, &events) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn undo_and_redo_of_a_bridged_plugins_state_reach_the_plugin() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("undo-bridge");
    let mut host = PluginHost::new(config(&dir));
    host.start_scan(&dirs);
    let deadline = Instant::now() + PATIENCE;
    let mut scanned = false;
    while !scanned {
        scanned = host
            .poll()
            .iter()
            .any(|e| matches!(e, PluginManagerEvent::ScanFinished(_)));
        assert!(Instant::now() < deadline, "no scan");
        std::thread::sleep(Duration::from_millis(5));
    }
    let project = demo_with_test_plugins();
    let synth = bridged(&project)[0];
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(host));
    sync.sync(&doc);
    wait(&mut sync, "the plugin", |s, _| {
        s.device_status(synth) == komponist_plugins::DeviceStatus::Ok
    });
    let gain = sync
        .host()
        .params(synth)
        .unwrap()
        .iter()
        .find(|p| p.name == "Gain")
        .expect("a gain parameter")
        .id;

    // Two states of the plugin (its state is its parameters), taken from the plugin itself.
    let snapshot = |sync: &mut EngineSync, value: Option<f64>| -> Arc<[u8]> {
        if let Some(value) = value {
            sync.host_mut().set_param(synth, gain, value);
        }
        // Let the debounced snapshot of an earlier change fire first, so the answer to our own
        // request is the one we take.
        let flushed = Instant::now() + Duration::from_millis(1300);
        while Instant::now() < flushed {
            sync.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        sync.host_mut().request_state(synth);
        let mut state = None;
        wait(sync, "a snapshot", |_, events| {
            for e in events {
                if let PluginManagerEvent::StateSnapshot { state: s, .. } = e {
                    state = Some(Arc::clone(s));
                }
            }
            state.is_some()
        });
        state.unwrap()
    };
    let loud = snapshot(&mut sync, Some(0.9));
    let quiet = snapshot(&mut sync, Some(0.3));
    // What the plugin holds now, asked for without changing anything.
    let current = |sync: &mut EngineSync| snapshot(sync, None);
    assert_ne!(loud, quiet);

    // The document gets `loud`, then a tracked change to `quiet`: the plugin follows both ways.
    let before = Arc::clone(doc.project());
    doc.apply("Zustand", |p| {
        edits::set_device_state(p, synth, Some(Arc::clone(&loud)))
    })
    .unwrap();
    follow(&mut sync, &doc, &before, None);
    let before = set_state(&mut doc, synth, quiet.to_vec());
    follow(&mut sync, &doc, &before, None);
    assert_eq!(
        current(&mut sync),
        quiet,
        "the plugin follows the new state"
    );

    undo(&mut sync, &mut doc);
    assert_eq!(current(&mut sync), loud, "undo: the old state");
    redo(&mut sync, &mut doc);
    assert_eq!(current(&mut sync), quiet, "redo: the new state");
}
