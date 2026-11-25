//! Bridge lifecycle (PLAN WP-45 "Done when"): crash → the track continues, status crashed,
//! reload restores the state; a missing plugin keeps its state byte-identical through a save; the
//! watchdog; release, shutdown and drop end every bridge.

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use komponist_engine::Command;
use komponist_ipc::GuiTarget;
use komponist_ipc::LogLevel;
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, TrackId};
use komponist_plugins::manager::STATE_LOG_PREFIX;
use komponist_plugins::{
    DeviceStatus, FailReason, MissingPlugin, PluginError, PluginManager, PluginManagerEvent,
    SaveSnapshots,
};

/// Id of the parameter called `name`.
fn param_id(manager: &PluginManager, device: u64, name: &str) -> u32 {
    manager
        .params(DeviceId(device))
        .unwrap()
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no parameter {name}"))
        .id
}

/// Runs the rig until `condition` holds.
fn run_until(rig: &mut Rig, what: &str, mut condition: impl FnMut(&Rig) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition(rig) {
        assert!(Instant::now() < deadline, "{what}; events {:?}", rig.events);
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The next `StateSnapshot` of `device` the rig sees.
fn next_snapshot(rig: &mut Rig, device: u64) -> SavedPluginState {
    let from = rig.events.len();
    let mut found = None;
    run_until(rig, "a state snapshot", |rig| {
        found = rig.events[from..].iter().find_map(|e| match e {
            PluginManagerEvent::StateSnapshot { device: d, state } if d.0 == device => {
                Some(state.clone())
            }
            _ => None,
        });
        found.is_some()
    });
    found.unwrap()
}

/// Peak of the next `blocks` offline blocks after a note on `track`.
fn play(rig: &mut Rig, track: u64, key: u8) -> f32 {
    rig.note(track, key, 127);
    let range = rig.render(4 * BLOCK);
    rig.note(track, key, 0);
    rig.render(4 * BLOCK);
    rig.peak(range)
}

#[test]
fn restore_updates_parameter_bases_without_overwriting_a_later_edit() {
    let dir = scratch("state-param-bases");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let initial = rig.manager.param_base(device_id, gain).unwrap();
    rig.manager.request_state(device_id);
    let state = next_snapshot(&mut rig, 10);

    rig.manager.set_param(device_id, gain, 0.625);
    assert_eq!(rig.manager.param_base(device_id, gain), Some(0.625));
    assert_ne!(next_snapshot(&mut rig, 10), state);
    assert!(rig.manager.restore_saved_state(device_id, &state));
    rig.manager.request_state(device_id);
    assert_eq!(next_snapshot(&mut rig, 10), state);
    run_until(&mut rig, "the restored gain base", |rig| {
        rig.manager.param_base(device_id, gain) == Some(initial)
    });

    let from = rig.events.len();
    assert!(rig.manager.restore_saved_state(device_id, &state));
    let invalid_state = Arc::from(&b"not a state of kp-test-synth"[..]);
    assert!(rig.manager.set_state(device_id, &invalid_state));
    // The engine cannot safely deliver this edit before both earlier control requests.
    assert!(!rig.manager.edit_param_through_render(device_id, gain, 0.5));
    rig.manager.set_param(device_id, gain, 0.5);
    run_until(&mut rig, "the later edit safely captured", |rig| {
        rig.events[from..].contains(&PluginManagerEvent::StateApplied { device: device_id })
            && !rig.manager.has_pending_state_edits()
    });
    assert_eq!(rig.manager.param_base(device_id, gain), Some(0.5));
    rig.manager.request_state(device_id);
    let edited_state = next_snapshot(&mut rig, 10);
    assert_ne!(edited_state, state);

    // Restore the child's newly captured state after changing the value, so the assertion
    // observes the actual child value rather than the manager's optimistic cache.
    rig.manager.set_param(device_id, gain, 0.625);
    assert!(rig.manager.restore_saved_state(device_id, &edited_state));
    run_until(
        &mut rig,
        "the later edit recovered from the child snapshot",
        |rig| rig.manager.param_base(device_id, gain) == Some(0.5),
    );
}

#[test]
fn loading_bridge_restores_the_latest_parameter_bases() {
    let dir = scratch("loading-state-bases");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let original = rig.manager.snapshot_states(PATIENCE).pop().unwrap().1;

    rig.reload(10);
    assert_eq!(rig.manager.status(device_id), DeviceStatus::Loading);
    for value in [0.625, 0.5] {
        let state = SavedPluginState {
            bytes: original.bytes.clone(),
            parameter_bases: [(gain, value)].into(),
        };
        assert!(rig.manager.restore_saved_state(device_id, &state));
    }
    rig.wait_status(10, DeviceStatus::Ok);
    assert_eq!(rig.manager.param_base(device_id, gain), Some(0.5));
    rig.render(2 * BLOCK);
    let applied = rig.manager.snapshot_states(PATIENCE).pop().unwrap().1;
    assert_ne!(applied.bytes, original.bytes);
    assert!(applied.parameter_bases.is_empty());

    rig.reload(10);
    let bad = Arc::from(&b"invalid CLAP state"[..]);
    assert!(rig.manager.set_state(device_id, &bad));
    rig.wait_status(10, DeviceStatus::Ok);
    assert!(rig.manager.state_warning(device_id).is_some());
    assert!(rig.manager.snapshot_states(PATIENCE).is_empty());
}

#[test]
fn snapshots_wait_for_restored_parameter_bases_to_reach_audio() {
    let dir = scratch("snapshot-after-state-overlay");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let (_, original) = rig.manager.snapshot_states(PATIENCE).pop().unwrap();
    let restored = SavedPluginState {
        bytes: original.bytes.clone(),
        parameter_bases: [(gain, 0.5)].into(),
    };
    assert!(rig.manager.restore_saved_state(device_id, &restored));

    // Poll the control channel without running audio. The reply updates the host's base
    // cache even though the processor has not received it.
    let deadline = Instant::now() + PATIENCE;
    let applied = PluginManagerEvent::StateApplied { device: device_id };
    while !rig.events.contains(&applied) {
        assert!(Instant::now() < deadline, "restore was not acknowledged");
        rig.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(rig.manager.param_base(device_id, gain), Some(0.5));
    assert!(rig.manager.snapshot_states(PATIENCE).is_empty());

    rig.render(2 * BLOCK);
    let (_, captured) = rig.manager.snapshot_states(PATIENCE).pop().unwrap();
    assert_ne!(captured.bytes, original.bytes);
    assert!(captured.parameter_bases.is_empty());
}

#[test]
fn macro_snapshot_preserves_an_independent_plugin_edit() {
    let dir = scratch("macro-state-provenance");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let decay = param_id(&rig.manager, 10, "Decay");
    let base = rig.manager.param_base(device_id, gain);
    let slot = rig.slots[&device_id];
    let output = |rig: &mut Rig, value| {
        rig.send(Command::SetParam {
            slot,
            id: gain,
            value,
        });
        rig.run(4);
    };
    // The CLAP adapter exposes the fixture's gain in normalized coordinates, even though
    // the plugin's editor shows dB. These values lie exactly on its 0.1 dB steps.
    let mapped = 0.6;
    let preview = 0.4;
    let next_mapped = 0.5;
    let independent = 0.5;

    rig.manager.set_macro_params(&[(device_id, gain, mapped)]);
    output(&mut rig, mapped);
    // Simulate a native GUI temporarily taking the processor away from the mapped value.
    output(&mut rig, preview);
    rig.manager.preview_param(device_id, gain, preview);
    assert_eq!(rig.manager.param_base(device_id, gain), base);
    rig.manager.request_state(device_id);
    let state = next_snapshot(&mut rig, 10);
    assert!(rig.manager.is_macro_snapshot(device_id, &state.bytes));
    assert_eq!(state.parameter_bases[&gain], mapped);
    assert_eq!(
        rig.manager.param_value(device_id, gain).unwrap().value,
        preview
    );

    output(&mut rig, mapped);
    assert_eq!(rig.manager.param_base(device_id, gain), base);

    rig.manager
        .set_macro_params(&[(device_id, gain, next_mapped)]);
    output(&mut rig, next_mapped);
    rig.manager.set_param(device_id, decay, independent);
    rig.manager.request_state(device_id);
    let edited = next_snapshot(&mut rig, 10);
    assert!(!rig.manager.is_macro_snapshot(device_id, &edited.bytes));
    assert_eq!(rig.manager.param_base(device_id, decay), Some(independent));
    assert_eq!(rig.manager.param_base(device_id, gain), base);

    rig.manager.set_macro_params(&[]);
    assert_eq!(rig.manager.param_base(device_id, gain), Some(next_mapped));
    rig.manager.request_state(device_id);
    let state = next_snapshot(&mut rig, 10);
    assert!(rig.manager.is_macro_snapshot(device_id, &state.bytes));

    // Restore the earlier mixed snapshot after changing both parameters. StateApplied reports
    // values read from the real bridge plugin, rather than the manager's optimistic cache.
    rig.manager.set_param(device_id, gain, preview);
    rig.manager.set_param(device_id, decay, 0.75);
    let from = rig.events.len();
    assert!(rig.manager.restore_saved_state(device_id, &edited));
    run_until(&mut rig, "the restored macro and independent edit", |rig| {
        rig.events[from..].contains(&PluginManagerEvent::StateApplied { device: device_id })
    });
    for (id, expected) in [(gain, next_mapped), (decay, independent)] {
        let actual = rig.manager.param_base(device_id, id).unwrap();
        assert!(
            (actual - expected).abs() < 1e-6,
            "{id}: {actual} vs {expected}"
        );
    }
}

#[test]
fn crash_keeps_the_track_running_and_reload_restores_the_state() {
    let dir = scratch("crash");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let crash = device(11, DeviceRole::Effect, clap_plugin(CRASH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth, crash])]));
    rig.wait_status(10, DeviceStatus::Ok);
    rig.wait_status(11, DeviceStatus::Ok);
    let level = play(&mut rig, 1, 69);
    assert!((0.2..0.3).contains(&level), "through the effect: {level}");

    // A state with Crash = 0.5 (below the trigger), snapshotted as on save or GUI close. While
    // active, a value reaches the plugin with the next block that gets through, so snapshots are
    // taken until the state differs from the default one.
    rig.manager.request_state(DeviceId(11));
    let default_state = next_snapshot(&mut rig, 11);
    let crash_param = param_id(&rig.manager, 11, "Crash");
    rig.manager.set_param(DeviceId(11), crash_param, 0.5);
    run_until(&mut rig, "the echo of Crash", |rig| {
        rig.manager
            .param_value(DeviceId(11), crash_param)
            .is_some_and(|v| v.value == 0.5 && !v.text.is_empty())
    });
    let snapshot = loop {
        rig.render(BLOCK);
        rig.manager.request_state(DeviceId(11));
        let state = next_snapshot(&mut rig, 11);
        if state != default_state {
            break state;
        }
    };

    // Crash rising past 0.9 while active: SIGSEGV in the plugin's `process`.
    rig.manager.set_param(DeviceId(11), crash_param, 1.0);
    let crashed = DeviceStatus::Crashed(FailReason::Exited {
        code: None,
        signal: Some(SEGV),
    });
    rig.wait_status(11, crashed.clone());
    assert!(rig.events.contains(&PluginManagerEvent::StatusChanged {
        device: DeviceId(11),
        status: crashed.clone(),
    }));
    run_until(&mut rig, "ProcessorFailed", |rig| rig.failed(11));
    // The track keeps running: the synth plays on through the failed slot.
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Ok);
    let level = play(&mut rig, 1, 72);
    assert!(level > 0.2, "track after the crash: {level}");
    assert!(!rig.failed(10));

    // "Neu laden": a new bridge with the last snapshot.
    rig.reload(11);
    assert_eq!(rig.manager.status(DeviceId(11)), DeviceStatus::Loading);
    rig.wait_status(11, DeviceStatus::Ok);
    run_until(&mut rig, "the restored Crash value and base", |rig| {
        rig.manager.param_base(DeviceId(11), crash_param) == Some(0.5)
            && rig
                .manager
                .param_value(DeviceId(11), crash_param)
                .is_some_and(|v| v.value == 0.5)
    });
    rig.manager.request_state(DeviceId(11));
    assert_eq!(next_snapshot(&mut rig, 11), snapshot, "byte for byte");
    let level = play(&mut rig, 1, 69);
    assert!(
        (0.2..0.3).contains(&level),
        "through the reloaded effect: {level}"
    );
    rig.manager.shutdown();
}

