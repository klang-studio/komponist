//! Instance mode with the VST3 builds of the WP-27 fixtures, through the real binary and the full
//! IPC path (PLAN WP-32 "Done when"): sound, effect and latency, parameter changes from the app,
//! state between bridges, `kp-test-crash` killing only its bridge, `kp-test-slow` producing
//! `Late`. The VST3 layer is the hosting crate's; the bridge dispatches by format.
//!
//! Each test is skipped with a note when its fixture is missing (`scripts/build-test-plugins.sh`).

mod common;

use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::shm::BLOCK_FLAG_RESET;
use komponist_ipc::{
    BlockError, BlockStatus, BridgeMsg, BridgeState, ControlMsg, PluginFormat, PluginKind,
    ProcessExit,
};

fn vst3(name: &str) -> Option<std::path::PathBuf> {
    fixture_in("vst3", name)
}

/// Plays `key` for one block and releases it until silence; returns the peak of the first block.
fn play(bridge: &mut Bridge, key: u8) -> f32 {
    bridge.block(&[note_on(0, key)]);
    let level = bridge.peak(0..BLOCK);
    bridge.block(&[note_off(0, key)]);
    for _ in 0..4 {
        bridge.block(&[]);
    }
    assert!(bridge.peak(0..BLOCK) < 1e-4, "released");
    level
}

