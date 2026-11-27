//! The LFO behaviour of Hexa pinned before #352 moved it into `klang_synth::CyclicLfo`: an
//! FNV-1a hash over the output bits (and the LFO positions) of representative [`Synth`] renders,
//! committed for the reference platform (Linux x86_64); elsewhere the digests are printed and not
//! compared. (#360 also passed a differential oracle against a verbatim copy of the old `Lfos`
//! before removing it.)
//!
//! The renders cover the block splits 1, 31, 32, 33, 64, 256 and irregular splits from a fixed
//! seed, because Hexa advances its LFOs once per block.

use crate::lfo::{LfoParams, LfoWaveform};
use crate::modulation::{ModRoute, ModSource, ModTarget};
use crate::{Patch, Synth};

const SR: f32 = 48_000.0;

// ------------------------------------------------------------------------------------------------
// Shared helpers

/// A small deterministic generator for the test scripts (splitmix64), independent of the RNG the
/// LFOs use.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `1..=max`.
    fn size(&mut self, max: usize) -> usize {
        1 + (self.next() % max as u64) as usize
    }
}

/// How the samples are cut into calls (Hexa: into blocks).
#[derive(Clone, Copy, Debug)]
enum Split {
    Fixed(usize),
    /// Sizes 1..=`max` from a fixed seed.
    Irregular {
        seed: u64,
        max: usize,
    },
}

impl Split {
    fn name(self) -> String {
        match self {
            Split::Fixed(n) => format!("split-{n}"),
            Split::Irregular { seed, max } => format!("irregular-{seed}-{max}"),
        }
    }

    fn sizes(self) -> impl FnMut() -> usize {
        let mut dice = Dice(match self {
            Split::Fixed(_) => 0,
            Split::Irregular { seed, .. } => seed,
        });
        move || match self {
            Split::Fixed(n) => n,
            Split::Irregular { max, .. } => dice.size(max),
        }
    }
}

/// The block splits of #358.
const SPLITS: [Split; 7] = [
    Split::Fixed(1),
    Split::Fixed(31),
    Split::Fixed(32),
    Split::Fixed(33),
    Split::Fixed(64),
    Split::Fixed(256),
    Split::Irregular {
        seed: 358,
        max: 300,
    },
];

/// FNV-1a (64 bit) over the little-endian bytes of `u32` words.
struct Digest(u64);

impl Digest {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn word(&mut self, word: u32) {
        for byte in word.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn samples(&mut self, samples: &[f32]) {
        samples.iter().for_each(|s| self.word(s.to_bits()));
    }
}

/// Compares `actual` with the committed `expected` digests on the reference platform (Linux
/// x86_64); elsewhere prints them. On a mismatch the message holds the table to commit if the
/// change is intended.
fn check_digests(actual: &[(String, u64)], expected: &[(&str, u64)]) {
    let table: String = actual
        .iter()
        .map(|(name, digest)| format!("    (\"{name}\", {digest:#018x}),\n"))
        .collect();
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!(
            "Hexa LFO render digests, not compared (the reference platform is Linux x86_64):\n\
             {table}"
        );
        return;
    }
    // Every name once on both sides, so each committed entry is checked exactly once.
    let unique = |names: Vec<&str>| {
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate digest names: {names:?}"
        );
    };
    unique(actual.iter().map(|(name, _)| name.as_str()).collect());
    unique(expected.iter().map(|(name, _)| *name).collect());
    let differ: Vec<&str> = actual
        .iter()
        .filter(|(name, digest)| !expected.iter().any(|(n, d)| n == name && d == digest))
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        differ.is_empty() && actual.len() == expected.len(),
        "Hexa render digests differ from the committed reference: {differ:?}.\n\
         Only if the change is intended, commit this table:\n{table}"
    );
}

// ------------------------------------------------------------------------------------------------
// LFO parameters and routes

fn lfo(waveform: LfoWaveform, rate: f32) -> LfoParams {
    LfoParams {
        waveform,
        rate,
        ..LfoParams::default()
    }
}