#[test]
fn note_crash_in_one_track_leaves_the_other_playing() {
    let dir = scratch("note-crash");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let crash = device(20, DeviceRole::Instrument, clap_plugin(CRASH), None);
    let mut rig = Rig::new(
        manager,
        project(vec![track(1, vec![synth]), track(2, vec![crash])]),
    );
    rig.wait_status(10, DeviceStatus::Ok);
    rig.wait_status(20, DeviceStatus::Ok);
    // Key 127 on track 2 writes through a null pointer in the plugin.
    rig.note(2, 127, 127);
    rig.wait_status(
        20,
        DeviceStatus::Crashed(FailReason::Exited {
            code: None,
            signal: Some(SEGV),
        }),
    );
    run_until(&mut rig, "ProcessorFailed", |rig| rig.failed(20));
    let level = play(&mut rig, 1, 69);
    assert!(
        (0.2..0.3).contains(&level),
        "track 1 after the crash: {level}"
    );
    // Reload of a device whose bridge is already gone, without any snapshot: the stored state.
    rig.reload(20);
    rig.wait_status(20, DeviceStatus::Ok);
    rig.manager.shutdown();
}

#[test]
fn missing_plugin_keeps_its_state_byte_identical_through_a_save() {
    let dir = scratch("missing");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    // Every byte value, so no text encoding could survive by accident.
    let blob: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
    let gone = PluginRef {
        format: PluginFormat::Vst3,
        id: "56535456697441766974616C00000000".into(),
        name: "Vital".into(),
        vendor: "Vital Audio".into(),
        version: "1.5.5".into(),
        module: Box::default(),
    };
    let original = project(vec![
        track(
            1,
            vec![
                device(10, DeviceRole::Instrument, gone.clone(), Some(&blob)),
                device(11, DeviceRole::Effect, clap_plugin(FX), None),
            ],
        ),
        // This plugin is not installed in this manager: missing too.
        track(
            2,
            vec![device(
                20,
                DeviceRole::Instrument,
                clap_plugin("com.klopfer.drum-sampler"),
                Some(b"kit"),
            )],
        ),
        track(
            3,
            vec![device(30, DeviceRole::Instrument, clap_plugin(SYNTH), None)],
        ),
    ]);
    // Project load: from the file format.
    let text = komponist_model::persist::to_json(&original).unwrap();
    let loaded = komponist_model::persist::from_json(&text).unwrap();

    let missing = manager.missing_plugins(&loaded);
    assert_eq!(
        missing,
        [
            MissingPlugin {
                track: Some(TrackId(1)),
                device: DeviceId(10),
                role: DeviceRole::Instrument,
                index: 0,
                plugin: gone.clone(),
            },
            MissingPlugin {
                track: Some(TrackId(2)),
                device: DeviceId(20),
                role: DeviceRole::Instrument,
                index: 0,
                plugin: clap_plugin("com.klopfer.drum-sampler"),
            },
        ]
    );

    let mut rig = Rig::new(manager, loaded.clone());
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Missing);
    assert_eq!(rig.manager.status(DeviceId(20)), DeviceStatus::Missing);
    let placeholder = find_device(&loaded, DeviceId(10)).clone();
    assert_eq!(
        rig.manager.create_processor(&placeholder, SETUP).err(),
        Some(PluginError::NotInstalled(Box::new(gone)))
    );
    rig.wait_status(30, DeviceStatus::Ok);
    rig.wait_status(11, DeviceStatus::Ok);
    assert!(rig.events.contains(&PluginManagerEvent::StatusChanged {
        device: DeviceId(10),
        status: DeviceStatus::Missing,
    }));
    // The placeholder instrument is silent; the effect after it runs.
    let level = play(&mut rig, 1, 60);
    assert_eq!(level, 0.0);
    let gain = param_id(&rig.manager, 30, "Gain");
    rig.manager.set_param(DeviceId(30), gain, 0.5);
    run_until(&mut rig, "the edited gain", |rig| {
        !rig.manager.has_pending_state_edits()
    });

    // Save: snapshots of the running devices go into the document (untracked), then the file.
    let mut document = Document::new(loaded);
    let states = rig.manager.snapshot_states(PATIENCE);
    let devices: Vec<u64> = states.iter().map(|(d, _)| d.0).collect();
    assert_eq!(devices, [11, 30]);
    for (device, state) in states {
        document
            .apply_untracked(|p| komponist_model::edits::set_saved_plugin_state(p, device, &state))
            .unwrap();
    }
    let saved = komponist_model::persist::to_json(document.project()).unwrap();
    let reopened = komponist_model::persist::from_json(&saved).unwrap();
    let state = |id| find_device(&reopened, DeviceId(id)).state.clone();
    assert_eq!(state(10).as_deref(), Some(&blob[..]), "byte-identical");
    assert_eq!(state(20).as_deref(), Some(&b"kit"[..]));
    assert!(state(30).is_some_and(|s| !s.is_empty()));
    let placeholder: &Device = find_device(&reopened, DeviceId(10));
    assert_eq!(placeholder.plugin.name, "Vital");
    rig.manager.shutdown();
}

