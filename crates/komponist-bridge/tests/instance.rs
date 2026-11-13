//! Instance mode against the WP-27 fixtures, through the real binary and the full IPC path
//! (control socket, memfd, futex handshake, event rings). PLAN WP-31 "Done when": sound through
//! the full IPC path; a parameter change from the app reaches the plugin; state round trip;
//! `kp-test-crash` kills only its bridge; `kp-test-slow` produces `Late` without blocking. Plus
//! the protocol around them: errors, deactivation, render mode, RT priority, watchdog heartbeat,
//! shutdown, non-dumpable bridges.
//!
//! Each test is skipped with a note when its fixture is missing (`scripts/build-test-plugins.sh`).

mod common;

use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::shm::{BLOCK_FLAG_RESET, MAX_BLOCK};
use komponist_ipc::{
    BlockError, BlockStatus, BridgeMsg, BridgeState, ControlMsg, ErrorContext, LogLevel,
    PluginFormat, PluginKind, ProcessExit, RenderMode,
};

/// Waits until the bridge publishes `state` (it changes asynchronously to control messages).
fn wait_state(bridge: &Bridge, state: BridgeState) {
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    while bridge.region().bridge_state() != state {
        assert!(
            Instant::now() < deadline,
            "bridge state never became {state:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn protocol_errors_keep_the_bridge_alive() {
    let mut bridge = Bridge::spawn();
    assert_eq!(bridge.region().bridge_state(), BridgeState::Starting);
    // The heartbeat works before anything is loaded.
    bridge.send(ControlMsg::ListParams);
    let params = bridge.expect("Params", |m| match m {
        BridgeMsg::Params(p) => Some(p.clone()),
        _ => None,
    });
    assert!(params.is_empty());

    bridge.send(ControlMsg::Activate {
        sample_rate: SAMPLE_RATE,
        max_block: 256,
    });
    bridge.expect_error(ErrorContext::Activate);
    bridge.send(ControlMsg::GetState { request: 1 });
    bridge.expect_error(ErrorContext::State);
    bridge.send(ControlMsg::SetState { state: vec![1] });
    bridge.expect_error(ErrorContext::State);
    bridge.send(ControlMsg::SetParam { id: 0, value: 0.0 });
    bridge.expect_error(ErrorContext::Params);
    bridge.send(ControlMsg::OpenGui {
        parent_xid: 1,
        scale: 1.0,
    });
    bridge.expect_error(ErrorContext::Gui);
    bridge.send(ControlMsg::SetRenderMode(RenderMode::Offline));
    bridge.expect_error(ErrorContext::Activate);
    // Nothing to deactivate or close: no answer, no harm.
    bridge.send(ControlMsg::Deactivate);
    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| (*m == BridgeMsg::GuiClosed).then_some(()));

    bridge.send(ControlMsg::Load {
        format: PluginFormat::Clap,
        bundle: "/nonexistent/X.clap".into(),
        plugin_id: SYNTH.into(),
    });
    bridge.expect_error(ErrorContext::Load);
    // VST3 arrives with WP-32: the hosting layer's `Unsupported`, cleanly.
    bridge.send(ControlMsg::Load {
        format: PluginFormat::Vst3,
        bundle: "/usr/lib/vst3/X.vst3".into(),
        plugin_id: "0123456789ABCDEF0123456789ABCDEF".into(),
    });
    let message = bridge.expect_error(ErrorContext::Load);
    assert!(message.contains("unsupported"), "{message}");
    assert_eq!(bridge.region().bridge_state(), BridgeState::Starting);

    if let Some(bundle) = fixture("kp-test-synth") {
        bridge.send(ControlMsg::Load {
            format: PluginFormat::Clap,
            bundle: bundle.clone(),
            plugin_id: "no.such.plugin".into(),
        });
        bridge.expect_error(ErrorContext::Load);
        bridge.load(&bundle, SYNTH);
        bridge.send(ControlMsg::Load {
            format: PluginFormat::Clap,
            bundle,
            plugin_id: SYNTH.into(),
        });
        bridge.expect_error(ErrorContext::Load);
        for max_block in [0, MAX_BLOCK as u32 + 1] {
            bridge.send(ControlMsg::Activate {
                sample_rate: SAMPLE_RATE,
                max_block,
            });
            bridge.expect_error(ErrorContext::Activate);
        }
        bridge.send(ControlMsg::Activate {
            sample_rate: 0.0,
            max_block: 256,
        });
        bridge.expect_error(ErrorContext::Activate);
        bridge.activate();
        bridge.send(ControlMsg::Activate {
            sample_rate: SAMPLE_RATE,
            max_block: 256,
        });
        bridge.expect_error(ErrorContext::Activate);
        bridge.send(ControlMsg::SetState {
            state: b"not a state".to_vec(),
        });
        bridge.expect_error(ErrorContext::State);
        // Still serving blocks and the heartbeat.
        bridge.block(&[]);
        bridge.heartbeat();
    }
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn synth_sounds_through_the_ipc_path() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, SYNTH);
    assert_eq!(info.descriptor.id, SYNTH);
    assert_eq!(info.descriptor.kind, PluginKind::Instrument);
    assert!(info.has_gui);
    assert_eq!(info.latency, 0);
    let names: Vec<&str> = info.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gain", "Decay"]);
    assert_eq!(info.values.len(), 2);
    let gain = Bridge::param_id(&info, "Gain");
    let gain_value = info.values.iter().find(|v| v.id == gain).unwrap();
    assert!((gain_value.value - 0.8).abs() < 1e-6);
    assert!(gain_value.text.contains("dB"), "{gain_value:?}");
    assert_eq!(bridge.region().bridge_state(), BridgeState::Loaded);

    // Loaded but not active: the app's RT side is refused, nothing is published.
    assert_eq!(bridge.block_with(&[], GENEROUS), Err(BlockError::NotActive));

    assert_eq!(bridge.activate(), 0);
    assert_eq!(bridge.region().bridge_state(), BridgeState::Active);
    let result = bridge.block(&[]);
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(bridge.peak(0..BLOCK), 0.0);

    // A note at frame 100: silent before, a sine at velocity 1 × −12 dB ≈ 0.25 after, L = R.
    let result = bridge.block(&[note_on(100, 69)]);
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(result.dropped_events, 0);
    assert_eq!(bridge.peak(0..100), 0.0);
    let level = bridge.peak(100..BLOCK);
    assert!((0.2..0.3).contains(&level), "level {level}");
    assert_eq!(bridge.output[0][..BLOCK], bridge.output[1][..BLOCK]);

    // Held for 50 blocks (267 ms): it keeps sounding and falls by 60 dB per second (Decay 1 s).
    for _ in 0..50 {
        bridge.block(&[]);
    }
    let held = bridge.peak(0..BLOCK);
    assert!((0.02..0.08).contains(&held), "level after 267 ms {held}");
    // Note off: the 10 ms release ends within a few blocks.
    bridge.block(&[note_off(0, 69)]);
    for _ in 0..4 {
        bridge.block(&[]);
    }
    assert!(bridge.peak(0..BLOCK) < 1e-4, "{}", bridge.peak(0..BLOCK));

    // The engine's reset flag (stop, locate, loop jump) silences a sounding note at once.
    bridge.block(&[note_on(0, 72)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);
    bridge.next_flags = BLOCK_FLAG_RESET;
    bridge.block(&[]);
    assert_eq!(bridge.peak(0..BLOCK), 0.0);
    // No output events from a synth without GUI interaction.
    assert_eq!(bridge.monitor.pop_output_event(), None);

    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn effect_audio_and_latency_through_the_ipc_path() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, FX);
    assert_eq!(info.descriptor.kind, PluginKind::Effect);
    assert!(!info.has_gui);
    assert_eq!(info.latency, 0, "inactive");
    assert_eq!(bridge.activate(), 64);
    assert_eq!(bridge.region().latency(), 64);
    // nice-plug reports the latency from `activate`; the bridge serves that before processing
    // starts, so it does not turn into a restart.
    bridge.sync();
    assert!(
        !bridge.seen.iter().any(|m| matches!(
            m,
            BridgeMsg::RestartRequested | BridgeMsg::LatencyChanged(_)
        )),
        "{:?}",
        bridge.seen
    );
    assert_eq!(bridge.region().bridge_state(), BridgeState::Active);

    bridge.input[0][10] = 1.0;
    bridge.input[1][20] = -0.5;
    bridge.block(&[]);
    let (left, right) = (&bridge.output[0][..BLOCK], &bridge.output[1][..BLOCK]);
    assert_eq!(left[74], 1.0);
    assert_eq!(right[84], -0.5);
    assert_eq!(left.iter().filter(|s| **s != 0.0).count(), 1);
    assert_eq!(right.iter().filter(|s| **s != 0.0).count(), 1);
    assert_eq!(bridge.shutdown(), EXIT_OK);

    // A block above the activated maximum is not given to the plugin: the effect's fallback is
    // the dry input, flagged as a plugin error.
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, FX);
    bridge.activate_with(SAMPLE_RATE, BLOCK as u32 / 2);
    for (i, s) in bridge.input[0].iter_mut().enumerate() {
        *s = i as f32;
    }
    let result = bridge.block(&[]);
    assert_eq!(result.status, BlockStatus::PluginError as u32);
    assert_eq!(bridge.output[0][..BLOCK], bridge.input[0][..BLOCK]);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn param_changes_from_the_app_reach_the_plugin() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, SYNTH);
    let gain = Bridge::param_id(&info, "Gain");

    // Inactive: through `flush_params`; the echo carries the plugin's text.
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

    // Active: the value reaches the next block as an event at offset 0.
    bridge.send(ControlMsg::SetParam {
        id: gain,
        value: 0.0,
    });
    // The echo is sent after the value was queued for the RT thread.
    assert_eq!(bridge.expect_value(gain).0, 0.0);
    bridge.block(&[]);
    let quiet = bridge.peak(0..BLOCK);
    assert!(quiet < level * 0.05, "−60 dB expected, level {quiet}");
    bridge.send(ControlMsg::SetParam {
        id: gain,
        value: 1.0,
    });
    assert_eq!(bridge.expect_value(gain).0, 1.0);
    bridge.block(&[]);
    let loud = bridge.peak(0..BLOCK);
    assert!((0.9..1.01).contains(&loud), "0 dB expected, level {loud}");

    // Automation from the engine travels through the shared event ring.
    bridge.block(&[param(0, gain, 0.5)]);
    let level = bridge.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");

    // Deactivated and active again: the plugin kept the last value.
    bridge.send(ControlMsg::Deactivate);
    wait_state(&bridge, BridgeState::Loaded);
    bridge.activate();
    bridge.block(&[note_on(0, 60)]);
    let level = bridge.peak(0..BLOCK);
    assert!((0.025..0.04).contains(&level), "level {level}");
    assert_eq!(bridge.shutdown(), EXIT_OK);
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
fn state_round_trips_between_bridges() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut first = Bridge::spawn();
    let info = first.load(&bundle, SYNTH);
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
    assert!(!state.is_empty());
    assert_eq!(first.shutdown(), EXIT_OK);

    // A new bridge, as after a crash or on project load: Load, SetState, then Activate.
    let mut second = Bridge::spawn();
    second.load(&bundle, SYNTH);
    second.send(ControlMsg::SetState {
        state: state.clone(),
    });
    // After a state load the bridge reports every value.
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
    let loud = play(&mut second, 70);
    assert!(loud > 0.5, "0 dB expected, level {loud}");
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
fn crash_kills_only_its_bridge() {
    let (Some(crash_bundle), Some(synth_bundle)) =
        (fixture("kp-test-crash"), fixture("kp-test-synth"))
    else {
        return;
    };
    let mut synth = Bridge::spawn();
    synth.load(&synth_bundle, SYNTH);
    synth.activate();
    synth.block(&[note_on(0, 69)]);
    assert!(synth.peak(0..BLOCK) > 0.1);

    let mut crash = Bridge::spawn();
    let info = crash.load(&crash_bundle, CRASH);
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
    assert_eq!(
        crash.channel.recv(),
        Err(komponist_ipc::ChannelError::Closed)
    );
    // The app's RT side degrades without blocking.
    assert_eq!(crash.begin(&[]), Err(BlockError::Busy));

    // The other bridge plays on.
    assert_eq!(synth.watcher.try_exit().unwrap(), None);
    for key in 40..60 {
        synth.block(&[note_on(0, key)]);
        assert!(synth.peak(0..BLOCK) > 0.1);
    }
    synth.heartbeat();

    // The parameter path: Crash rising past 0.9 while active (set after loading, PLAN WP-27).
    let mut crash = Bridge::spawn();
    crash.load(&crash_bundle, CRASH);
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
    eprintln!(
        "crash: SIGSEGV in process detected through the pidfd {detected:.1?} after the request"
    );
}