/// Eight LFOs: every shape, the extreme rates, delays, fade-ins, start phases, amounts and both
/// key-sync modes.
fn all_shapes() -> Vec<LfoParams> {
    use LfoWaveform::*;
    vec![
        LfoParams {
            phase: 0.1,
            ..lfo(Sine, 5.0)
        },
        LfoParams {
            phase: 0.5,
            key_sync: false,
            ..lfo(Triangle, 0.37)
        },
        LfoParams {
            delay: 0.05,
            fade_in: 0.2,
            ..lfo(SawUp, 50.0)
        },
        LfoParams {
            key_sync: false,
            amount: 0.6,
            ..lfo(SawDown, 13.3)
        },
        LfoParams {
            fade_in: 0.5,
            amount: 0.7,
            ..lfo(Square, 2.2)
        },
        lfo(SampleHold, 7.7),
        LfoParams {
            key_sync: false,
            phase: 0.9,
            ..lfo(SampleHold, 0.01)
        },
        LfoParams {
            delay: 0.3,
            fade_in: 1.5,
            phase: 0.75,
            ..lfo(Sine, 31.0)
        },
    ]
}

fn route(source: ModSource, target: ModTarget, depth: f32, via: ModSource) -> ModRoute {
    ModRoute {
        source,
        target,
        depth,
        via,
    }
}

// ------------------------------------------------------------------------------------------------
// Render digests

/// A performance event for [`Synth`], at a sample position.
#[derive(Clone, Copy, Debug)]
enum Play {
    On(u8, u8),
    Off(u8),
    Midi([u8; 3]),
    /// A new patch (`Engine::set_patch`; LFOs it adds start on the sounding voices).
    Patch(fn() -> Patch),
}

/// One second: a note, a second one off the 32-sample grid, a third, a retrigger of the first,
/// the mod wheel and pressure, releases and a late note.
const PERFORMANCE: &[(usize, Play)] = &[
    (0, Play::On(60, 100)),
    (1000, Play::On(64, 80)),
    (5003, Play::On(67, 120)),
    (9001, Play::On(60, 90)),
    (12_000, Play::Midi([0xb0, 1, 100])),
    (14_017, Play::Midi([0xd0, 90, 0])),
    (20_000, Play::Off(64)),
    (26_000, Play::Off(60)),
    (26_000, Play::Off(67)),
    (30_011, Play::On(72, 70)),
    (40_000, Play::Off(72)),
];
const FRAMES: usize = 48_000;

/// Renders `patch` through `Synth<N>` with `split`, the calls also cut at the events (as a host
/// processor does), and hashes the output and the LFO positions after every call. Notes, MIDI
/// and renders run inside `assert_no_alloc`, as on the audio thread.
fn render_digest<const N: usize>(patch: Patch, plays: &[(usize, Play)], split: Split) -> u64 {
    use assert_no_alloc::assert_no_alloc;
    let mut synth = Synth::<N>::boxed(SR, patch);
    let mut digest = Digest::new();
    let mut sizes = split.sizes();
    let (mut l, mut r) = ([0.0; 512], [0.0; 512]);
    let (mut pos, mut next, mut peak) = (0, 0, 0.0f32);
    while pos < FRAMES {
        while next < plays.len() && plays[next].0 <= pos {
            match plays[next].1 {
                Play::On(key, velocity) => assert_no_alloc(|| synth.note_on(key, velocity)),
                Play::Off(key) => assert_no_alloc(|| synth.note_off(key)),
                Play::Midi(bytes) => assert_no_alloc(|| synth.midi(&bytes)),
                Play::Patch(build) => synth.set_patch(build()),
            }
            next += 1;
        }
        let until = plays.get(next).map_or(FRAMES, |p| p.0.min(FRAMES));
        let n = sizes().min(until - pos);
        assert_no_alloc(|| synth.render(&mut l[..n], &mut r[..n]));
        digest.samples(&l[..n]);
        digest.samples(&r[..n]);
        peak = l[..n]
            .iter()
            .chain(&r[..n])
            .fold(peak, |m, s| m.max(s.abs()));
        match synth.lfo_positions() {
            Some(p) => digest.samples(&p),
            None => digest.word(u32::MAX),
        }
        pos += n;
    }
    assert!(peak > 0.01, "silent render");
    digest.0
}

