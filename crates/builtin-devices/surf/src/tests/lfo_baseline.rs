//! The LFO behaviour of surf pinned before #352 moves volt's LFO into a shared component: render
//! digests (FNV-1a over the output bits) of LFO-heavy setups and presets, committed for the
//! reference platform (Linux x86_64); elsewhere they are printed and not compared. Every block
//! split (1, 31, 32, 33, 64, 256, irregular) must give the same bits; that is checked on every
//! platform. surf runs volt's engine, so it follows volt's LFO (`klang_synth::CyclicLfo`, #361).

use komponist_device_core::InternalProcessor;
use komponist_device_dsp::va_curve::{dst, src};
use komponist_processor::{EventKind, Processor};

use super::baseline_support::{check_digests, split_independent_digest};
use super::{BLOCK, RATE, cc, off, on, render, wire};

/// One second: a note, a second one off the 32-sample grid, a third, a retrigger of the first
/// (the same key again), the mod wheel, releases and a late note.
fn performance() -> Vec<(u32, EventKind)> {
    vec![
        (0, on(60, 100)),
        (1000, on(64, 80)),
        (5003, on(67, 120)),
        (9001, on(60, 90)),
        (12_000, cc(1, 100)),
        (20_000, off(64)),
        (26_000, off(60)),
        (26_000, off(67)),
        (30_011, on(72, 70)),
        (40_000, off(72)),
    ]
}

const FRAMES: usize = 48_000;

/// Matrix slot `n` (1 …4): `source` → `dest` by `amount`.
fn slot(n: usize, source: usize, dest: usize, amount: f64) -> [(&'static str, f64); 3] {
    const NAMES: [[&str; 3]; 4] = [
        ["mod1_src", "mod1_dst", "mod1_amt"],
        ["mod2_src", "mod2_dst", "mod2_amt"],
        ["mod3_src", "mod3_dst", "mod3_amt"],
        ["mod4_src", "mod4_dst", "mod4_amt"],
    ];
    let [s, d, a] = NAMES[n - 1];
    [(s, source as f64), (d, dest as f64), (a, amount)]
}

/// The defaults with `changes`, as values by wire id.
fn values(changes: &[(&str, f64)]) -> Vec<f64> {
    let mut values = crate::SPEC.defaults();
    for (key, value) in changes {
        values[wire(key) as usize] = *value;
    }
    values
}

/// The values of preset `name`.
fn preset(name: &str) -> Vec<f64> {
    let index = crate::presets::presets()
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("no preset {name}"));
    crate::SPEC.preset_values(index).unwrap()
}

/// Renders `values` with every split; asserts they give the same bits and returns the digest.
fn render_digest(values: &[f64]) -> u64 {
    split_independent_digest(FRAMES, &performance(), |calls| {
        let mut p = InternalProcessor::from_dsp(&crate::SPEC, values, crate::dsp());
        p.activate(RATE, BLOCK).unwrap();
        let mut out = [Vec::new(), Vec::new()];
        for (n, here) in calls {
            let part = render(&mut p, n, &here, 120.0);
            out[0].extend_from_slice(&part[0]);
            out[1].extend_from_slice(&part[1]);
        }
        out
    })
}

/// Sample-and-hold on position A, smooth random on the pitch, the LFOs' rates modulated by the
/// third envelope and by LFO 1, fades, unipolar output, unison.
fn lfo_matrix() -> Vec<f64> {
    let mut all = vec![
        ("lfo1_shape", 4.0),
        ("lfo1_rate", 6.0),
        ("lfo1_fade", 0.3),
        ("lfo1_uni", 1.0),
        ("lfo2_shape", 5.0),
        ("lfo2_rate", 3.0),
        ("lfo2_fade", 0.05),
        ("mod_a", 0.3),
        ("mod_d", 0.4),
        ("mod_s", 0.2),
        ("unison", 3.0),
        ("analog", 0.5),
    ];
    all.extend(slot(1, src::LFO1, dst::PW_A, 0.5));
    all.extend(slot(2, src::LFO2, dst::PITCH, 0.1));
    all.extend(slot(3, src::ENV3, dst::LFO1_RATE, 0.8));
    all.extend(slot(4, src::LFO1, dst::LFO2_RATE, -0.6));
    values(&all)
}

/// Both LFOs free-running: a fast unipolar sine on position B and a tempo-synced smooth random
/// on the cutoff; the mod wheel's vibrato; mono.
fn free_lfos() -> Vec<f64> {
    let mut all = vec![
        ("lfo1_key", 0.0),
        ("lfo1_shape", 0.0),
        ("lfo1_rate", 13.0),
        ("lfo1_uni", 1.0),
        ("lfo2_key", 0.0),
        ("lfo2_shape", 5.0),
        ("lfo2_sync", 1.0),
        ("lfo2_div", 6.0),
        ("wheel_vib", 50.0),
        ("mode", 1.0),
    ];
    all.extend(slot(1, src::LFO1, dst::PW_B, 0.4));
    all.extend(slot(2, src::LFO2, dst::CUTOFF, 0.6));
    values(&all)
}

/// Factory presets that use their LFOs: synced (Growl Bass), free triangle and smooth random
/// (Vowel Pad), keyed smooth random (Random Drift), keyed triangle (Pulse Lead), synced and
/// keyed sample-and-hold (Bit Glitch).
const PRESETS: [&str; 5] = [
    "Growl Bass",
    "Vowel Pad",
    "Random Drift",
    "Pulse Lead",
    "Bit Glitch",
];

#[test]
fn surf_renders_match_the_committed_digests() {
    let mut renders = vec![
        ("lfo-matrix".to_string(), lfo_matrix()),
        ("free-lfos".to_string(), free_lfos()),
    ];
    for name in PRESETS {
        renders.push((name.to_string(), preset(name)));
    }
    let actual: Vec<(String, u64)> = renders
        .into_iter()
        .map(|(name, values)| (name, render_digest(&values)))
        .collect();
    check_digests("surf", &actual, SURF_DIGESTS);
}

/// The committed digests (Linux x86_64), from `e97f261c`, before #352. Every block split gives
/// the same bits, so there is one per render.
const SURF_DIGESTS: &[(&str, u64)] = &[
    ("lfo-matrix", 0x2cc2d769a9704901),
    ("free-lfos", 0x5eb6144bd547993d),
    ("Growl Bass", 0x08ceb18e9acaaf5e),
    ("Vowel Pad", 0x0bf2691e965f05c5),
    ("Random Drift", 0x0ff4e4bf16b81678),
    ("Pulse Lead", 0x9831ca2cf22a5956),
    ("Bit Glitch", 0x63854631a7338529),
];
