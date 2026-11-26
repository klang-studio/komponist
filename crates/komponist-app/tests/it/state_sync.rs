//! Tracked preset, paste, and asset-link edits reach the running processor through
//! `EngineSync::sync_device_states`, including on undo and redo. Current-format tests use real
//! CLAP processors. The legacy Hexa and unreadable-state tests cover the compatibility paths
//! used when opening older projects.

use crate::common::*;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_app::device_host::{BuiltinHost, DeviceHost, is_hexa};
use komponist_app::engine_sync::{EngineSync, MidiSetup, SyncEvent};
use komponist_audio::{HostKind, StreamConfig};
use komponist_audiofile::fixtures as testkit;
use komponist_builtin::Internal;
use komponist_engine::Command;
use komponist_ipc::control::RenderMode;
use komponist_ipc::device_assets::AssetStatus;
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Document, Project, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

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
fn follow(sync: &mut EngineSync, doc: &Document, before: &Project) {
    sync.sync(doc);
    sync.sync_device_states(before, doc.project(), None);
}

fn undo(sync: &mut EngineSync, doc: &mut Document) {
    let before = Arc::clone(doc.project());
    assert!(doc.undo());
    follow(sync, doc, &before);
}

fn redo(sync: &mut EngineSync, doc: &mut Document) {
    let before = Arc::clone(doc.project());
    assert!(doc.redo());
    follow(sync, doc, &before);
}

/// Render a live note and drain its release before probing again.
fn note_peak_with_tail(sync: &mut EngineSync, track: TrackId, key: u8, tail_frames: usize) -> f32 {
    sync.send(Command::LiveNote {
        track,
        key,
        velocity: 100,
    });
    let (l, r) = sync.render_held(9600).expect("held engine");
    sync.send(Command::LiveNote {
        track,
        key,
        velocity: 0,
    });
    sync.render_held(tail_frames);
    l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn note_peak(sync: &mut EngineSync, track: TrackId) -> f32 {
    note_peak_with_tail(sync, track, 60, 96_000)
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
    komponist_hexa::encode_state(&komponist_hexa::preset_patch(preset).unwrap())
}

#[test]
fn undo_and_redo_of_a_legacy_hexa_patch_change_reach_the_running_processor() {
    let project = demo_project();
    let (track, synth) = hexa_track(&project);
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(BuiltinHost::new()));
    sync.sync(&doc);
    sync.hold_engine().expect("engine held");

    // A patch with a clearly different level from the first.
    let (a, b) = (0, 17);
    let before = set_state(&mut doc, synth, hexa_state(a));
    follow(&mut sync, &doc, &before);
    let peak_a = note_peak(&mut sync, track);
    let before = set_state(&mut doc, synth, hexa_state(b));
    follow(&mut sync, &doc, &before);
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
fn undo_and_redo_of_a_legacy_hexa_knob_keep_a_held_note_sounding() {
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
        komponist_hexa::decode_state(device.state.as_deref()).unwrap()
    };
    // A sustaining patch, so the note is heard as long as it is held.
    let before = set_state(&mut doc, synth, hexa_state(0));
    follow(&mut sync, &doc, &before);
    let original = patch(&doc);

    sync.send(Command::LiveNote {
        track,
        key: 60,
        velocity: 100,
    });
    assert!(quietest(&mut sync, 9600) > 1e-3, "the note sounds");
    let reloads = sync.device_reloads();

    // A tracked knob change (as a paste over or a cancelled gesture would be), undo, redo.
    let volume = komponist_hexa::device::param(&komponist_model::params::ParamId::new("volume"))
        .unwrap()
        .field;
    let mut turned = original;
    volume.set(&mut turned, volume.get(&original) * 0.5);
    let before = set_state(&mut doc, synth, komponist_hexa::encode_state(&turned));
    follow(&mut sync, &doc, &before);
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
    follow(&mut sync, &doc, &before);
    assert_eq!(sync.device_reloads(), reloads + 1);
    undo(&mut sync, &mut doc);
    assert_eq!(sync.device_reloads(), reloads + 2);
}