#[test]
fn slow_plugin_is_late_without_blocking() {
    let Some(bundle) = fixture("kp-test-slow") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, SLOW);
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
    assert!(waited >= RT_DEADLINE, "{waited:?}");
    assert!(
        waited < Duration::from_millis(30),
        "the app waited for the plugin: {waited:?}"
    );

    // The bridge main loop is not blocked by the sleeping RT thread: the heartbeat comes back.
    let heartbeat = bridge.heartbeat();
    let heartbeat_done = t0.elapsed();

    // The next blocks find the bridge busy and publish nothing until it finished the late one.
    let mut busy = 0;
    let next = loop {
        match bridge.begin(&[]) {
            Err(BlockError::Busy) => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            other => break other.unwrap(),
        }
    };
    let resynchronised = t0.elapsed();
    assert!(busy >= 1);
    assert_eq!(next, late.wrapping_add(1), "Busy blocks published nothing");
    assert!(
        resynchronised >= Duration::from_millis(45),
        "{resynchronised:?}"
    );
    let [left, right] = &mut bridge.output;
    let result = bridge
        .client
        .wait_done(next, Instant::now() + GENEROUS, [left, right])
        .unwrap();
    assert_eq!(result.status, BlockStatus::Ok as u32);
    assert_eq!(bridge.output[0][..BLOCK], bridge.input[0][..BLOCK]);
    assert!(
        heartbeat_done < Duration::from_millis(45),
        "heartbeat answered only after the slow block: {heartbeat_done:?}"
    );
    assert_eq!(bridge.shutdown(), EXIT_OK);
    eprintln!(
        "slow: Late after {waited:.1?} (deadline {RT_DEADLINE:?}), heartbeat {heartbeat:.1?} \
         while the plugin slept, {busy} Busy, resynchronised after {resynchronised:.1?}"
    );
}

