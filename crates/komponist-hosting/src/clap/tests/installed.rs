//! Tests against plugins installed on this machine. Each is skipped when its bundle is missing.
//! Klopfer (the owner's nice-plug plugin) runs by default; the third-party plugins are
//! `#[ignore]`: `cargo test -p komponist-hosting -- --ignored installed`.

use std::time::Instant;

use komponist_ipc::{PluginKind, WireEventKind};

use super::*;
use crate::clap::events::NO_NOTE_ID;

pub(super) const KLOPFER: &str = "~/.clap/Klopfer.clap";
pub(super) const SURGE_XT: &str = "/usr/lib/clap/Surge XT.clap";
pub(super) const SURGE_XT_FX: &str = "/usr/lib/clap/Surge XT Effects.clap";
pub(super) const DEXED: &str = "/usr/lib/clap/Dexed.clap";
pub(super) const SHORTCIRCUIT_XT: &str = "/usr/local/lib/clap/Shortcircuit XT.clap";

/// What a plugin should do in [`exercise`].
struct Expect {
    kind: PluginKind,
    /// Instruments: the notes played make sound. Effects: a sine input comes out.
    sound: bool,
}

/// Scan, load, params, activate, one second of processing with notes (instruments) or a sine
/// input (effects), state round trip, deactivate. Prints a summary line for the report.
fn exercise(path: &str, expect: Expect) {
    let Some(bundle) = installed(path) else {
        return;
    };
    let started = Instant::now();
    let descriptors = crate::scan(&bundle).unwrap();
    assert!(!descriptors.is_empty());
    let d = descriptors[0].clone();
    assert_eq!(d.kind, expect.kind, "{d:?}");

    let mut rig = load(&bundle, &d.id);
    let loaded = started.elapsed();
    let plugin = rig.plugin.as_mut();
    let params = plugin.params();
    for p in params.iter().take(3) {
        assert!(p.min <= p.default && p.default <= p.max, "{p:?}");
        assert!(plugin.param_value(p.id).is_some(), "{p:?}");
    }

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
                        note_id: NO_NOTE_ID,
                    })
                } else if i == total / 2 {
                    Some(WireEventKind::NoteOff {
                        channel: 0,
                        key,
                        note_id: NO_NOTE_ID,
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
        assert_ne!(status, crate::ProcessStatus::Error, "block {i}");
        assert!(!blocks.has_nan(), "block {i}");
        peak = peak.max(blocks.peak(0..BLOCK));
        out_events += blocks.out_count;
    }
    processor.stop_processing();
    plugin.deactivate(processor);
    if expect.sound {
        assert!(peak > 0.01, "{}: peak {peak}", d.name);
    }

    let state = plugin.save_state().unwrap();
    plugin.load_state(&state).unwrap();
    // The state also loads into a second instance.
    let mut other = load(&bundle, &d.id);
    other.plugin.load_state(&state).unwrap();
    drop(other);

    eprintln!(
        "{}: kind {:?}, {} params, load {:?}, latency {latency}, peak {peak:.3}, \
         {out_events} output events, state {} bytes, note names {}, GUI {}",
        d.name,
        d.kind,
        params.len(),
        loaded,
        state.len(),
        plugin.note_names().len(),
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
        },
    );
}