#[test]
fn vst3_synth_sounds_through_the_ipc_path() {
    let Some(bundle) = vst3("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, SYNTH_VST3);
    assert_eq!(info.descriptor.format, PluginFormat::Vst3);
    assert_eq!(info.descriptor.id, SYNTH_VST3);
    assert_eq!(info.descriptor.kind, PluginKind::Instrument);
    assert!(info.has_gui);
    assert_eq!(info.latency, 0);
    let names: Vec<&str> = info.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gain", "Decay"]);
    let gain = Bridge::param_id(&info, "Gain");
    let gain_value = info.values.iter().find(|v| v.id == gain).unwrap();
    assert!((gain_value.value - 0.8).abs() < 1e-6);
    assert!(gain_value.text.contains("dB"), "{gain_value:?}");
    assert_eq!(bridge.region().bridge_state(), BridgeState::Loaded);

    assert_eq!(bridge.activate(), 0);
    let result = bridge.block(&[]);
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(bridge.peak(0..BLOCK), 0.0);

    // A note at frame 100: silent before, a sine at velocity 1 × −12 dB ≈ 0.25 after, L = R.
    let result = bridge.block(&[note_on(100, 69)]);
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(bridge.peak(0..100), 0.0);
    let level = bridge.peak(100..BLOCK);
    assert!((0.2..0.3).contains(&level), "level {level}");
    assert_eq!(bridge.output[0][..BLOCK], bridge.output[1][..BLOCK]);
    bridge.block(&[note_off(0, 69)]);
    for _ in 0..4 {
        bridge.block(&[]);
    }
    assert!(bridge.peak(0..BLOCK) < 1e-4, "{}", bridge.peak(0..BLOCK));

    // The engine's reset flag silences a sounding note at once.
    bridge.block(&[note_on(0, 72)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);
    bridge.next_flags = BLOCK_FLAG_RESET;
    bridge.block(&[]);
    assert_eq!(bridge.peak(0..BLOCK), 0.0);
    assert_eq!(bridge.monitor.pop_output_event(), None);

    // Shutdown unloads the plugin (ModuleExit) and exits cleanly.
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn vst3_effect_audio_and_latency_through_the_ipc_path() {
    let Some(bundle) = vst3("kp-test-fx") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, FX_VST3);
    assert_eq!(info.descriptor.kind, PluginKind::Effect);
    assert!(!info.has_gui);
    assert_eq!(info.latency, 0, "inactive");
    assert_eq!(bridge.activate(), 64);
    assert_eq!(bridge.region().latency(), 64);
    bridge.sync();
    // A latency report from activation (VST3 `restartComponent(kLatencyChanged)`) must not
    // change the value or turn into a restart.
    for m in &bridge.seen {
        match m {
            BridgeMsg::LatencyChanged(latency) => assert_eq!(*latency, 64),
            BridgeMsg::RestartRequested => panic!("restart after activation: {:?}", bridge.seen),
            _ => {}
        }
    }

    bridge.input[0][10] = 1.0;
    bridge.input[1][20] = -0.5;
    bridge.block(&[]);
    let (left, right) = (&bridge.output[0][..BLOCK], &bridge.output[1][..BLOCK]);
    assert_eq!(left[74], 1.0);
    assert_eq!(right[84], -0.5);
    assert_eq!(left.iter().filter(|s| **s != 0.0).count(), 1);
    assert_eq!(right.iter().filter(|s| **s != 0.0).count(), 1);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn vst3_param_changes_from_the_app_reach_the_plugin() {
    let Some(bundle) = vst3("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, SYNTH_VST3);
    let gain = Bridge::param_id(&info, "Gain");

    // Inactive: controller at once, processor with the first block.
    bridge.send(ControlMsg::SetParam {
        id: gain,
        value: 0.5,
    });
    let (value, text) = bridge.expect_value(gain);
    assert_eq!(value, 0.5);
    assert!(text.contains("-30") && text.contains("dB"), "{text}");
    bridge.activate();
    bridge.block(&[note_on(0, 69)]);
    let level = bridge.peak(0..BLOCK);
    assert!(
        (0.025..0.04).contains(&level),
        "−30 dB expected, level {level}"
    );

    // Active: the value reaches the next block as a parameter change at offset 0.
    bridge.send(ControlMsg::SetParam {
        id: gain,
        value: 0.0,
    });
    assert_eq!(bridge.expect_value(gain).0, 0.0);
    bridge.block(&[]);
    let quiet = bridge.peak(0..BLOCK);
    assert!(quiet < level * 0.05, "−60 dB expected, level {quiet}");

    // Automation from the engine travels through the shared event ring.
    bridge.block(&[param(0, gain, 0.5)]);
    let level = bridge.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");

    // Deactivated and active again: the plugin kept the last value.
    bridge.send(ControlMsg::Deactivate);
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    while bridge.region().bridge_state() != BridgeState::Loaded {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    bridge.activate();
    bridge.block(&[note_on(0, 60)]);
    let level = bridge.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn vst3_state_round_trips_between_bridges() {
    let Some(bundle) = vst3("kp-test-synth") else {
        return;
    };
    let mut first = Bridge::spawn();
    let info = first.load(&bundle, SYNTH_VST3);
    let gain = Bridge::param_id(&info, "Gain");
    let decay = Bridge::param_id(&info, "Decay");
    first.send(ControlMsg::SetParam {
        id: gain,
        value: 0.5,
    });
    first.send(ControlMsg::SetParam {
        id: decay,
        value: 0.25,
    });
    first.expect_value(decay);
    first.send(ControlMsg::GetState { request: 7 });
    let state = first.expect("State", |m| match m {
        BridgeMsg::State { request: 7, state } => Some(state.clone()),
        _ => None,
    });
    assert_eq!(&state[..4], b"KPV3", "komponist's VST3 state blob");
    assert_eq!(first.shutdown(), EXIT_OK);

    let mut second = Bridge::spawn();
    second.load(&bundle, SYNTH_VST3);
    second.send(ControlMsg::SetState {
        state: state.clone(),
    });
    let values = second.expect("ParamValues", |m| match m {
        BridgeMsg::ParamValues(v) if v.len() == 2 => Some(v.clone()),
        _ => None,
    });
    let value = |id| values.iter().find(|v| v.id == id).unwrap().value;
    assert!((value(gain) - 0.5).abs() < 1e-6, "{values:?}");
    assert!((value(decay) - 0.25).abs() < 1e-6, "{values:?}");
    second.send(ControlMsg::GetState { request: 8 });
    let again = second.expect("State", |m| match m {
        BridgeMsg::State { request: 8, state } => Some(state.clone()),
        _ => None,
    });
    assert_eq!(
        again, state,
        "the state survives a round trip byte for byte"
    );
    second.activate();
    let level = play(&mut second, 69);
    assert!(
        (0.025..0.04).contains(&level),
        "−30 dB expected, level {level}"
    );
    // A state loaded while active applies from the next block.
    second.send(ControlMsg::SetParam {
        id: gain,
        value: 1.0,
    });
    second.expect_value(gain);
    assert!(play(&mut second, 70) > 0.5);
    second.send(ControlMsg::SetState { state });
    second.expect_value(gain);
    let level = play(&mut second, 71);
    assert!(
        (0.025..0.04).contains(&level),
        "−30 dB expected, level {level}"
    );
    assert_eq!(second.shutdown(), EXIT_OK);
}

#[test]
fn vst3_crash_kills_only_its_bridge() {
    let (Some(crash_bundle), Some(synth_bundle)) = (vst3("kp-test-crash"), vst3("kp-test-synth"))
    else {
        return;
    };
    let mut synth = Bridge::spawn();
    synth.load(&synth_bundle, SYNTH_VST3);
    synth.activate();
    synth.block(&[note_on(0, 69)]);
    assert!(synth.peak(0..BLOCK) > 0.1);

    let mut crash = Bridge::spawn();
    let info = crash.load(&crash_bundle, CRASH_VST3);
    crash.activate();
    for (i, s) in crash.input[0].iter_mut().enumerate() {
        *s = (i as f32 * 0.05).sin() * 0.5;
    }
    crash.block(&[note_on(0, 60)]);
    assert_eq!(
        crash.output[0][..BLOCK],
        crash.input[0][..BLOCK],
        "passthrough"
    );

    // Note 127 makes the plugin write through a null pointer inside `process`.
    let t0 = Instant::now();
    let outcome = crash.block_with(&[note_on(10, 127)], RT_DEADLINE);
    assert_eq!(outcome, Err(BlockError::Late));
    let exit = crash.wait_exit();
    let detected = t0.elapsed();
    assert_eq!(
        exit,
        ProcessExit {
            code: None,
            signal: Some(11),
        }
    );
    assert_eq!(crash.begin(&[]), Err(BlockError::Busy));

    // The other bridge plays on.
    assert_eq!(synth.watcher.try_exit().unwrap(), None);
    for key in 40..60 {
        synth.block(&[note_on(0, key)]);
        assert!(synth.peak(0..BLOCK) > 0.1);
    }
    synth.heartbeat();

    // The parameter path: Crash rising past 0.9 while active, as a VST3 parameter change.
    let mut crash = Bridge::spawn();
    crash.load(&crash_bundle, CRASH_VST3);
    crash.activate();
    crash.block(&[]);
    let crash_param = Bridge::param_id(&info, "Crash");
    crash.send(ControlMsg::SetParam {
        id: crash_param,
        value: 1.0,
    });
    crash.expect_value(crash_param);
    assert_eq!(
        crash.block_with(&[], Duration::from_millis(50)),
        Err(BlockError::Late)
    );
    assert_eq!(crash.wait_exit().signal, Some(11));

    synth.block(&[note_on(0, 72)]);
    assert!(synth.peak(0..BLOCK) > 0.1);
    assert_eq!(synth.shutdown(), EXIT_OK);
    eprintln!("VST3 crash: SIGSEGV in process detected {detected:.1?} after the request");
}

#[test]
fn vst3_slow_plugin_is_late_without_blocking() {
    let Some(bundle) = vst3("kp-test-slow") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, SLOW_VST3);
    bridge.activate();
    for (i, s) in bridge.input[0].iter_mut().enumerate() {
        *s = i as f32 / 1000.0;
    }
    for _ in 0..10 {
        bridge.block(&[]);
    }
    assert_eq!(
        bridge.output[0][..BLOCK],
        bridge.input[0][..BLOCK],
        "passthrough"
    );

    // Note 126: the plugin sleeps 50 ms inside `process`; the app waits for its deadline only.
    let t0 = Instant::now();
    let late = bridge.begin(&[note_on(0, 126)]).unwrap();
    let [left, right] = &mut bridge.output;
    let outcome = bridge
        .client
        .wait_done(late, t0 + RT_DEADLINE, [left, right]);
    let waited = t0.elapsed();
    assert_eq!(outcome, Err(BlockError::Late));
    assert!(
        waited < Duration::from_millis(30),
        "the app waited for the plugin: {waited:?}"
    );
    // The main loop is not blocked by the sleeping RT thread.
    let heartbeat = bridge.heartbeat();
    assert!(
        t0.elapsed() < Duration::from_millis(45),
        "heartbeat answered only after the slow block: {:?}",
        t0.elapsed()
    );
    // The bridge catches up and passes audio through again.
    let next = loop {
        match bridge.begin(&[]) {
            Err(BlockError::Busy) => std::thread::sleep(Duration::from_millis(2)),
            other => break other.unwrap(),
        }
    };
    let [left, right] = &mut bridge.output;
    let result = bridge
        .client
        .wait_done(next, Instant::now() + GENEROUS, [left, right])
        .unwrap();
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(bridge.output[0][..BLOCK], bridge.input[0][..BLOCK]);
    assert_eq!(bridge.shutdown(), EXIT_OK);
    eprintln!("VST3 slow: Late after {waited:.1?}, heartbeat {heartbeat:.1?} while it slept");
}