/// Capture the device's own bytes and the host-owned parameter bases together.
fn snapshot(sync: &mut EngineSync, device: DeviceId) -> SavedPluginState {
    sync.host_mut().request_state(device);
    let mut snapshot = None;
    // A bridge defers a capture while an edit is queued for its audio processor.
    // Poll until it publishes the snapshot taken after delivering the edit.
    wait(sync, "the device state snapshot", |_, events| {
        snapshot = events.iter().rev().find_map(|event| match event {
            PluginManagerEvent::StateSnapshot { device: id, state } if *id == device => {
                Some(state.clone())
            }
            _ => None,
        });
        snapshot.is_some()
    });
    snapshot.unwrap()
}

fn apply_snapshot(
    sync: &mut EngineSync,
    doc: &mut Document,
    device: DeviceId,
    state: &SavedPluginState,
) {
    let before = Arc::clone(doc.project());
    doc.apply("Zustand", |p| {
        edits::set_saved_plugin_state(p, device, state)
    })
    .unwrap();
    follow(sync, doc, &before);
}

fn wait_for_state(sync: &mut EngineSync, device: DeviceId) {
    wait(sync, "the device applied the state", |_, events| {
        events.iter().any(|event| {
            matches!(event, PluginManagerEvent::StateApplied { device: applied } if *applied == device)
        })
    });
}

/// Keep the live processors in offline mode, like an export. The probes render the held engine
/// offline (`render_held`), so they wait for every bridge block instead of a deadline (#347).
fn hold_offline(sync: &mut EngineSync, devices: &[DeviceId]) {
    sync.hold_engine().expect("engine held");
    sync.host_mut().set_render_mode(RenderMode::Offline);
    wait(sync, "the processors' offline render mode", |sync, _| {
        devices
            .iter()
            .all(|&device| sync.device_status(device) == DeviceStatus::Ok)
    });
}

#[test]
fn undo_and_redo_of_clap_effect_state_reach_the_running_processor() {
    const LIMITER_ID: &str = "org.klang-studio.komponist.limiter";
    let Some(mut dirs) = clap_fixture_dirs("state-sync-clap", "limiter.clap") else {
        return;
    };
    let Some(synth_dirs) = fixture_dirs() else {
        return;
    };
    dirs.clap.extend(synth_dirs.clap);

    let dir = scratch("undo-limiter");
    let host = scan_fixture_host(&dir, &dirs);
    let (project, track, synth) = instrument_project("State sync", clap_plugin(SYNTH));
    let plugin = host
        .catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == LIMITER_ID)
        .expect("the CLAP limiter")
        .plugin
        .clone();
    let (project, limiter) = edits::insert_device(
        &project,
        ChannelRef::Master,
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin,
            state: None,
            sidechain: None,
        },
    )
    .unwrap();
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(host));
    sync.sync(&doc);
    wait(&mut sync, "the synth and limiter", |sync, _| {
        [synth, limiter]
            .into_iter()
            .all(|device| sync.device_status(device) == DeviceStatus::Ok)
    });
    wait(&mut sync, "the limiter layout", |sync, _| {
        sync.host().native_layout(limiter).is_some()
    });
    let (gain, ceiling) = {
        let ids = sync
            .host()
            .native_layout(limiter)
            .unwrap()
            .author_ids()
            .unwrap();
        (ids["gain"], ids["ceiling"])
    };
    let capture = |sync: &mut EngineSync, value: f64| {
        assert!(sync.host_mut().set_param(limiter, ceiling, value));
        snapshot(sync, limiter)
    };
    // The normalized gain maximum is +24 dB. Ceiling endpoints are -12 dB and 0 dB.
    assert!(sync.host_mut().set_param(limiter, gain, 1.0));
    let loud = capture(&mut sync, 1.0);
    let quiet = capture(&mut sync, 0.0);
    assert_ne!(loud, quiet);
    hold_offline(&mut sync, &[synth, limiter]);

    apply_snapshot(&mut sync, &mut doc, limiter, &loud);
    wait_for_state(&mut sync, limiter);
    let loud_peak = note_peak(&mut sync, track);
    apply_snapshot(&mut sync, &mut doc, limiter, &quiet);
    wait_for_state(&mut sync, limiter);
    let quiet_peak = note_peak(&mut sync, track);
    assert!(
        (0.20..=0.26).contains(&quiet_peak),
        "−12 dB ceiling: {quiet_peak}"
    );
    assert!(
        loud_peak > quiet_peak * 1.5,
        "loud {loud_peak}, quiet {quiet_peak}"
    );

    let reloads = sync.device_reloads();
    undo(&mut sync, &mut doc);
    wait_for_state(&mut sync, limiter);
    let peak = note_peak(&mut sync, track);
    assert!(peak > quiet_peak * 1.5, "undo: the old ceiling ({peak})");
    redo(&mut sync, &mut doc);
    wait_for_state(&mut sync, limiter);
    let peak = note_peak(&mut sync, track);
    assert!(
        (0.20..=0.26).contains(&peak),
        "redo: the new ceiling ({peak})"
    );
    assert_eq!(sync.device_reloads(), reloads);
}

