//! Tests against VST3 plugins installed on this machine. Each is skipped when its bundle is
//! missing. Klopfer (the owner's nice-plug plugin) runs by default; the third-party plugins are
//! `#[ignore]`: `cargo test -p komponist-hosting -- --ignored vst3::tests::installed`.

use std::time::Instant;

use komponist_ipc::control::param_flags;
use komponist_ipc::{ParamInfo, PluginKind, WireEvent, WireEventKind};

use super::*;

pub(super) const KLOPFER: &str = "~/.vst3/Klopfer.vst3";
pub(super) const SURGE_XT: &str = "/usr/lib/vst3/Surge XT.vst3";
pub(super) const SURGE_XT_FX: &str = "/usr/lib/vst3/Surge XT Effects.vst3";
pub(super) const DEXED: &str = "/usr/lib/vst3/Dexed.vst3";
pub(super) const DECENT_SAMPLER: &str = "/usr/lib/vst3/DecentSampler.vst3";
pub(super) const SHORTCIRCUIT_XT: &str = "/usr/local/lib/vst3/Shortcircuit XT.vst3";

/// What a plugin should do in [`exercise`].
struct Expect {
    kind: PluginKind,
    /// Instruments: the notes played make sound. Effects: a sine input comes out.
    sound: bool,
    /// A changed parameter is part of the saved state.
    param_in_state: bool,
}

/// A parameter whose state round trip can be checked: visible, writable, not the bypass.
fn probe_param(params: &[ParamInfo]) -> Option<&ParamInfo> {
    let excluded = param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS;
    params
        .iter()
        .filter(|p| p.flags & excluded == 0 && p.flags & param_flags::AUTOMATABLE != 0)
        .find(|p| p.step_count == 0 || p.step_count >= 2)
}

/// A value of `p` different from `now`, on its step grid.
fn other_value(p: &ParamInfo, now: f64) -> f64 {
    let raw = if now > 0.5 { now - 0.4 } else { now + 0.4 };
    if p.step_count > 0 {
        let steps = f64::from(p.step_count);
        let snapped = (raw * steps).round() / steps;
        if (snapped - now).abs() < 0.5 / steps {
            (snapped + 1.0 / steps).min(1.0)
        } else {
            snapped
        }
    } else {
        raw
    }
}

