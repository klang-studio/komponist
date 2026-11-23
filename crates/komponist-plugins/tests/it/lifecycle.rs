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
fn next_snapshot(rig: &mut Rig, device: u64) -> Arc<[u8]> {
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

    rig.manager.set_param(device_id, gain, -24.0);
    assert_eq!(rig.manager.param_base(device_id, gain), Some(-24.0));
    assert!(rig.manager.set_state(device_id, &state));
    run_until(&mut rig, "the restored gain base", |rig| {
        rig.manager.param_base(device_id, gain) == Some(initial)
    });

    let from = rig.events.len();
    assert!(rig.manager.set_state(device_id, &state));
    rig.manager.set_param(device_id, gain, -30.0);
    run_until(&mut rig, "the second restore acknowledgement", |rig| {
        rig.events[from..].contains(&PluginManagerEvent::StateApplied { device: device_id })
    });
    assert_eq!(rig.manager.param_base(device_id, gain), Some(-30.0));
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
    let from = rig.events.len();
    for _ in 0..5 {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !rig.events[from..]
            .iter()
            .any(|event| matches!(event, PluginManagerEvent::StateSnapshot { .. }))
    );

    output(&mut rig, mapped);
    rig.manager.request_state(device_id);
    let state = next_snapshot(&mut rig, 10);
    assert!(rig.manager.is_macro_snapshot(device_id, &state));
    assert_eq!(rig.manager.param_base(device_id, gain), base);

    rig.manager
        .set_macro_params(&[(device_id, gain, next_mapped)]);
    output(&mut rig, next_mapped);
    rig.manager.set_param(device_id, decay, independent);
    rig.manager.request_state(device_id);
    let edited = next_snapshot(&mut rig, 10);
    assert!(!rig.manager.is_macro_snapshot(device_id, &edited));
    assert_eq!(rig.manager.param_base(device_id, decay), Some(independent));
    assert_eq!(rig.manager.param_base(device_id, gain), base);

    rig.manager.set_macro_params(&[]);
    assert_eq!(rig.manager.param_base(device_id, gain), Some(next_mapped));
    rig.manager.request_state(device_id);
    let state = next_snapshot(&mut rig, 10);
    assert!(rig.manager.is_macro_snapshot(device_id, &state));

    // Restore the earlier mixed snapshot after changing both parameters. StateApplied reports
    // values read from the real bridge plugin, rather than the manager's optimistic cache.
    rig.manager.set_param(device_id, gain, preview);
    rig.manager.set_param(device_id, decay, 0.75);
    let from = rig.events.len();
    assert!(rig.manager.set_state(device_id, &edited));
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
        // klopfer is not configured in this manager: missing too.
        track(
            2,
            vec![device(20, DeviceRole::Instrument, klopfer(), Some(b"kit"))],
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
                plugin: klopfer(),
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
            .apply_untracked(|p| {
                komponist_model::edits::set_device_state(p, device, Some(state.clone()))
            })
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
fn temporary_parameter_edit_defers_plugin_snapshot_until_base_returns() {
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
    assert!(rig.manager.snapshot_states(PATIENCE).is_empty());
    assert_eq!(rig.manager.param_base(device_id, id), Some(base));

    rig.send(komponist_engine::Command::SetParam {
        slot,
        id,
        value: base,
    });
    rig.process(BLOCK);
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
    assert!(rig.manager.snapshot_states(PATIENCE).is_empty());
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
        rig.process(BLOCK);
    };
    let temporary = base * 0.5;
    let decay_after = 0.5;
    assert_ne!(decay_before, decay_after);

    set(&mut rig, gain, temporary);
    rig.manager.preview_param(device_id, gain, temporary);
    let timeout = Duration::from_millis(120);
    assert_eq!(
        rig.manager.snapshot_for_save(timeout),
        SaveSnapshots::default()
    );

    rig.manager.edited_param(device_id, decay, decay_after);
    set(&mut rig, decay, decay_after);
    let outcome = rig.manager.snapshot_for_save(timeout);
    assert_eq!(outcome.blocked, [device_id]);
    assert!(outcome.captured.is_empty());
    assert!(outcome.unavailable.is_empty());

    set(&mut rig, gain, base);
    let outcome = rig.manager.snapshot_for_save(PATIENCE);
    assert!(outcome.blocked.is_empty());
    assert!(outcome.unavailable.is_empty());
    let states = outcome.captured;
    let [(saved_device, saved)] = states.as_slice() else {
        panic!("expected a single saved state");
    };
    assert_eq!(*saved_device, device_id);

    // Change both values again, then load the saved blob to see what really went into the file.
    rig.manager.edited_param(device_id, decay, decay_before);
    set(&mut rig, decay, decay_before);
    rig.manager.edited_param(device_id, gain, temporary);
    set(&mut rig, gain, temporary);
    assert!(rig.manager.set_state(device_id, saved));
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
    assert_ne!(&*states[0].1, blob);
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
            .apply_untracked(|p| {
                komponist_model::edits::set_device_state(p, device, Some(state.clone()))
            })
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
    assert_ne!(&state[..], blob);
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

/// The test name the plugin manager runs as its bridge in
/// [`a_bridge_stalled_mid_frame_blocks_no_poll_and_is_killed`].
#[cfg(unix)]
const STALLING_BRIDGE: &str = "lifecycle::stalling_bridge_entry";

/// A fake bridge that stops in the middle of a frame (#216): this test binary, started by the
/// plugin manager as `<test> lifecycle::stalling_bridge_entry --exact … -- <marker> --instance
/// --control-fd N` (libtest takes everything after `--` as name filters, so only this test runs).
/// It plays the start-up protocol, answers two `Ping`s, then writes the header and one payload
/// byte of a `Pong`, creates the file `<marker>` (the parent's proof that the partial frame is
/// out) and stops itself (`SIGSTOP`). In an ordinary test run it does nothing.
#[cfg(unix)]
#[test]
fn stalling_bridge_entry() {
    use komponist_ipc::{BridgeChannel, BridgeMsg, ControlMsg, LoadedInfo, PluginDescriptor};
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};

    let args: Vec<String> = std::env::args().collect();
    let Some(at) = args.iter().position(|arg| arg == "--control-fd") else {
        return;
    };
    let fd: i32 = args[at + 1].parse().expect("fd number");
    let marker = std::path::PathBuf::from(&args[at - 2]);
    // SAFETY: the plugin manager made this inherited fd ours, and nothing else owns it.
    let channel = BridgeChannel::from_fd(unsafe { OwnedFd::from_raw_fd(fd) });
    let send = |msg: BridgeMsg| {
        if channel.send(&msg).is_err() {
            std::process::exit(0);
        }
    };
    send(BridgeMsg::Hello {
        protocol: komponist_ipc::control::PROTOCOL_VERSION,
        pid: std::process::id(),
    });
    let Ok(_region) = channel.recv_fd() else {
        std::process::exit(0);
    };
    let mut pongs = 0;
    loop {
        match channel.recv() {
            Ok(ControlMsg::Load { bundle, .. }) => send(BridgeMsg::Loaded(Box::new(LoadedInfo {
                descriptor: PluginDescriptor {
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
                params: Vec::new(),
                values: Vec::new(),
                latency: 0,
                note_names: Vec::new(),
                has_gui: false,
                native_layout: None,
            }))),
            Ok(ControlMsg::Activate { .. }) => send(BridgeMsg::Activated { latency: 0 }),
            Ok(ControlMsg::Ping { seq }) if pongs < 2 => {
                pongs += 1;
                send(BridgeMsg::Pong { seq });
            }
            Ok(ControlMsg::Ping { .. }) => {
                // A frame header announcing a 5-byte message (tag `TAG_LAST`, u24 length, u32
                // total), and one byte of it.
                let partial = [0, 5, 0, 0, 5, 0, 0, 0, 0];
                let written = rustix::io::write(channel.as_fd(), &partial).expect("write");
                assert_eq!(written, partial.len());
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
            Ok(_) => {}
            Err(_) => std::process::exit(0),
        }
    }
}

/// Times every `PluginManager::poll` of a test on a watcher thread. A poll that blocks never
/// returns, so the test could not fail by itself: once one call has run for [`PATIENCE`], the
/// watcher ends the process with a message. It is armed only during a call, and dropping the
/// watch (also while a failed assertion unwinds) stops the watcher.
#[cfg(unix)]
struct PollWatch {
    since: Arc<std::sync::Mutex<Option<Instant>>>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl PollWatch {
    fn new(test: &'static str) -> PollWatch {
        let watch = PollWatch {
            since: Arc::default(),
            done: Arc::default(),
        };
        let (since, done) = (watch.since.clone(), watch.done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let blocked = since
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some_and(|since| since.elapsed() > PATIENCE);
                if blocked {
                    eprintln!("{test}: a plugin manager poll blocked for {PATIENCE:?}");
                    std::process::exit(101);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        watch
    }

    /// `manager.poll()`, watched.
    fn poll(&self, manager: &mut PluginManager) -> Vec<PluginManagerEvent> {
        *self.since.lock().unwrap() = Some(Instant::now());
        let events = manager.poll();
        *self.since.lock().unwrap() = None;
        events
    }
}

#[cfg(unix)]
impl Drop for PollWatch {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// #216: a bridge frozen in the middle of a frame holds up no `poll` of the plugin manager (the UI
/// thread), and the watchdog still finds it unresponsive and kills it.
#[cfg(unix)]
#[test]
fn a_bridge_stalled_mid_frame_blocks_no_poll_and_is_killed() {
    let dir = scratch("stalled-mid-frame");
    // The real bridge scans the fixtures into the cache file; the manager under test reads the
    // catalog from it and starts the fake bridge.
    let Some(scanner) = scanned_manager(&dir) else {
        return;
    };
    drop(scanner);
    // The fake bridge creates it once its partial frame is written.
    let marker = dir.join("partial-frame-written");
    let mut config = config(&dir);
    config.bridge_exe = std::env::current_exe().unwrap();
    config.bridge_args = [
        STALLING_BRIDGE,
        "--exact",
        "--nocapture",
        "--test-threads=1",
        "--",
    ]
    .map(Into::into)
    .to_vec();
    config.bridge_args.push(marker.clone().into());
    let mut manager = PluginManager::new(config);
    manager.set_watchdog_timing(Duration::from_millis(50), Duration::from_millis(500));
    manager.set_warm_up(Duration::ZERO);
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