#[test]
fn undo_and_redo_of_asset_links_reach_the_clap_sampler() {
    const SLOTS: u32 = 8;
    let dir = scratch("undo-sampler");
    let Some((host, plugin)) =
        native_fixture(&dir, "klopfer.clap", "org.klang-studio.komponist.sampler")
    else {
        return;
    };
    let (project, track, drums) = instrument_project("State sync", plugin);
    // The two files have the same short waveform at different levels. The running
    // processor's peak distinguishes their identities independently of the project's links.
    let write_sample = |name: &str, amplitude: f32| {
        let path = dir.join(name);
        let samples: Vec<_> = (0..2048)
            .map(|frame| (std::f32::consts::TAU * frame as f32 / 64.0).sin() * amplitude)
            .collect();
        testkit::write_wav(&path, &samples, testkit::RATE).unwrap();
        AssetLink::new(&path, None)
    };
    let original = write_sample("Original.wav", 0.125);
    let replacement = write_sample("Replacement.wav", 0.5);
    let links: BTreeMap<_, _> = (0..SLOTS).map(|slot| (slot, original.clone())).collect();
    let project = edits::set_device_asset_links(&project, drums, links.clone()).unwrap();
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(host));
    sync.sync(&doc);
    wait(&mut sync, "the sampler pad layout", |sync, _| {
        sync.device_status(drums) == DeviceStatus::Ok
            && sync
                .host()
                .native_layout(drums)
                .is_some_and(|layout| layout.pad_bank.is_some())
    });
    let pads = sync
        .host()
        .native_layout(drums)
        .unwrap()
        .pad_bank
        .as_ref()
        .unwrap()
        .pads
        .clone();
    assert_eq!(pads.len(), SLOTS as usize);
    hold_offline(&mut sync, &[drums]);

    let expect_slots = |sync: &mut EngineSync, target: u32, expected: AssetStatus| {
        wait(
            sync,
            &format!("slot {target} to be {expected:?} and the rest ready"),
            |sync, _| {
                sync.host().asset_slots(drums).is_some_and(|snapshot| {
                    snapshot.slots.len() == SLOTS as usize
                        && snapshot
                            .slots
                            .iter()
                            .any(|slot| slot.id == target && slot.status == expected)
                        && snapshot
                            .slots
                            .iter()
                            .all(|slot| slot.id == target || slot.status == AssetStatus::Ready)
                })
            },
        );
    };
    let apply_links = |sync: &mut EngineSync, doc: &mut Document, links: &BTreeMap<_, _>| {
        let before = Arc::clone(doc.project());
        doc.apply("Dateien", |p| {
            edits::set_device_asset_links(p, drums, links.clone())
        })
        .unwrap();
        follow(sync, doc, &before);
    };
    let peak = |sync: &mut EngineSync, key| note_peak_with_tail(sync, track, key, 256);

    for pad in pads {
        let slot = pad.asset_slot;
        expect_slots(&mut sync, slot, AssetStatus::Ready);
        let reference = peak(&mut sync, pad.note);
        assert!(
            reference > 1e-3,
            "original asset in slot {slot}: {reference}"
        );

        let mut cleared = links.clone();
        assert!(cleared.remove(&slot).is_some());
        apply_links(&mut sync, &mut doc, &cleared);
        expect_slots(&mut sync, slot, AssetStatus::Empty);
        undo(&mut sync, &mut doc);
        expect_slots(&mut sync, slot, AssetStatus::Ready);
        let restored = peak(&mut sync, pad.note);
        assert!(
            (restored / reference - 1.0).abs() < 0.03,
            "undo must restore the original asset in slot {slot}: {restored} vs {reference}"
        );
        redo(&mut sync, &mut doc);
        expect_slots(&mut sync, slot, AssetStatus::Empty);

        let mut relinked = cleared;
        relinked.insert(slot, replacement.clone());
        apply_links(&mut sync, &mut doc, &relinked);
        expect_slots(&mut sync, slot, AssetStatus::Ready);
        let relinked = peak(&mut sync, pad.note);
        assert!(
            (relinked / reference - 4.0).abs() < 0.12,
            "relink must load the replacement in slot {slot}: {relinked} vs {reference}"
        );
        undo(&mut sync, &mut doc);
        expect_slots(&mut sync, slot, AssetStatus::Empty);
        redo(&mut sync, &mut doc);
        expect_slots(&mut sync, slot, AssetStatus::Ready);
        let redone = peak(&mut sync, pad.note);
        assert!(
            (redone / reference - 4.0).abs() < 0.12,
            "redo must load the replacement in slot {slot}: {redone} vs {reference}"
        );

        apply_links(&mut sync, &mut doc, &links);
        expect_slots(&mut sync, slot, AssetStatus::Ready);
    }
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
                SyncEvent::Engine(_) => None,
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
    let host = scan_fixture_host(&dir, &dirs);
    let (project, _, synth) = instrument_project("State sync", clap_plugin(SYNTH));
    let mut doc = Document::new(project);
    let mut sync = null_sync(Box::new(host));
    sync.sync(&doc);
    wait(&mut sync, "the plugin", |sync, _| {
        sync.device_status(synth) == DeviceStatus::Ok
    });
    assert!(sync.host().bridge_pid(synth).is_some());
    let gain = sync
        .host()
        .params(synth)
        .unwrap()
        .iter()
        .find(|p| p.name == "Gain")
        .expect("a gain parameter")
        .id;

    // Take both states from the plugin, then drive the running instance with the document.
    assert!(sync.host_mut().set_param(synth, gain, 0.9));
    let loud = snapshot(&mut sync, synth);
    assert!(sync.host_mut().set_param(synth, gain, 0.3));
    let quiet = snapshot(&mut sync, synth);
    assert_ne!(loud, quiet);

    apply_snapshot(&mut sync, &mut doc, synth, &loud);
    wait_for_state(&mut sync, synth);
    assert_eq!(snapshot(&mut sync, synth), loud);
    apply_snapshot(&mut sync, &mut doc, synth, &quiet);
    wait_for_state(&mut sync, synth);
    assert_eq!(snapshot(&mut sync, synth), quiet);

    undo(&mut sync, &mut doc);
    wait_for_state(&mut sync, synth);
    assert_eq!(snapshot(&mut sync, synth), loud, "undo: the old state");
    redo(&mut sync, &mut doc);
    wait_for_state(&mut sync, synth);
    assert_eq!(snapshot(&mut sync, synth), quiet, "redo: the new state");
}