#[test]
fn temporary_parameter_edit_saves_the_base_and_restores_it_on_reopen() {
    let dir = scratch("temporary-param-state");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let id = param_id(&rig.manager, 10, "Gain");
    let base = rig.manager.param_base(device_id, id).unwrap();
    let slot = rig.slots[&device_id];
    let safe = rig.manager.snapshot_states(PATIENCE);
    assert_eq!(safe.len(), 1);

    let temporary = base * 0.5;
    assert_ne!(temporary, base);
    rig.send(komponist_engine::Command::SetParam {
        slot,
        id,
        value: temporary,
    });
    rig.process(BLOCK);
    rig.manager.request_state(device_id);
    rig.manager.preview_param(device_id, id, temporary);
    let states = rig.manager.snapshot_states(PATIENCE);
    let [(saved_device, state)] = states.as_slice() else {
        panic!("expected the playing plugin's snapshot");
    };
    assert_eq!(*saved_device, device_id);
    assert_eq!(rig.manager.param_base(device_id, id), Some(base));
    let raw = state.bytes.as_ref();
    assert_ne!(
        raw,
        safe[0].1.bytes.as_ref(),
        "the plug-in captured the live value"
    );
    assert_eq!(state.parameter_bases.get(&id), Some(&base));

    assert!(rig.manager.restore_saved_state(device_id, state));
    run_until(&mut rig, "active state restored", |rig| {
        rig.events.iter().any(|event| {
            matches!(event, PluginManagerEvent::StateApplied { device } if *device == device_id)
        })
    });
    rig.process(BLOCK);
    let active = rig.manager.snapshot_states(PATIENCE);
    let raw = active[0].1.bytes.as_ref();
    assert_eq!(raw, safe[0].1.bytes.as_ref());

    let mut reopened = device(
        10,
        DeviceRole::Instrument,
        clap_plugin(SYNTH),
        Some(&state.bytes),
    );
    reopened.parameter_bases.clone_from(&state.parameter_bases);
    rig.manager.shutdown();
    let mut rig = Rig::new(
        scanned_manager(&dir).unwrap(),
        project(vec![track(1, vec![reopened])]),
    );
    rig.wait_status(10, DeviceStatus::Ok);
    assert_eq!(rig.manager.param_base(device_id, id), Some(base));
    assert_eq!(rig.manager.snapshot_states(PATIENCE), safe);
    rig.manager.shutdown();
}

#[test]
fn snapshots_keep_an_edit_pending_until_the_engine_delivers_it() {
    let dir = scratch("snapshot-before-engine-edit");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let id = param_id(&rig.manager, 10, "Gain");
    let original = rig.manager.snapshot_states(PATIENCE);
    assert_eq!(original.len(), 1);

    let base = rig.manager.param_base(device_id, id).unwrap();
    let slot = rig.slots[&device_id];
    let send = |rig: &mut Rig, value| {
        rig.send(komponist_engine::Command::SetParam { slot, id, value });
        rig.process(BLOCK);
    };
    send(&mut rig, base * 0.5);
    rig.manager.preview_param(device_id, id, base * 0.5);
    assert_eq!(rig.manager.snapshot_states(PATIENCE).len(), 1);
    send(&mut rig, base);
    // The synth rounds to its internal 0.1 dB step. The requested value is between steps.
    let value = 0.403;
    assert_ne!(rig.manager.param_base(device_id, id), Some(value));
    assert!(rig.manager.edited_param(device_id, id, value));
    rig.manager.request_state(device_id);

    // The manager has received the edit. No audio block has delivered it to the plugin yet.
    assert!(rig.manager.snapshot_states(PATIENCE).is_empty());
    assert!(rig.manager.poll().into_iter().all(|event| !matches!(
        event,
        PluginManagerEvent::StateSnapshot { device, .. } if device == device_id
    )));
    assert!(rig.manager.has_pending_state_edits());
    let outcome = rig.manager.snapshot_for_save(Duration::from_millis(100));
    assert_eq!(outcome.blocked, [device_id]);
    assert!(outcome.captured.is_empty());
    assert!(outcome.unavailable.is_empty());

    send(&mut rig, value);
    rig.manager.request_state(device_id);
    let updated = next_snapshot(&mut rig, 10);
    assert_ne!(updated, original[0].1);
    let applied = rig.manager.param_base(device_id, id).unwrap();
    assert!((applied - 0.403333333).abs() < 1e-6, "{applied}");
    assert!(!rig.manager.has_pending_state_edits());
    assert_eq!(
        rig.manager.snapshot_for_save(PATIENCE),
        SaveSnapshots {
            captured: vec![(device_id, updated)],
            ..Default::default()
        }
    );
    rig.manager.shutdown();
}

#[test]
fn project_save_reports_a_slow_bridge_and_keeps_the_other_snapshot() {
    let dir = scratch("save-with-slow-bridge");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let slow = DeviceId(10);
    let healthy = DeviceId(20);
    let mut rig = Rig::new(
        manager,
        project(vec![
            track(
                1,
                vec![device(
                    slow.0,
                    DeviceRole::Instrument,
                    clap_plugin(SLOW),
                    None,
                )],
            ),
            track(
                2,
                vec![device(
                    healthy.0,
                    DeviceRole::Instrument,
                    clap_plugin(SYNTH),
                    None,
                )],
            ),
        ]),
    );
    rig.wait_status(slow.0, DeviceStatus::Ok);
    rig.wait_status(healthy.0, DeviceStatus::Ok);
    let baseline = rig.manager.snapshot_for_save(PATIENCE);
    assert_eq!(baseline.captured.len(), 2);
    assert!(baseline.blocked.is_empty());
    assert!(baseline.unavailable.is_empty());

    // kp-test-slow delays one state call after seeing note 125.
    rig.note(1, 125, 127);
    rig.run(4);
    let outcome = rig.manager.snapshot_for_save(Duration::from_millis(200));
    assert_eq!(outcome.unavailable, [slow]);
    assert!(outcome.blocked.is_empty());
    assert_eq!(outcome.captured, [baseline.captured[1].clone()]);

    let retry = rig.manager.snapshot_for_save(PATIENCE);
    assert_eq!(retry, baseline);
}

