//! Plugins installed on this machine through the bridge, as CLAP and as VST3: load, activate,
//! notes for one second of blocks, state into a second bridge, shutdown. Each is skipped when its
//! bundle is missing.
//!
//! `#[ignore]` (large third-party plugins, slow to load):
//! `cargo test -p komponist-bridge --test it installed:: -- --ignored`.

use crate::common;

use std::time::Instant;

use common::*;
use komponist_ipc::{BlockStatus, BridgeMsg, ControlMsg, PluginKind};

/// Load, process with notes, state round trip into a second bridge, shutdown. `sound`: the notes
/// must be audible (Klopfer's default kit has no samples, so it may stay silent).
fn exercise(path: &str, plugin_id: &str, sound: bool) {
    let Some(bundle) = installed(path) else {
        return;
    };
    let t0 = Instant::now();
    let mut bridge = Bridge::spawn();
    let info = bridge.load(&bundle, plugin_id);
    let loaded = t0.elapsed();
    assert_eq!(info.descriptor.kind, PluginKind::Instrument);
    assert_eq!(info.values.len(), info.params.len());
    let latency = bridge.activate();
    assert_eq!(bridge.region().latency(), latency);

    let notes = [36u8, 38, 42, 48, 60, 64, 67];
    let total = SAMPLE_RATE as usize / BLOCK;
    let mut peak = 0.0f32;
    let mut out_events = 0;
    for i in 0..total {
        let events: Vec<_> = if i == 2 {
            notes.iter().map(|&k| note_on(0, k)).collect()
        } else if i == total / 2 {
            notes.iter().map(|&k| note_off(0, k)).collect()
        } else {
            Vec::new()
        };
        let result = bridge.block(&events);
        assert_eq!(result.status, BlockStatus::Ok as u32, "block {i}");
        assert!(
            bridge
                .output
                .iter()
                .all(|c| c[..BLOCK].iter().all(|s| s.is_finite())),
            "block {i}"
        );
        peak = peak.max(bridge.peak(0..BLOCK));
        while bridge.monitor.pop_output_event().is_some() {
            out_events += 1;
        }
    }
    if sound {
        assert!(peak > 0.01, "{}: peak {peak}", info.descriptor.name);
    }

    bridge.send(ControlMsg::GetState { request: 1 });
    let state = bridge.expect("State", |m| match m {
        BridgeMsg::State { request: 1, state } => Some(state.clone()),
        _ => None,
    });
    let heartbeat = bridge.heartbeat();
    assert_eq!(bridge.shutdown(), EXIT_OK);

    // The state into a new bridge, as after a crash: no error, and it processes.
    let mut second = Bridge::spawn();
    second.load(&bundle, plugin_id);
    second.send(ControlMsg::SetState {
        state: state.clone(),
    });
    // An `Error` for the state would fail `expect`.
    second.sync();
    second.activate();
    for _ in 0..10 {
        second.block(&[]);
    }
    assert_eq!(second.shutdown(), EXIT_OK);
    eprintln!(
        "{} through the bridge: load {loaded:.0?}, {} params, latency {latency}, peak {peak:.3}, \
         {out_events} output events, state {} bytes, heartbeat {heartbeat:.1?}",
        info.descriptor.name,
        info.params.len(),
        state.len(),
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn klopfer() {
    exercise("~/.clap/Klopfer.clap", "com.klopfer.drum-sampler", false);
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt() {
    exercise(
        "/usr/lib/clap/Surge XT.clap",
        "org.surge-synth-team.surge-xt",
        true,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed() {
    exercise(
        "/usr/lib/clap/Dexed.clap",
        "com.digital-suburban.dexed",
        true,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn klopfer_vst3() {
    exercise(
        "~/.vst3/Klopfer.vst3",
        "4B6C6F706665724472756D536D706C72",
        false,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt_vst3() {
    exercise(
        "/usr/lib/vst3/Surge XT.vst3",
        "ABCDEF019182FAEB566D624153675854",
        true,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed_vst3() {
    exercise(
        "/usr/lib/vst3/Dexed.vst3",
        "ABCDEF019182FAEB4447534244657864",
        true,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn decent_sampler_vst3() {
    exercise(
        "/usr/lib/vst3/DecentSampler.vst3",
        "ABCDEF019182FAEB446C647944736D70",
        false,
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn shortcircuit_xt_vst3() {
    exercise(
        "/usr/local/lib/vst3/Shortcircuit XT.vst3",
        "7B791499693F5465B590BA9435E6D873",
        false,
    );
}
