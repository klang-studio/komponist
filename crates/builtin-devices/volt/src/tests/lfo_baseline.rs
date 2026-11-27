//! The LFO behaviour of volt pinned before #352 moved it onto `klang_synth::CyclicLfo`: an
//! FNV-1a hash over the output bits of representative renders, committed for the reference
//! platform (Linux x86_64); elsewhere the digests are printed and not compared. volt steps its
//! LFOs once per control interval, so every block split must give the same bits; that is checked
//! on every platform. (The differential oracle against a verbatim copy of the old `Lfo` proved
//! the adoption bit for bit and was removed with #361.)

use komponist_device_dsp::va_curve::{dst, src};
use komponist_processor::EventKind;

use super::baseline_support::{check_digests, split_independent_digest};
use super::{bend, cc, off, on, pressure, processor, render, slot, wire};

/// One second: a note, a second one off the 32-sample grid, a third, a retrigger of the first
/// (the same key again), the wheels, releases and a late note.
fn performance() -> Vec<(u32, EventKind)> {
    vec![
        (0, on(60, 100)),
        (1000, on(64, 80)),
        (5003, on(67, 120)),
        (9001, on(60, 90)),
        (12_000, cc(1, 100)),
        (13_500, bend(3000)),
        (14_017, pressure(90)),
        (20_000, off(64)),
        (26_000, off(60)),
        (26_000, off(67)),
        (30_011, on(72, 70)),
        (40_000, off(72)),
    ]
}

const FRAMES: usize = 48_000;

/// Renders `changes` with every split; asserts they give the same bits and returns the digest.
fn render_digest(changes: &[(&str, f64)]) -> u64 {
    split_independent_digest(FRAMES, &performance(), |calls| {
        let mut p = processor(changes);
        let mut out = [Vec::new(), Vec::new()];
        for (n, here) in calls {
            let part = render(&mut p, n, &here, 120.0);
            out[0].extend_from_slice(&part[0]);
            out[1].extend_from_slice(&part[1]);
        }
        out
    })
}

/// The values of preset `name`, as changes by model id.
fn preset(name: &str) -> Vec<(&'static str, f64)> {
    let index = crate::presets::presets()
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("no preset {name}"));
    let values = crate::SPEC.preset_values(index).unwrap();
    super::KEYS
        .iter()
        .map(|key| (*key, values[wire(key) as usize]))
        .collect()
}

/// Sample-and-hold into the cutoff, smooth random into the pitch, the LFOs' rates modulated by
/// the third envelope and by LFO 1, fades, unipolar output, unison.
fn lfo_matrix() -> Vec<(&'static str, f64)> {
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
    all.extend(slot(1, src::LFO1, dst::CUTOFF, 0.5));
    all.extend(slot(2, src::LFO2, dst::PITCH, 0.1));
    all.extend(slot(3, src::ENV3, dst::LFO1_RATE, 0.8));
    all.extend(slot(4, src::LFO1, dst::LFO2_RATE, -0.6));
    all
}

/// Both LFOs free-running (shared by all voices): a fast unipolar sine on the pulse width and
/// a tempo-synced smooth random on the cutoff; the mod wheel's vibrato; mono with retrigger.
fn free_lfos() -> Vec<(&'static str, f64)> {
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
    all.extend(slot(1, src::LFO1, dst::PW_A, 0.4));
    all.extend(slot(2, src::LFO2, dst::CUTOFF, 0.6));
    all
}

/// The remaining shapes: a keyed unipolar square with a fade on the volume and a free unipolar
/// triangle on the pulse width of B and the mix.
fn uni_shapes() -> Vec<(&'static str, f64)> {
    let mut all = vec![
        ("lfo1_shape", 3.0),
        ("lfo1_rate", 4.0),
        ("lfo1_uni", 1.0),
        ("lfo1_fade", 0.05),
        ("lfo2_shape", 1.0),
        ("lfo2_key", 0.0),
        ("lfo2_uni", 1.0),
        ("lfo2_rate", 0.7),
    ];
    all.extend(slot(1, src::LFO1, dst::VOLUME, -0.5));
    all.extend(slot(2, src::LFO2, dst::PW_B, 0.5));
    all.extend(slot(3, src::LFO2, dst::MIX, 0.3));
    all
}

/// Factory presets that use their LFOs: vibrato with a fade (Mono Lead), a slow triangle (Wide
/// Pad), a synced free triangle (Wobble), a smooth random (Noise Wind).
const PRESETS: [&str; 4] = ["Mono Lead", "Wide Pad", "Wobble", "Noise Wind"];

#[test]
fn volt_renders_match_the_committed_digests() {
    let mut actual = vec![
        ("lfo-matrix".to_string(), render_digest(&lfo_matrix())),
        ("free-lfos".to_string(), render_digest(&free_lfos())),
        ("uni-shapes".to_string(), render_digest(&uni_shapes())),
    ];
    for name in PRESETS {
        actual.push((name.to_string(), render_digest(&preset(name))));
    }
    check_digests("volt", &actual, VOLT_DIGESTS);
}

/// The committed digests (Linux x86_64), from `e97f261c`, before #352. Every block split gives
/// the same bits, so there is one per render.
const VOLT_DIGESTS: &[(&str, u64)] = &[
    ("lfo-matrix", 0xa10986aa51579f52),
    ("free-lfos", 0x7251f41e999a64d9),
    ("uni-shapes", 0x925038e7975e0705),
    ("Mono Lead", 0x236b9e4d2906ec81),
    ("Wide Pad", 0xafa582a2c9d2812e),
    ("Wobble", 0x63f7608cdb6117d1),
    ("Noise Wind", 0x4df84a48418bfd75),
];