#[test]
fn project_save_requires_persistent_edits_even_during_a_temporary_override() {
    let dir = scratch("save-with-temporary-param");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let decay = param_id(&rig.manager, 10, "Decay");
    let base = rig.manager.param_base(device_id, gain).unwrap();
    let decay_before = rig.manager.param_base(device_id, decay).unwrap();
    let slot = rig.slots[&device_id];
    let set = |rig: &mut Rig, id, value| {
        rig.send(komponist_engine::Command::SetParam { slot, id, value });
        // State verification must see the edit even when the bridge misses a realtime deadline.
        rig.render(BLOCK);
    };
    let temporary = base * 0.5;
    let decay_after = 0.5;
    assert_ne!(decay_before, decay_after);

    set(&mut rig, gain, temporary);
    rig.manager.preview_param(device_id, gain, temporary);
    let timeout = Duration::from_millis(120);
    let preview_only = rig.manager.snapshot_for_save(timeout);
    assert!(preview_only.blocked.is_empty());
    assert!(preview_only.unavailable.is_empty());
    let [(saved_device, saved)] = preview_only.captured.as_slice() else {
        panic!("expected a snapshot with the temporary output corrected");
    };
    assert_eq!(*saved_device, device_id);
    assert_eq!(saved.parameter_bases[&gain], base);

    // The host cannot save an independent edit until the active processor receives it.
    rig.manager.edited_param(device_id, decay, decay_after);
    let outcome = rig.manager.snapshot_for_save(timeout);
    assert_eq!(outcome.blocked, [device_id]);
    assert!(outcome.captured.is_empty());
    assert!(outcome.unavailable.is_empty());

    set(&mut rig, decay, decay_after);
    // The bridge may still be returning a deferred snapshot at the first audio callback.
    // Drive further callbacks until a checked capture acknowledges the delivered edit.
    rig.manager.request_state(device_id);
    run_until(&mut rig, "the verified edit to reach the plugin", |rig| {
        !rig.manager.has_pending_state_edits()
    });
    let outcome = rig.manager.snapshot_for_save(PATIENCE);
    assert!(outcome.blocked.is_empty());
    assert!(outcome.unavailable.is_empty());
    let states = outcome.captured;
    let [(saved_device, saved)] = states.as_slice() else {
        panic!("expected a single saved state");
    };
    assert_eq!(*saved_device, device_id);
    assert_eq!(saved.parameter_bases[&gain], base);

    // Change both values again, then load the saved blob to see what really went into the file.
    rig.manager.edited_param(device_id, decay, decay_before);
    set(&mut rig, decay, decay_before);
    rig.manager.edited_param(device_id, gain, temporary);
    set(&mut rig, gain, temporary);
    assert!(rig.manager.restore_saved_state(device_id, saved));
    run_until(&mut rig, "the saved bases", |rig| {
        rig.manager.param_base(device_id, gain) == Some(base)
            && rig
                .manager
                .param_base(device_id, decay)
                .is_some_and(|v| (v - decay_after).abs() < 1e-6)
    });
    rig.manager.shutdown();
}

#[test]
fn parameter_changes_are_snapshotted_once_debounced() {
    let dir = scratch("debounce");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, 10, "Gain");
    let from = rig.events.len();
    let t0 = Instant::now();
    // A knob drag: many values in a row, no explicit request.
    for step in 0..20 {
        rig.manager
            .set_param(DeviceId(10), gain, 0.3 + f64::from(step) * 0.01);
        rig.process(BLOCK);
    }
    let snapshots = |rig: &Rig| {
        rig.events[from..]
            .iter()
            .filter(|e| matches!(e, PluginManagerEvent::StateSnapshot { .. }))
            .count()
    };
    run_until(&mut rig, "a debounced snapshot", |rig| snapshots(rig) > 0);
    let after = t0.elapsed();
    assert!(
        after >= Duration::from_millis(900),
        "not debounced: {after:?}"
    );
    // No second one without further changes.
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_millis(1500) {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(snapshots(&rig), 1);
    rig.manager.shutdown();
}

#[test]
fn unmapping_a_persistent_macro_value_allows_saving_after_a_failed_restore() {
    let dir = scratch("bad-state-unmap");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let device = DeviceId(10);
    let blob = b"not a state of kp-test-synth";
    let synth = self::device(10, DeviceRole::Instrument, clap_plugin(SYNTH), Some(blob));
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(device.0, DeviceStatus::Ok);
    let gain = param_id(&rig.manager, device.0, "Gain");
    let base = rig.manager.param_base(device, gain).unwrap();
    assert!(rig.manager.state_warning(device).is_some());

    // Removing a no-op mapping leaves the rejected bytes alone.
    rig.manager.set_macro_params(&[(device, gain, base)]);
    rig.manager.set_macro_params(&[]);
    assert!(rig.manager.state_warning(device).is_some());
    assert_eq!(
        rig.manager.snapshot_for_save(PATIENCE),
        SaveSnapshots::default()
    );

    let value = 0.5;
    assert_ne!(base, value);
    rig.manager.set_macro_params(&[(device, gain, value)]);
    rig.send(Command::SetParam {
        slot: rig.slots[&device],
        id: gain,
        value,
    });
    rig.run(4);
    assert!(rig.manager.state_warning(device).is_some());

    rig.manager.set_macro_params(&[]);
    assert_eq!(rig.manager.state_warning(device), None);
    let outcome = rig.manager.snapshot_for_save(PATIENCE);
    assert!(outcome.blocked.is_empty());
    assert!(outcome.unavailable.is_empty());
    let states = outcome.captured;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0, device);
    assert_ne!(&*states[0].1.bytes, blob);
    assert_eq!(rig.manager.param_base(device, gain), Some(value));
}