/// A corrupted state, or one a newer build wrote, plays as the defaults, but the host reports it
/// (device warning plus a state log event, which the app turns into a status message), the document
/// keeps the bytes and no knob turn saves defaults over them (#224).
#[test]
fn an_unreadable_device_state_is_reported_and_never_overwritten() {
    use komponist_builtin::BuiltinKind;
    use komponist_model::PluginRef;
    use komponist_plugins::manager::STATE_LOG_PREFIX;

    let corrupted: &[u8] = b"{\"params\": {\"gain\": ";
    let newer: &[u8] = br#"{"version":9,"params":"packed-v9"}"#;
    let base = demo_project()
        .tracks
        .iter()
        .flat_map(|track| &track.channel.devices)
        .find(|device| is_hexa(device))
        .cloned()
        .expect("the demo has a Hexa");
    let device_for = |plugin: PluginRef, state: &[u8], id: u64| komponist_model::Device {
        id: DeviceId(id),
        plugin,
        state: Some(state.to_vec().into()),
        ..(*base).clone()
    };
    let setup = komponist_plugins::ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: 512,
    };
    let mut host = BuiltinHost::new();
    let mut id = 900u64;
    for state in [corrupted, newer] {
        for plugin in [
            Internal::Limiter.plugin_ref(),
            komponist_hexa::CATALOG.plugin_ref(),
        ] {
            id += 1;
            let device = device_for(plugin, state, id);
            assert!(host.create_processor(&device, setup).is_some());
            assert!(host.state_warning(device.id).is_some(), "{state:?}");
            let logged = host.poll().into_iter().any(|e| {
                matches!(&e, PluginManagerEvent::Log { device: Some(d), message, .. }
                    if *d == device.id && message.starts_with(STATE_LOG_PREFIX))
            });
            assert!(logged, "state log event for {state:?}");
            // The stored bytes are untouched and no knob turn replaces them.
            assert_eq!(device.state.as_deref(), Some(state));
            let kind = BuiltinKind::from_plugin(&device.plugin).unwrap();
            assert!(kind.state_error(device.state.as_deref()).is_some());
        }
    }
    let limiter_gain = Internal::Limiter
        .wire(&komponist_model::params::ParamId::new("gain"))
        .unwrap();
    for state in [corrupted, newer] {
        assert_eq!(
            Internal::Limiter.with_value(Some(state), limiter_gain, 6.0),
            None
        );
    }

    // A readable state, and a device without one, carry no warning; releasing clears it.
    let fine = Internal::Limiter
        .with_value(None, limiter_gain, 6.0)
        .unwrap();
    let device = device_for(Internal::Limiter.plugin_ref(), &fine, 950);
    host.create_processor(&device, setup).unwrap();
    assert_eq!(host.state_warning(device.id), None);
    host.release(DeviceId(901));
    assert_eq!(host.state_warning(DeviceId(901)), None);
}