/// Scan, load, params, activate, one second of processing with notes (instruments) or a sine
/// input (effects), a parameter's state round trip (also into a second instance), deactivate.
/// Prints a summary line for the report.
fn exercise(path: &str, expect: Expect) {
    let Some(bundle) = installed(path) else {
        return;
    };
    let started = Instant::now();
    let descriptors = crate::scan(&bundle).unwrap();
    let scanned = started.elapsed();
    assert!(!descriptors.is_empty());
    let d = descriptors[0].clone();
    assert_eq!(d.format, PluginFormat::Vst3);
    assert_eq!(d.kind, expect.kind, "{d:?}");

    let started = Instant::now();
    let mut rig = load(&bundle, &d.id);
    let loaded = started.elapsed();
    let plugin = rig.plugin.as_mut();
    let params = plugin.params();
    for p in params.iter().take(3) {
        assert!(p.min <= p.default && p.default <= p.max, "{p:?}");
        assert!(plugin.param_value(p.id).is_some(), "{p:?}");
    }
    let sample_text = params
        .iter()
        .find(|p| p.flags & param_flags::HIDDEN == 0)
        .and_then(|p| Some((p.name.clone(), plugin.param_text(p.id, p.default)?)));

    let mut processor = plugin.activate(CONFIG).unwrap();
    let latency = plugin.latency();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    let notes = [36u8, 38, 42, 48, 60, 64, 67];
    let mut peak = 0.0f32;
    let mut out_events = 0;
    let mut phase = 0.0f32;
    let total = (SAMPLE_RATE as usize) / BLOCK;
    for i in 0..total {
        let mut events = Vec::new();
        if expect.kind == PluginKind::Instrument {
            let kind = |key| {
                if i == 2 {
                    Some(WireEventKind::NoteOn {
                        channel: 0,
                        key,
                        velocity: 110,
                        note_id: u32::MAX,
                    })
                } else if i == total / 2 {
                    Some(WireEventKind::NoteOff {
                        channel: 0,
                        key,
                        note_id: u32::MAX,
                    })
                } else {
                    None
                }
            };
            events.extend(
                notes
                    .iter()
                    .filter_map(|&k| kind(k))
                    .map(|k| WireEvent::new(0, k)),
            );
        } else {
            for s in 0..BLOCK {
                let v = 0.5 * phase.sin();
                blocks.input[0][s] = v;
                blocks.input[1][s] = v;
                phase += std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32;
            }
        }
        let status = blocks.run(processor.as_mut(), &events);
        assert_ne!(status, ProcessStatus::Error, "block {i}");
        assert!(!blocks.has_nan(), "block {i}");
        peak = peak.max(blocks.peak(0..BLOCK));
        out_events += blocks.out_count;
    }
    processor.stop_processing();
    plugin.deactivate(processor);
    if expect.sound {
        assert!(peak > 0.01, "{}: peak {peak}", d.name);
    }

    // State: a changed parameter survives save → change back → load, in this instance and in a
    // fresh one (truce-rack failed this for every JUCE plugin, spike WP-15 §2).
    let probe = probe_param(&params).cloned();
    let mut round_trip = None;
    if let Some(p) = &probe {
        let before = plugin.param_value(p.id).unwrap();
        let wanted = other_value(p, before);
        plugin.flush_params(
            &[WireEvent::new(
                0,
                WireEventKind::ParamValue {
                    id: p.id,
                    value: wanted,
                },
            )],
            &mut Vec::new(),
        );
        let set = plugin.param_value(p.id).unwrap();
        round_trip = Some((p.name.clone(), before, set));
    }
    let state = plugin.save_state().unwrap();
    plugin.load_state(&state).unwrap();
    let mut other = load(&bundle, &d.id);
    other.plugin.load_state(&state).unwrap();
    if let (Some(p), Some((_, before, set)), true) = (&probe, &round_trip, expect.param_in_state) {
        assert!(
            (set - before).abs() > 1e-6,
            "{}: {} did not change ({before} → {set})",
            d.name,
            p.name
        );
        let restored = other.plugin.param_value(p.id).unwrap();
        assert!(
            (restored - set).abs() < 1e-3,
            "{}: {} restored {restored}, saved {set}",
            d.name,
            p.name
        );
    }
    drop(other);

    eprintln!(
        "{}: kind {:?}, features {:?}, {} params, scan {scanned:?}, load {loaded:?}, latency \
         {latency}, peak {peak:.3}, {out_events} output events, state {} bytes, state round trip \
         {round_trip:?}, text {sample_text:?}, GUI {}",
        d.name,
        d.kind,
        d.features,
        params.len(),
        state.len(),
        plugin.has_gui(),
    );
}

#[test]
fn klopfer() {
    // Klopfer's default kit has no samples loaded, so it may stay silent.
    exercise(
        KLOPFER,
        Expect {
            kind: PluginKind::Instrument,
            sound: false,
            param_in_state: true,
        },
    );
}

#[test]
#[ignore = "third-party plugin; run with --ignored"]
fn surge_xt() {
    exercise(
        SURGE_XT,
        Expect {
            kind: PluginKind::Instrument,
            sound: true,
            param_in_state: true,
        },
    );
}

#[test]
#[ignore = "third-party plugin; run with --ignored"]
fn surge_xt_effects() {
    exercise(
        SURGE_XT_FX,
        Expect {
            kind: PluginKind::Effect,
            sound: true,
            param_in_state: true,
        },
    );
}