#[test]
fn a_state_that_fails_to_load_is_kept_until_the_user_changes_something() {
    let dir = scratch("bad-state");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let blob: &[u8] = b"not a state of kp-test-synth";
    let original = project(vec![
        track(
            1,
            vec![device(
                10,
                DeviceRole::Instrument,
                clap_plugin(SYNTH),
                Some(blob),
            )],
        ),
        track(
            2,
            vec![device(20, DeviceRole::Instrument, clap_plugin(SYNTH), None)],
        ),
    ]);
    let mut rig = Rig::new(manager, original.clone());
    // Usable, with the plugin's defaults (−12 dB), and a warning.
    rig.wait_status(10, DeviceStatus::Ok);
    rig.wait_status(20, DeviceStatus::Ok);
    let warning = rig.manager.state_warning(DeviceId(10)).map(str::to_owned);
    assert!(
        warning.as_deref().is_some_and(|w| !w.is_empty()),
        "{warning:?}"
    );
    assert_eq!(rig.manager.state_warning(DeviceId(20)), None);
    assert!(rig.events.iter().any(|e| matches!(
        e,
        PluginManagerEvent::Log { device: Some(DeviceId(10)), level: LogLevel::Warning, message }
            if message.starts_with(STATE_LOG_PREFIX)
    )));
    assert!(!rig.events.iter().any(|e| matches!(
        e,
        PluginManagerEvent::StatusChanged {
            status: DeviceStatus::Crashed(_),
            ..
        }
    )));
    let level = play(&mut rig, 1, 69);
    assert!((0.2..0.3).contains(&level), "defaults: {level}");

    // No snapshot replaces the stored blob: not on request, not on save.
    let from = rig.events.len();
    rig.manager.request_state(DeviceId(10));
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(1500) {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!rig.events[from..].iter().any(|e| matches!(
        e,
        PluginManagerEvent::StateSnapshot {
            device: DeviceId(10),
            ..
        }
    )));
    let states = rig.manager.snapshot_states(PATIENCE);
    let devices: Vec<u64> = states.iter().map(|(d, _)| d.0).collect();
    assert_eq!(devices, [20]);
    // Ordinary save can use the last known state. Replacing a running device requires a fresh,
    // restorable snapshot and rejects it if the bridge kept an unusable blob.
    assert_eq!(
        rig.manager
            .snapshot_selected_states(&[DeviceId(10), DeviceId(20)], PATIENCE),
        Err(DeviceId(10))
    );
    // A device with no running bridge does not require a live snapshot.
    let only_healthy = rig
        .manager
        .snapshot_selected_states(&[DeviceId(20), DeviceId(999)], PATIENCE)
        .unwrap();
    assert_eq!(
        only_healthy.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [DeviceId(20)]
    );
    let mut document = Document::new(original);
    for (device, state) in states {
        document
            .apply_untracked(|p| komponist_model::edits::set_saved_plugin_state(p, device, &state))
            .unwrap();
    }
    let saved = komponist_model::persist::to_json(document.project()).unwrap();
    let reopened = komponist_model::persist::from_json(&saved).unwrap();
    assert_eq!(
        find_device(&reopened, DeviceId(10)).state.as_deref(),
        Some(blob),
        "the stored blob survives the save"
    );

    // A reload tries the stored blob again.
    rig.reload(10);
    rig.wait_status(10, DeviceStatus::Ok);
    assert!(rig.manager.state_warning(DeviceId(10)).is_some());

    // The user changes something: the warning clears and snapshots resume (debounced).
    let gain = param_id(&rig.manager, 10, "Gain");
    rig.manager.set_param(DeviceId(10), gain, 0.5);
    assert_eq!(rig.manager.state_warning(DeviceId(10)), None);
    let state = next_snapshot(&mut rig, 10);
    assert_ne!(&state.bytes[..], blob);
    let devices: Vec<u64> = rig
        .manager
        .snapshot_states(PATIENCE)
        .iter()
        .map(|(d, _)| d.0)
        .collect();
    assert_eq!(devices, [10, 20]);
    rig.manager.shutdown();
}

/// Whether process `pid` exists (a zombie counts: it is not reaped yet). macOS has no `/proc`: `kill -0`.
/// Windows: the process can be opened and has no exit code yet.
fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        komponist_ipc::ProcessWatcher::new(pid)
            .is_ok_and(|mut w| w.try_exit().is_ok_and(|exit| exit.is_none()))
    } else if cfg!(target_os = "linux") {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// What the owner's crash check sends (`tests/macos-frames-crash.sh`, internal notes): SIGABRT
/// from outside ends a bridge and reads "Abgestürzt". SIGSEGV from `kill` does not: Rust's std
/// handler for stack overflows sees no guard-page fault, restores the default action and returns,
/// and as nothing faults again the bridge runs on (owner test of #52, 2026-11-16).
#[cfg(unix)]
#[test]
fn a_bridge_killed_by_sigabrt_is_crashed_and_kill_sigsegv_is_survived() {
    let dir = scratch("sigabrt");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let pid = rig.manager.bridge_pid(DeviceId(10)).unwrap();
    let raw = rustix::process::Pid::from_raw(pid as i32).unwrap();
    rustix::process::kill_process(raw, rustix::process::Signal::SEGV).unwrap();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(500) {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Ok);
    assert!(alive(pid), "kill -SEGV left the bridge running");
    rustix::process::kill_process(raw, rustix::process::Signal::ABORT).unwrap();
    rig.wait_status(
        10,
        DeviceStatus::Crashed(FailReason::Exited {
            code: None,
            signal: Some(6),
        }),
    );
}

/// Windows: a bridge another process ends with an exception code (what a crash or `abort` looks
/// like from outside) reads "Abgestürzt" with that code, and reloads.
#[cfg(windows)]
#[test]
fn a_bridge_terminated_with_an_exception_code_is_crashed() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    /// `STATUS_STACK_BUFFER_OVERRUN`, the code of a fast fail (`abort` in a plugin).
    const FAST_FAIL: u32 = 0xC000_0409;
    let dir = scratch("terminated");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let pid = rig.manager.bridge_pid(DeviceId(10)).unwrap();
    // SAFETY: a fresh handle with the right to terminate, closed again.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!process.is_null());
        assert_ne!(TerminateProcess(process, FAST_FAIL), 0);
        CloseHandle(process);
    }
    rig.wait_status(
        10,
        DeviceStatus::Crashed(FailReason::Exited {
            code: None,
            signal: Some(FAST_FAIL as i32),
        }),
    );
    run_until(&mut rig, "ProcessorFailed", |rig| rig.failed(10));
    rig.reload(10);
    rig.wait_status(10, DeviceStatus::Ok);
}

/// Stops every thread of process `pid` (`SIGSTOP`; Windows: attaching as a debugger, which
/// suspends the process until the debugger continues it, and this test never does).
fn stop_process(pid: u32) {
    #[cfg(unix)]
    {
        let raw = rustix::process::Pid::from_raw(pid as i32).unwrap();
        rustix::process::kill_process(raw, rustix::process::Signal::STOP).unwrap();
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            DebugActiveProcess, DebugSetProcessKillOnExit,
        };
        // SAFETY: plain FFI calls; the stopped bridge is killed by the watchdog.
        unsafe {
            assert_ne!(
                DebugActiveProcess(pid),
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            DebugSetProcessKillOnExit(1);
        }
    }
}

/// Lets a process stopped by [`stop_process`] go on, so a kill can complete (Windows: a debugged
/// process exits only once its debugger has seen the exit; nothing to do after `SIGSTOP`).
fn release_process(pid: u32) {
    #[cfg(unix)]
    let _ = pid;
    // SAFETY: detaches the debugger this thread attached in `stop_process`.
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::DebugActiveProcessStop(pid);
    }
}

#[test]
fn watchdog_kills_a_hanging_bridge() {
    let dir = scratch("watchdog");
    let Some(mut manager) = scanned_manager(&dir) else {
        return;
    };
    manager.set_watchdog_timing(Duration::from_millis(50), Duration::from_millis(500));
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    // A healthy bridge answers every ping.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(1) {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Ok);

    // A stalled bridge (stopped) misses its Pong: "hängt", killed, the slot fails over.
    let pid = rig.manager.bridge_pid(DeviceId(10)).unwrap();
    stop_process(pid);
    let t0 = Instant::now();
    rig.wait_status(10, DeviceStatus::Crashed(FailReason::Unresponsive));
    let detected = t0.elapsed();
    eprintln!("watchdog: stalled bridge detected after {detected:.1?}");
    assert!(detected < Duration::from_secs(5));
    run_until(&mut rig, "ProcessorFailed", |rig| rig.failed(10));
    release_process(pid);
    run_until(&mut rig, "the killed bridge reaped", |_| !alive(pid));
    rig.reload(10);
    rig.wait_status(10, DeviceStatus::Ok);
}

/// The test name the plugin manager runs as the fake bridge of [`fake_bridge_manager`].
#[cfg(unix)]
const FAKE_BRIDGE: &str = "lifecycle::fake_bridge_entry";

/// How the fake bridge responds to control traffic.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FakeBridge {
    /// Writes the header and one payload byte of a `Pong`, then stops itself (`SIGSTOP`).
    StallsMidFrame,
    /// Sends a [`SLOW_LOG_LEN`]-byte `Log` message over about [`SLOW_SEND`], then the `Pong`.
    SendsSlowly,
    /// Defers a startup restore until the parent sends a later parameter edit.
    FailsStartupRestore,
    /// Removes a parameter from the inventory when the parent edits it.
    RemovesParameter,
    /// Defers the first snapshot of a quiet device, then supplies a state on retry.
    DefersQuietSnapshot,
}