/// Every shape on eight LFOs, each routed somewhere audible, with their rate, amount and fade-in
/// modulated by velocity, the mod wheel and pressure.
fn lfo_heavy() -> Patch {
    let mut patch = Patch::default();
    for op in &mut patch.operators {
        op.envelope.sustain = 1.0;
        op.envelope.release = 0.2;
    }
    for p in all_shapes() {
        patch.lfos.push(p);
    }
    let none = ModSource::None;
    for r in [
        route(ModSource::Lfo(0), ModTarget::OpPitch(0), 0.3, none),
        route(ModSource::Lfo(1), ModTarget::OpLevel(1), 0.4, none),
        route(ModSource::Lfo(2), ModTarget::Tune, 0.2, none),
        route(ModSource::Lfo(3), ModTarget::Balance, 0.7, none),
        route(ModSource::Lfo(4), ModTarget::Volume, 0.3, none),
        route(ModSource::Lfo(5), ModTarget::OpPitch(1), 3.0, none),
        route(ModSource::Lfo(6), ModTarget::OpFeedback(1), 0.5, none),
        route(
            ModSource::Lfo(7),
            ModTarget::OpPan(0),
            0.5,
            ModSource::ModWheel,
        ),
        route(ModSource::Velocity, ModTarget::LfoRate(0), 2.0, none),
        route(ModSource::Velocity, ModTarget::LfoRate(5), 1.0, none),
        route(ModSource::ModWheel, ModTarget::LfoAmount(4), -0.5, none),
        route(ModSource::Aftertouch, ModTarget::LfoFade(7), 0.3, none),
    ] {
        patch.modulation.push(r);
    }
    patch
}

/// [`lfo_heavy`] with its first two LFOs only (and their routes).
fn two_lfos() -> Patch {
    let mut patch = lfo_heavy();
    let full = patch;
    patch.lfos = Default::default();
    patch.modulation = Default::default();
    for p in &full.lfos.as_slice()[..2] {
        patch.lfos.push(*p);
    }
    for r in full.modulation.as_slice() {
        if matches!(r.source, ModSource::Lfo(l) if l < 2) {
            patch.modulation.push(*r);
        }
    }
    patch
}

/// Factory presets that lean on their LFOs: sample-and-hold (key-synced with its rate modulated,
/// four free-running ones), delay and fade-in, amount modulation, a 50 Hz square, a modulated
/// falling saw.
const PRESETS: [&str; 6] = [
    "Random Filter",
    "Star Field",
    "Detroit Strings",
    "Clap",
    "Neuro Bass",
    "Gameboy Lead",
];

fn preset_patch(name: &str) -> Patch {
    let index = (0..crate::PRESET_COUNT)
        .find(|&i| crate::preset_name(i) == Some(name))
        .unwrap_or_else(|| panic!("no preset {name}"));
    crate::preset(index).unwrap().patch
}

#[test]
fn hexa_renders_match_the_committed_digests() {
    let mut actual = Vec::new();
    let mut add = |name: String, digest: u64| actual.push((name, digest));
    let mut added = PERFORMANCE.to_vec();
    added.push((7000, Play::Patch(lfo_heavy)));
    // `render_digest` plays the events in list order: keep them sorted by position.
    added.sort_by_key(|(at, _)| *at);
    for split in SPLITS {
        let s = split.name();
        add(
            format!("lfo-heavy/{s}"),
            render_digest::<1>(lfo_heavy(), PERFORMANCE, split),
        );
        add(
            format!("lfo-added/{s}"),
            render_digest::<1>(two_lfos(), &added, split),
        );
        // Two engines: the second one's sample-and-hold runs from its own seed.
        add(
            format!("lfo-heavy-2-engines/{s}"),
            render_digest::<2>(lfo_heavy(), PERFORMANCE, split),
        );
        for name in PRESETS {
            add(
                format!("{name}/{s}"),
                render_digest::<1>(preset_patch(name), PERFORMANCE, split),
            );
        }
    }
    check_digests(&actual, HEXA_DIGESTS);
}