/// A restore failure must preserve the original bytes while the user leaves the controls alone.
/// A deliberate edit may recover the device and record freshly edited state.
#[test]
fn invalid_clap_state_survives_until_a_deliberate_edit() {
    use komponist_app::actions::DropTarget;
    use komponist_app::{Action, App, PluginAction};

    let corrupted: &[u8] = b"{\"params\": {\"gain\": ";
    let mut app = App::headless(Project::new("Unreadable"));
    let shipped = |app: &App, id| crate::common::shipped_plugin(app.engine().host(), id).unwrap();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "org.klang-studio.komponist.hexa"),
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "org.klang-studio.komponist.limiter"),
        target: DropTarget::Track(track),
    }));
    let devices: Vec<DeviceId> = app.project().tracks[0]
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(devices.len(), 2);
    for &device in &devices {
        crate::common::wait_for_layout(&mut app, device);
    }
    let hexa_volume = crate::common::author_parameter(&app, devices[0], "volume");
    let limiter_gain = crate::common::author_parameter(&app, devices[1], "gain");
    let knobs = [(devices[0], hexa_volume), (devices[1], limiter_gain)];
    let state = |app: &App, device: DeviceId| {
        let (_, _, d) = app.project().find_device(device).unwrap();
        d.state.as_deref().map(<[u8]>::to_vec)
    };
    // The same actions do reach the controls of readable instances.
    for (device, id) in knobs {
        let initial = app.engine().host().param_base(device, id).unwrap();
        let value = if initial < 0.5 { 0.75 } else { 0.25 };
        app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
        crate::common::step_until(&mut app, "the live edit on readable state", |app| {
            app.engine()
                .host()
                .param_value(device, id)
                .is_some_and(|param| (param.value - value).abs() < 1e-6)
        });
    }
    let mut project = Arc::clone(app.project());
    for &device in &devices {
        project = Arc::new(
            edits::set_device_state(&project, device, Some(corrupted.to_vec().into())).unwrap(),
        );
    }
    let mut app = App::headless(Arc::unwrap_or_clone(project));
    crate::common::step_until(&mut app, "both CLAP restore warnings", |app| {
        devices
            .iter()
            .all(|&device| app.engine().host().state_warning(device).is_some())
    });
    // Outlast the one-second persistence debounce before checking the unreadable bytes.
    crate::common::step_for(&mut app, Duration::from_secs(2));
    for &device in &devices {
        assert_eq!(
            state(&app, device).as_deref(),
            Some(corrupted),
            "{device:?}"
        );
    }
    assert_eq!(app.doc().undo_label(), None);
    for (device, id) in knobs {
        let initial = app.engine().host().param_base(device, id).unwrap();
        let value = if initial < 0.5 { 0.75 } else { 0.25 };
        app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
        crate::common::step_until(&mut app, "the deliberate recovery edit", |app| {
            app.engine().host().state_warning(device).is_none()
                && state(app, device).is_some_and(|bytes| bytes.as_slice() != corrupted)
        });
    }
}