#[cfg(unix)]
impl FakeBridge {
    fn arg(self) -> &'static str {
        match self {
            FakeBridge::StallsMidFrame => "stalls-mid-frame",
            FakeBridge::SendsSlowly => "sends-slowly",
            FakeBridge::FailsStartupRestore => "fails-startup-restore",
            FakeBridge::RemovesParameter => "removes-parameter",
            FakeBridge::DefersQuietSnapshot => "defers-quiet-snapshot",
        }
    }
}

/// Length of the slow `Log` message of [`FakeBridge::SendsSlowly`]: many frames.
#[cfg(unix)]
const SLOW_LOG_LEN: usize = 1024 * 1024;

/// How long [`FakeBridge::SendsSlowly`] takes for its message: several `pong_timeout`s of
/// [`fake_bridge_manager`].
#[cfg(unix)]
const SLOW_SEND: Duration = Duration::from_millis(2000);

/// A fake bridge (#216): this test binary, started by the plugin manager as `<test>
/// lifecycle::fake_bridge_entry --exact … -- <marker> <mode> --instance --control-fd N` (libtest
/// takes everything after `--` as name filters, so only this test runs). It plays the start-up
/// protocol and then follows the selected [`FakeBridge`] mode. In an ordinary test run it does
/// nothing.
#[cfg(unix)]
#[test]
fn fake_bridge_entry() {
    use komponist_ipc::control::{ParamValue, StateRestoreError, param_flags};
    use komponist_ipc::{
        BridgeChannel, BridgeMsg, ControlMsg, LoadedInfo, ParamInfo, PluginDescriptor,
    };
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};

    let args: Vec<String> = std::env::args().collect();
    let Some(at) = args.iter().position(|arg| arg == "--control-fd") else {
        return;
    };
    let fd: i32 = args[at + 1].parse().expect("fd number");
    let marker = std::path::PathBuf::from(&args[at - 3]);
    let mode = [
        FakeBridge::StallsMidFrame,
        FakeBridge::SendsSlowly,
        FakeBridge::FailsStartupRestore,
        FakeBridge::RemovesParameter,
        FakeBridge::DefersQuietSnapshot,
    ]
    .into_iter()
    .find(|mode| mode.arg() == args[at - 2])
    .expect("fake bridge mode");
    // SAFETY: the plugin manager made this inherited fd ours, and nothing else owns it.
    let channel = BridgeChannel::from_fd(unsafe { OwnedFd::from_raw_fd(fd) });
    let send = |msg: BridgeMsg| {
        if channel.send(&msg).is_err() {
            std::process::exit(0);
        }
    };
    let write_all = |mut bytes: &[u8]| {
        while !bytes.is_empty() {
            match rustix::io::write(channel.as_fd(), bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => std::process::exit(0),
            }
        }
    };
    send(BridgeMsg::Hello {
        protocol: komponist_ipc::control::PROTOCOL_VERSION,
        pid: std::process::id(),
    });
    let Ok(_region) = channel.recv_fd() else {
        std::process::exit(0);
    };
    let param = |id| ParamInfo {
        id,
        name: format!("Parameter {id}"),
        module: String::new(),
        min: 0.0,
        max: 1.0,
        default: 0.5,
        step_count: 0,
        flags: param_flags::AUTOMATABLE,
    };
    let inventory = match mode {
        FakeBridge::FailsStartupRestore => vec![param(7)],
        FakeBridge::RemovesParameter => vec![param(7), param(8)],
        _ => Vec::new(),
    };
    let mut pings = 0;
    let mut activated = false;
    let mut pending_restore = None;
    let mut ready_for_edit = false;
    let mut deferred_snapshot = false;
    loop {
        match channel.recv() {
            Ok(ControlMsg::Load { bundle, .. }) => send(BridgeMsg::Loaded(Box::new(LoadedInfo {
                descriptor: PluginDescriptor {
                    localized: None,
                    format: komponist_ipc::PluginFormat::Clap,
                    id: SYNTH.into(),
                    name: "kp-test-synth".into(),
                    vendor: "klang-studio".into(),
                    version: "0.1.0".into(),
                    kind: komponist_ipc::PluginKind::Instrument,
                    features: Vec::new(),
                    bundle,
                    sidechain: false,
                },
                params: inventory.clone(),
                values: inventory
                    .iter()
                    .map(|param| ParamValue {
                        id: param.id,
                        value: param.default,
                        text: String::new(),
                    })
                    .collect(),
                latency: 0,
                note_names: Vec::new(),
                has_gui: false,
                native_layout: None,
            }))),
            Ok(ControlMsg::Activate { .. }) => {
                send(BridgeMsg::Activated { latency: 0 });
                activated = true;
            }
            Ok(ControlMsg::SetState { request, .. }) => pending_restore = Some(request),
            Ok(ControlMsg::GetState { request }) if mode == FakeBridge::DefersQuietSnapshot => {
                if deferred_snapshot {
                    send(BridgeMsg::State {
                        request,
                        state: b"quiet plugin state".to_vec(),
                        verified: Vec::new(),
                    });
                } else {
                    send(BridgeMsg::StateDeferred { request });
                    deferred_snapshot = true;
                }
            }
            Ok(ControlMsg::SetParam { id: 8, .. }) if mode == FakeBridge::RemovesParameter => {
                send(BridgeMsg::Params {
                    params: vec![param(7)],
                    native_layout: None,
                });
            }
            Ok(ControlMsg::SetParam { .. }) if ready_for_edit => {
                if let Some(request) = pending_restore.take() {
                    send(BridgeMsg::StateApplied {
                        request,
                        result: Err(StateRestoreError {
                            message: "rejected the startup state".into(),
                            changed_values: Vec::new(),
                        }),
                    });
                }
            }
            Ok(ControlMsg::GetStateChecked {
                request, expected, ..
            }) if ready_for_edit || mode == FakeBridge::RemovesParameter => {
                send(BridgeMsg::State {
                    request,
                    state: b"later edit".to_vec(),
                    verified: expected
                        .into_iter()
                        .map(|(id, value, _)| (id, value))
                        .collect(),
                });
            }
            Ok(ControlMsg::Ping { seq }) => {
                pings += 1;
                if pings == 3 && mode == FakeBridge::StallsMidFrame {
                    // A frame header announcing a 5-byte message (tag `TAG_LAST`, u24 length,
                    // u32 total), and one byte of it.
                    write_all(&[0, 5, 0, 0, 5, 0, 0, 0, 0]);
                    std::fs::write(&marker, b"partial frame written").expect("marker file");
                    rustix::process::kill_process(
                        rustix::process::getpid(),
                        rustix::process::Signal::STOP,
                    )
                    .expect("SIGSTOP");
                    // Only a kill ends a stopped process; should it ever go on, it stays silent.
                    loop {
                        std::thread::park();
                    }
                }
                if pings == 3 && mode == FakeBridge::SendsSlowly {
                    // The frames `send` writes, captured, then written in slices with pauses:
                    // the `Pong` queues behind a message that takes several pong timeouts.
                    let wire = wire_of(&BridgeMsg::Log {
                        level: LogLevel::Info,
                        message: "x".repeat(SLOW_LOG_LEN),
                    });
                    let slices = 100;
                    for slice in wire.chunks(wire.len().div_ceil(slices)) {
                        write_all(slice);
                        std::thread::sleep(SLOW_SEND / slices as u32);
                    }
                    send(BridgeMsg::Pong { seq });
                    std::fs::write(&marker, b"slow message sent").expect("marker file");
                    continue;
                }
                send(BridgeMsg::Pong { seq });
            }
            Ok(_) => {}
            Err(_) => std::process::exit(0),
        }
        if mode == FakeBridge::FailsStartupRestore
            && activated
            && pending_restore.is_some()
            && !ready_for_edit
        {
            ready_for_edit = true;
            send(BridgeMsg::ParamValues(vec![ParamValue {
                id: 7,
                value: 0.5,
                text: "restore pending".into(),
            }]));
        }
    }
}