/// The committed digests (Linux x86_64), from `e97f261c`, before #352.
const HEXA_DIGESTS: &[(&str, u64)] = &[
    ("lfo-heavy/split-1", 0xb553e893d707ba09),
    ("lfo-added/split-1", 0x019aa0ee64114cac),
    ("lfo-heavy-2-engines/split-1", 0xcb67e10b9158844a),
    ("Random Filter/split-1", 0x6ccb86fe40f42d7b),
    ("Star Field/split-1", 0x4e436ea91730df46),
    ("Detroit Strings/split-1", 0xbfc52bf5385ba187),
    ("Clap/split-1", 0xdc6863eef9ccb1bf),
    ("Neuro Bass/split-1", 0x264554711c5836dd),
    ("Gameboy Lead/split-1", 0xd751b31474d0d95f),
    ("lfo-heavy/split-31", 0xfde261eb67687af5),
    ("lfo-added/split-31", 0x2a23b808cf781397),
    ("lfo-heavy-2-engines/split-31", 0x0d33e7e4fa636a92),
    ("Random Filter/split-31", 0x8e192df0cedf3667),
    ("Star Field/split-31", 0x7394f1f536f58408),
    ("Detroit Strings/split-31", 0xb78441836cb8ecb7),
    ("Clap/split-31", 0x58827262d788641e),
    ("Neuro Bass/split-31", 0x8f1444284c71e1d3),
    ("Gameboy Lead/split-31", 0x290be377b29f14a8),
    ("lfo-heavy/split-32", 0xc27c56bb99d331f2),
    ("lfo-added/split-32", 0xad5924390ef07312),
    ("lfo-heavy-2-engines/split-32", 0xfc4e492c310e4f56),
    ("Random Filter/split-32", 0x1595e537ce0ed795),
    ("Star Field/split-32", 0x86d08dce0fc99acc),
    ("Detroit Strings/split-32", 0x003190626a264e9d),
    ("Clap/split-32", 0xf7e3c27e02562cce),
    ("Neuro Bass/split-32", 0x993cef8fab46a616),
    ("Gameboy Lead/split-32", 0x7c8d439f15f0a30c),
    ("lfo-heavy/split-33", 0x89486cf1b470b7a7),
    ("lfo-added/split-33", 0x6a5965da9794d255),
    ("lfo-heavy-2-engines/split-33", 0xcc91620a849223e2),
    ("Random Filter/split-33", 0x8e1c962c8c56428a),
    ("Star Field/split-33", 0x51e38420c01b48bf),
    ("Detroit Strings/split-33", 0xbb2687a79a250649),
    ("Clap/split-33", 0x127bc35229f2dc3f),
    ("Neuro Bass/split-33", 0xb5b91204317a222b),
    ("Gameboy Lead/split-33", 0x359c15da00198a2e),
    ("lfo-heavy/split-64", 0xd3726f9f54da96a0),
    ("lfo-added/split-64", 0x011a180ead75b6a9),
    ("lfo-heavy-2-engines/split-64", 0x8f8444a1ec6dae75),
    ("Random Filter/split-64", 0x7c3239272a89c3f8),
    ("Star Field/split-64", 0x6a34a45c16393778),
    ("Detroit Strings/split-64", 0x1c361244a271004d),
    ("Clap/split-64", 0x86b9bd62cf1264bf),
    ("Neuro Bass/split-64", 0x9b6213df18cd8172),
    ("Gameboy Lead/split-64", 0x2848e1e6df9765d1),
    ("lfo-heavy/split-256", 0x3dfaef5decfbda25),
    ("lfo-added/split-256", 0x666eafbd14e61c34),
    ("lfo-heavy-2-engines/split-256", 0x365dfad429e84fe2),
    ("Random Filter/split-256", 0xf4e02b2c724520c2),
    ("Star Field/split-256", 0x5b523fe23cf39034),
    ("Detroit Strings/split-256", 0x11b284fbed7cc451),
    ("Clap/split-256", 0xf6887b55e7feff25),
    ("Neuro Bass/split-256", 0x5c3f1f1d5e9709f7),
    ("Gameboy Lead/split-256", 0x931686f9122bb1ad),
    ("lfo-heavy/irregular-358-300", 0xc5aa6f2db28fa52b),
    ("lfo-added/irregular-358-300", 0x9bd0ead60bb7c2fc),
    ("lfo-heavy-2-engines/irregular-358-300", 0x61db69e549ece9eb),
    ("Random Filter/irregular-358-300", 0x006d6be35f80df53),
    ("Star Field/irregular-358-300", 0xf8f079fb80d110db),
    ("Detroit Strings/irregular-358-300", 0xb55fb39d0a4840f3),
    ("Clap/irregular-358-300", 0x99aacb19d24c142f),
    ("Neuro Bass/irregular-358-300", 0x43115a1aa626a167),
    ("Gameboy Lead/irregular-358-300", 0x2646f112c9af6fdc),
];