#[test]
fn rt_thread_runs_at_sched_fifo_or_says_why() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, FX);
    bridge.activate();
    let logs = bridge.rt_logs();
    assert_eq!(logs.len(), 1, "{:?}", bridge.seen);
    let (level, message) = &logs[0];
    let policy = rt_thread_policy(bridge.pid()).expect("the RT thread kp-bridge-rt");
    match level {
        LogLevel::Info => assert_eq!(policy, (1, 80), "{message}"),
        // A machine without an RT rlimit: reported, and processing goes on.
        LogLevel::Warning => assert_eq!(policy, (0, 0), "{message}"),
        other => panic!("{other:?}: {message}"),
    }
    bridge.block(&[]);
    // A restart (render mode) does not repeat an unchanged report.
    bridge.send(ControlMsg::SetRenderMode(RenderMode::Offline));
    bridge.expect("Activated", |m| {
        matches!(m, BridgeMsg::Activated { .. }).then_some(())
    });
    bridge.sync();
    assert_eq!(bridge.rt_logs().len(), 1);
    assert_eq!(bridge.shutdown(), EXIT_OK);
    eprintln!("RT priority report: {level:?} {message:?}, thread (policy, priority) {policy:?}");

    // `--rt-priority` picks another priority.
    let mut bridge = Bridge::spawn_with(SpawnOptions {
        args: vec!["--rt-priority".into(), "70".into()],
        ..SpawnOptions::default()
    });
    bridge.load(&bundle, FX);
    bridge.activate();
    if bridge.rt_logs()[0].0 == LogLevel::Info {
        assert_eq!(rt_thread_policy(bridge.pid()), Some((1, 70)));
    }
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn refused_rt_priority_is_reported_and_processing_goes_on() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    if rustix::process::geteuid().is_root() {
        eprintln!("skipped: root may use SCHED_FIFO whatever RLIMIT_RTPRIO says");
        return;
    }
    let mut bridge = Bridge::spawn_with(SpawnOptions {
        rtprio_limit: Some(0),
        ..SpawnOptions::default()
    });
    bridge.load(&bundle, FX);
    assert_eq!(bridge.activate(), 64);
    let logs = bridge.rt_logs();
    assert_eq!(logs.len(), 1, "{:?}", bridge.seen);
    assert_eq!(logs[0].0, LogLevel::Warning, "{:?}", logs[0]);
    assert!(logs[0].1.contains("SCHED_FIFO 80"), "{:?}", logs[0]);
    assert_eq!(rt_thread_policy(bridge.pid()), Some((0, 0)));
    bridge.input[0][0] = 1.0;
    bridge.block(&[]);
    assert_eq!(bridge.output[0][64], 1.0);
    assert_eq!(bridge.shutdown(), EXIT_OK);
    eprintln!("refused RT priority: {:?}", logs[0]);
}