#[cfg(unix)]
#[test]
fn a_quiet_bridge_stays_pending_after_deferral_until_its_retry_succeeds() {
    let dir = scratch("deferred-retirement-capture");
    let Some((manager, _)) = fake_bridge_manager(&dir, FakeBridge::DefersQuietSnapshot) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(device_id.0, DeviceStatus::Ok);

    let outcome = rig
        .manager
        .snapshot_available_states(&[device_id], PATIENCE);
    assert_eq!(
        outcome,
        SaveSnapshots {
            unavailable: vec![device_id],
            ..Default::default()
        }
    );
    assert!(rig.manager.retiring_snapshot_pending(device_id));
    rig.manager.schedule_retiring_snapshot(device_id);
    let state = next_snapshot(&mut rig, device_id.0);
    assert_eq!(&*state.bytes, b"quiet plugin state");
    assert!(!rig.manager.retiring_snapshot_pending(device_id));
}

#[cfg(unix)]
#[test]
fn later_edit_remains_saveable_after_failed_startup_restore() {
    let dir = scratch("failed-startup-restore");
    let Some((manager, _)) = fake_bridge_manager(&dir, FakeBridge::FailsStartupRestore) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(
        10,
        DeviceRole::Instrument,
        clap_plugin(SYNTH),
        Some(b"startup state"),
    );
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    run_until(&mut rig, "activated with a restore pending", |rig| {
        rig.manager
            .param_value(device_id, 7)
            .is_some_and(|param| param.text == "restore pending")
    });
    assert_eq!(rig.manager.status(device_id), DeviceStatus::Loading);
    rig.manager.set_param(device_id, 7, 0.625);
    rig.wait_status(10, DeviceStatus::Ok);
    assert_eq!(rig.manager.state_warning(device_id), None);
    assert_eq!(rig.manager.param_base(device_id, 7), Some(0.625));
    let snapshot = rig.manager.snapshot_for_save(PATIENCE);
    assert!(snapshot.blocked.is_empty());
    assert_eq!(
        snapshot.captured,
        [(device_id, Arc::<[u8]>::from(&b"later edit"[..]).into())]
    );
}

#[cfg(unix)]
#[test]
fn removing_an_automated_parameter_preserves_snapshots_for_remaining_controls() {
    let dir = scratch("remove-automated-parameter");
    let Some((manager, _)) = fake_bridge_manager(&dir, FakeBridge::RemovesParameter) else {
        return;
    };
    let device_id = DeviceId(10);
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);

    let lanes = [(device_id, 7, None), (device_id, 8, None)];
    let snapshot_bases = |rig: &mut Rig| {
        let save = rig.manager.snapshot_for_save(PATIENCE);
        assert!(save.blocked.is_empty());
        let [(id, state)] = save.captured.try_into().expect("one fresh snapshot");
        assert_eq!(id, device_id);
        state.parameter_bases
    };
    rig.manager.set_automated_params(&lanes);
    assert_eq!(snapshot_bases(&mut rig), [(7, 0.5), (8, 0.5)].into());

    rig.manager.set_param(device_id, 8, 0.625);
    run_until(&mut rig, "parameter inventory pruned", |rig| {
        rig.manager.params(device_id).unwrap().len() == 1
    });
    assert_eq!(snapshot_bases(&mut rig), [(7, 0.5)].into());

    // A program compiled from the old inventory must not put the removed ID back.
    rig.manager.set_automated_params(&lanes);
    assert_eq!(snapshot_bases(&mut rig), [(7, 0.5)].into());

    // Removing the surviving lane still protects its base while the audio catches up.
    rig.manager.set_automated_params(&[]);
    assert_eq!(snapshot_bases(&mut rig), [(7, 0.5)].into());
}

/// The bytes `Channel::send` writes for `msg`, captured through a socket pair of its own (so the
/// fake bridge can write them at its own pace).
#[cfg(unix)]
fn wire_of(msg: &komponist_ipc::BridgeMsg) -> Vec<u8> {
    use std::os::fd::AsFd;
    let (reader, writer) = komponist_ipc::control::socket_pair().expect("socket pair");
    let writer = komponist_ipc::BridgeChannel::from_fd(writer);
    std::thread::scope(|scope| {
        scope.spawn(move || writer.send(msg).expect("capture send"));
        let mut wire = Vec::new();
        let mut buf = vec![0; 64 * 1024];
        loop {
            match rustix::io::read(reader.as_fd(), &mut buf) {
                Ok(0) => return wire,
                Ok(n) => wire.extend_from_slice(&buf[..n]),
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => panic!("capture read: {e}"),
            }
        }
    })
}

/// A manager whose catalog holds the scanned fixtures and whose bridges are the fake bridge in
/// `mode`, with a short watchdog; and the fake bridge's marker file.
#[cfg(unix)]
fn fake_bridge_manager(
    dir: &std::path::Path,
    mode: FakeBridge,
) -> Option<(PluginManager, std::path::PathBuf)> {
    // The real bridge scans the fixtures into the cache file; the manager under test reads the
    // catalog from it and starts the fake bridge.
    drop(scanned_manager(dir)?);
    let marker = dir.join("fake-bridge-marker");
    let mut config = config(dir);
    config.bridge_exe = std::env::current_exe().unwrap();
    config.bridge_args = [
        FAKE_BRIDGE,
        "--exact",
        "--nocapture",
        "--test-threads=1",
        "--",
    ]
    .map(Into::into)
    .to_vec();
    config.bridge_args.push(marker.clone().into());
    config.bridge_args.push(mode.arg().into());
    let mut manager = PluginManager::new(config);
    manager.set_watchdog_timing(Duration::from_millis(50), Duration::from_millis(500));
    manager.set_warm_up(Duration::ZERO);
    Some((manager, marker))
}

/// The state [`PollWatch`]'s watcher and its users share under one lock: the timeout decision
/// and the disarming cannot interleave.
#[cfg(unix)]
#[derive(Default)]
struct WatchState {
    /// When the `poll` in progress started.
    since: Option<Instant>,
    /// The watch is dropped: the watcher ends.
    done: bool,
}

/// Times every `PluginManager::poll` of a test on a watcher thread. A poll that blocks never
/// returns, so the test could not fail by itself: once one call has run for [`PATIENCE`], the
/// watcher ends the process with a message. It is armed only during a call (also a call that
/// panics disarms it), and dropping the watch (also while a failed assertion unwinds) stops the
/// watcher; both take the lock under which the watcher decides, so it never exits after that.
#[cfg(unix)]
struct PollWatch {
    state: Arc<std::sync::Mutex<WatchState>>,
}