#[test]
#[ignore = "third-party plugin; run with --ignored"]
fn dexed() {
    exercise(
        DEXED,
        Expect {
            kind: PluginKind::Instrument,
            sound: true,
            param_in_state: true,
        },
    );
}

#[test]
#[ignore = "third-party plugin; run with --ignored"]
fn decent_sampler() {
    // No sample library is loaded by default: silence is expected.
    exercise(
        DECENT_SAMPLER,
        Expect {
            kind: PluginKind::Instrument,
            sound: false,
            // Without a library its only parameter, "Main Volume", is not in its state (seen
            // after processed blocks too: the state XML has no field for it).
            param_in_state: false,
        },
    );
}

#[test]
#[ignore = "third-party plugin; run with --ignored"]
fn shortcircuit_xt() {
    // No sample is loaded by default: silence is expected.
    exercise(
        SHORTCIRCUIT_XT,
        Expect {
            kind: PluginKind::Instrument,
            sound: false,
            param_in_state: true,
        },
    );
}

/// The klopfer test kit's samples: `$KLOPFER_TESTKIT_DIR/samples` (klopfer's `testkit-out`,
/// generated by its `tools/testkit`). Unset: the test skips.
fn testkit_sample(name: &str) -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("KLOPFER_TESTKIT_DIR") else {
        eprintln!("skipped: KLOPFER_TESTKIT_DIR is not set");
        return None;
    };
    let path = Path::new(&dir).join("samples").join(name);
    match path.canonicalize() {
        Ok(path) => Some(path),
        Err(_) => {
            eprintln!("skipped: {} is missing", path.display());
            None
        }
    }
}

/// Klopfer with a kick on pad 1, set through its state (a host has no other way to load a
/// sample): its default state with `layers_1` pointing at the test kit's kick.
#[test]
fn klopfer_plays_a_sample_from_its_state() {
    let (Some(bundle), Some(kick)) = (installed(KLOPFER), testkit_sample("kick.wav")) else {
        return;
    };
    let id = crate::scan(&bundle).unwrap()[0].id.clone();
    let mut rig = load(&bundle, &id);
    let plugin = rig.plugin.as_mut();
    let blob = plugin.save_state().unwrap();
    let parts = crate::vst3::stream::decode_state(&blob).unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(parts.component).unwrap();
    let layers = serde_json::json!({
        "mode": "Velocity",
        "layers": [{"path": kick, "vel_lo": 1, "vel_hi": 127, "gain_db": 0.0, "tune": 0.0}],
    });
    json["fields"]["layers_1"] = serde_json::Value::String(layers.to_string());
    let component = serde_json::to_vec(&json).unwrap();
    let state = crate::vst3::stream::encode_state(&parts.cid, &component, parts.controller);
    plugin.load_state(&state).unwrap();

    let mut processor = plugin.activate(CONFIG).unwrap();
    processor.start_processing().unwrap();
    let mut blocks = Blocks::new();
    let note = WireEvent::new(
        0,
        WireEventKind::NoteOn {
            channel: 0,
            key: 36,
            velocity: 127,
            note_id: u32::MAX,
        },
    );
    // The sample may load in the background: retrigger until it sounds (at most 1 s).
    let mut peak = 0.0f32;
    let mut blocks_until_sound = None;
    for i in 0..(SAMPLE_RATE as usize / BLOCK) {
        let events = if i % 16 == 0 {
            std::slice::from_ref(&note)
        } else {
            &[]
        };
        blocks.run(processor.as_mut(), events);
        peak = peak.max(blocks.peak(0..BLOCK));
        if peak > 0.05 && blocks_until_sound.is_none() {
            blocks_until_sound = Some(i);
        }
    }
    processor.stop_processing();
    plugin.deactivate(processor);
    eprintln!(
        "Klopfer with the test kit's kick: peak {peak:.3}, sound after {blocks_until_sound:?} blocks"
    );
    assert!(peak > 0.05, "peak {peak}");
}