#[test]
fn deactivation_restart_and_render_mode() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    bridge.block(&[note_on(0, 60)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);

    bridge.send(ControlMsg::Deactivate);
    wait_state(&bridge, BridgeState::Loaded);
    assert_eq!(bridge.begin(&[]), Err(BlockError::NotActive));

    // Again, at another rate; the plugin starts from silence.
    bridge.activate_at(44_100.0);
    bridge.block(&[]);
    assert_eq!(bridge.peak(0..BLOCK), 0.0);
    bridge.block(&[note_on(0, 60)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);

    // Render mode while active: deactivate, switch, activate again.
    bridge.send(ControlMsg::SetRenderMode(RenderMode::Offline));
    bridge.expect("Activated", |m| {
        matches!(m, BridgeMsg::Activated { .. }).then_some(())
    });
    wait_state(&bridge, BridgeState::Active);
    bridge.block(&[note_on(0, 64)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);
    // Inactive: no `Activated`, the next activation uses the mode.
    bridge.send(ControlMsg::Deactivate);
    bridge.send(ControlMsg::SetRenderMode(RenderMode::Realtime));
    bridge.activate();
    bridge.block(&[note_on(0, 64)]);
    assert!(bridge.peak(0..BLOCK) > 0.1);
    assert!(
        !bridge
            .seen
            .iter()
            .any(|m| matches!(m, BridgeMsg::Activated { .. })),
        "{:?}",
        bridge.seen
    );
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn heartbeat_is_answered_while_processing() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    let mut times = Vec::new();
    for i in 0..200u32 {
        if i % 50 == 0 {
            bridge.block(&[note_on(0, 60 + (i / 50) as u8)]);
        } else {
            bridge.block(&[]);
        }
        times.push(bridge.heartbeat());
    }
    times.sort_unstable();
    let (median, max) = (times[times.len() / 2], times[times.len() - 1]);
    assert!(max < Duration::from_millis(500), "max {max:?}");
    assert_eq!(bridge.shutdown(), EXIT_OK);
    eprintln!(
        "heartbeat (ListParams → Params) while processing: median {median:.1?}, max {max:.1?}"
    );
}

#[test]
fn bridge_exits_when_the_app_goes_away() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    let mut bridge = Bridge::spawn();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    bridge.block(&[note_on(0, 60)]);
    // As if the app died: its end of the socket closes.
    rustix::net::shutdown(&bridge.channel, rustix::net::Shutdown::Both).unwrap();
    let t0 = Instant::now();
    assert_eq!(bridge.wait_exit(), EXIT_OK);
    eprintln!(
        "bridge exited {:.1?} after the app's socket closed",
        t0.elapsed()
    );
}

#[test]
fn bridges_are_not_dumpable_unless_asked() {
    let uid = rustix::process::getuid().as_raw();
    // The files in /proc/<pid> of a non-dumpable process belong to root.
    let mut bridge = Bridge::spawn();
    let owner = std::fs::metadata(format!("/proc/{}/stat", bridge.pid()))
        .unwrap()
        .uid();
    assert_eq!(owner, 0, "dumpable bridge");
    assert_eq!(bridge.shutdown(), EXIT_OK);

    let mut bridge = Bridge::spawn_with(SpawnOptions {
        env: vec![("KOMPONIST_BRIDGE_COREDUMP".into(), "1".into())],
        ..SpawnOptions::default()
    });
    let owner = std::fs::metadata(format!("/proc/{}/stat", bridge.pid()))
        .unwrap()
        .uid();
    assert_eq!(
        owner, uid,
        "KOMPONIST_BRIDGE_COREDUMP=1 keeps the bridge dumpable"
    );
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn instance_mode_needs_an_open_control_fd() {
    let output = std::process::Command::new(BRIDGE)
        .args(["--instance", "--control-fd", "999"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("control fd 999"), "{stderr}");
}