#[cfg(unix)]
impl PollWatch {
    fn new(test: &'static str) -> PollWatch {
        let state = Arc::<std::sync::Mutex<WatchState>>::default();
        let watched = state.clone();
        std::thread::spawn(move || {
            loop {
                {
                    let state = lock(&watched);
                    if state.done {
                        return;
                    }
                    if state.since.is_some_and(|since| since.elapsed() > PATIENCE) {
                        eprintln!("{test}: a plugin manager poll blocked for {PATIENCE:?}");
                        // Under the lock: nobody disarms the watch in between.
                        std::process::exit(101);
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        PollWatch { state }
    }

    /// `manager.poll()`, watched.
    fn poll(&self, manager: &mut PluginManager) -> Vec<PluginManagerEvent> {
        /// Disarms the watch when the poll returns or unwinds.
        struct Armed<'a>(&'a std::sync::Mutex<WatchState>);
        impl Drop for Armed<'_> {
            fn drop(&mut self) {
                lock(self.0).since = None;
            }
        }
        lock(&self.state).since = Some(Instant::now());
        let _armed = Armed(&self.state);
        manager.poll()
    }
}

#[cfg(unix)]
impl Drop for PollWatch {
    fn drop(&mut self) {
        lock(&self.state).done = true;
    }
}

/// The watch state, also after a panic elsewhere poisoned it.
#[cfg(unix)]
fn lock(state: &std::sync::Mutex<WatchState>) -> std::sync::MutexGuard<'_, WatchState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// #216: a bridge frozen in the middle of a frame holds up no `poll` of the plugin manager (the UI
/// thread), and the watchdog still finds it unresponsive and kills it.
#[cfg(unix)]
#[test]
fn a_bridge_stalled_mid_frame_blocks_no_poll_and_is_killed() {
    let dir = scratch("stalled-mid-frame");
    let Some((mut manager, marker)) = fake_bridge_manager(&dir, FakeBridge::StallsMidFrame) else {
        return;
    };
    let watch = PollWatch::new("a_bridge_stalled_mid_frame_blocks_no_poll_and_is_killed");

    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let _processor = manager
        .create_processor(&synth, SETUP)
        .expect("start the fake bridge");
    let pid = manager.bridge_pid(DeviceId(10)).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut longest = Duration::ZERO;
    let mut events = Vec::new();
    let mut detected = None;
    let mut ok_at = None;
    while detected.is_none() {
        assert!(Instant::now() < deadline, "not killed; events {events:?}");
        let t0 = Instant::now();
        events.extend(watch.poll(&mut manager));
        longest = longest.max(t0.elapsed());
        match manager.status(DeviceId(10)) {
            DeviceStatus::Ok if ok_at.is_none() => ok_at = Some(Instant::now()),
            // Whether the partial frame was out when the watchdog struck: else the test would
            // pass on a bridge that was merely not scheduled.
            DeviceStatus::Crashed(reason) => detected = Some((reason, marker.exists())),
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let (reason, partial_written) = detected.unwrap();
    let ok_at = ok_at.unwrap_or_else(|| panic!("the fake bridge never ran; events {events:?}"));
    assert!(
        partial_written,
        "the watchdog struck before the fake bridge wrote its partial frame"
    );
    assert_eq!(reason, FailReason::Unresponsive);
    eprintln!(
        "stalled mid-frame: detected after {:.1?}, longest poll {longest:.1?}",
        ok_at.elapsed()
    );
    assert!(
        longest < Duration::from_millis(250),
        "a poll took {longest:?}"
    );
    let t0 = Instant::now();
    while alive(pid) {
        assert!(t0.elapsed() < PATIENCE, "the stopped bridge was not killed");
        watch.poll(&mut manager);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// #216 review: a bridge whose `Pong` queues behind a large message that arrives over many polls,
/// for several pong timeouts, is alive while the message keeps coming; it is not killed, and the
/// message and the `Pong` arrive.
#[cfg(unix)]
#[test]
fn a_bridge_sending_a_large_message_slowly_is_not_killed() {
    let dir = scratch("sends-slowly");
    let Some((mut manager, marker)) = fake_bridge_manager(&dir, FakeBridge::SendsSlowly) else {
        return;
    };
    let watch = PollWatch::new("a_bridge_sending_a_large_message_slowly_is_not_killed");

    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let _processor = manager
        .create_processor(&synth, SETUP)
        .expect("start the fake bridge");
    let deadline = Instant::now() + PATIENCE;
    let mut events = Vec::new();
    let mut ok = false;
    let mut longest = Duration::ZERO;
    let started = Instant::now();
    // Until the fake bridge has sent the message and the `Pong`, and a little longer.
    let mut sent_at = None;
    while sent_at.is_none_or(|at: Instant| at.elapsed() < Duration::from_millis(300)) {
        assert!(
            Instant::now() < deadline,
            "the slow message never finished; events {events:?}"
        );
        let t0 = Instant::now();
        events.extend(watch.poll(&mut manager));
        longest = longest.max(t0.elapsed());
        match manager.status(DeviceId(10)) {
            DeviceStatus::Ok => ok = true,
            DeviceStatus::Crashed(reason) => panic!(
                "a bridge still sending was killed ({reason:?}) after {:.1?}",
                started.elapsed()
            ),
            _ => {}
        }
        if sent_at.is_none() && marker.exists() {
            sent_at = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ok, "the fake bridge never ran; events {events:?}");
    let slow = events.iter().any(|event| {
        matches!(event, PluginManagerEvent::Log { message, .. } if message.len() == SLOW_LOG_LEN)
    });
    assert!(slow, "the slow message did not arrive");
    assert_eq!(manager.status(DeviceId(10)), DeviceStatus::Ok);
    eprintln!(
        "sent slowly: {:.1?} without a kill, longest poll {longest:.1?}",
        started.elapsed()
    );
    assert!(
        longest < Duration::from_millis(250),
        "a poll took {longest:?}"
    );
}

#[test]
fn release_shutdown_and_drop_end_the_bridges() {
    let dir = scratch("shutdown");
    let Some(mut manager) = scanned_manager(&dir) else {
        return;
    };
    let devices: Vec<Device> = (1..=3)
        .map(|i| device(i, DeviceRole::Effect, clap_plugin(FX), None))
        .collect();
    for d in &devices {
        manager.create_processor(d, SETUP).unwrap();
    }
    // A legacy device without a verified bundle and an unknown instance.
    let hexa = Device {
        plugin: PluginRef {
            format: PluginFormat::Builtin,
            id: komponist_model::project::BUILTIN_HEXA.into(),
            name: "Hexa".into(),
            vendor: "klang-studio".into(),
            version: "0.1.0".into(),
            module: Box::default(),
        },
        ..device(9, DeviceRole::Instrument, clap_plugin(SYNTH), None)
    };
    assert!(matches!(
        manager.create_processor(&hexa, SETUP),
        Err(PluginError::NotInstalled(_))
    ));
    assert_eq!(manager.status(DeviceId(9)), DeviceStatus::Missing);
    assert_eq!(manager.status(DeviceId(99)), DeviceStatus::Ok);
    assert_eq!(
        manager.open_gui(DeviceId(99), GuiTarget::X11Child { xid: 1, scale: 1.0 }),
        Err(PluginError::UnknownDevice(DeviceId(99)))
    );
    let pids: Vec<u32> = (1..=3)
        .map(|i| manager.bridge_pid(DeviceId(i)).unwrap())
        .collect();
    let deadline = Instant::now() + PATIENCE;
    while (1..=3).any(|i| manager.status(DeviceId(i)) != DeviceStatus::Ok) {
        assert!(Instant::now() < deadline);
        manager.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        manager.open_gui(DeviceId(1), GuiTarget::X11Child { xid: 1, scale: 1.0 }),
        Err(PluginError::NoGui)
    );

    // Release: Shutdown, the bridge exits by itself.
    manager.release(DeviceId(1));
    assert_eq!(manager.bridge_pid(DeviceId(1)), None);
    let deadline = Instant::now() + PATIENCE;
    while alive(pids[0]) {
        assert!(Instant::now() < deadline, "released bridge still alive");
        manager.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(alive(pids[1]) && alive(pids[2]));

    // Shutdown: all others.
    manager.shutdown();
    assert!(!alive(pids[1]) && !alive(pids[2]));
    assert_eq!(manager.bridge_pid(DeviceId(2)), None);

    // Drop: SIGKILL, also for a bridge that is still loading.
    let mut manager = scanned_manager(&dir).unwrap();
    manager.create_processor(&devices[0], SETUP).unwrap();
    let pid = manager.bridge_pid(DeviceId(1)).unwrap();
    assert!(alive(pid));
    drop(manager);
    assert!(!alive(pid));
}
