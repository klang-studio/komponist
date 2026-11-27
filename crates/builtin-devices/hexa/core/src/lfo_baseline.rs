//! The LFO behaviour of Hexa pinned before #352 moves it into a shared component, in two layers:
//!
//! - **Differential oracle:** the production [`Lfos`] and the verbatim copy in
//!   [`crate::lfo_reference`] run the same scripts in lockstep; every output of `advance_voice`
//!   and every `position` must have the same bits. Runs on every platform.
//! - **Render digests:** an FNV-1a hash over the output bits (and the LFO positions) of
//!   representative [`Synth`] renders, committed for the reference platform (Linux x86_64);
//!   elsewhere the digests are printed and not compared.
//!
//! Both layers cover the block splits 1, 31, 32, 33, 64, 256 and irregular splits from a fixed
//! seed, because Hexa advances its LFOs once per block.

use crate::lfo::{LfoParams, LfoWaveform, Lfos, MAX_LFOS};
use crate::lfo_reference;
use crate::modulation::{ModRoute, ModSource, ModTarget, Modulated, Routes, SourceValues};
use crate::{MAX_VOICES, Patch, Synth};

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

    /// Uniform in 0..1.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
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
// Differential oracle

/// Something that happens before a block.
#[derive(Clone, Debug)]
enum Event {
    /// Note-on (or retrigger) on a voice; it sounds from now on.
    NoteOn(usize),
    /// The voice stops sounding (no more advancing), as when its envelopes end.
    Silence(usize),
    Seed(u32),
    /// New LFO parameters; LFOs beyond the old count start on every sounding voice, as
    /// `Engine::set_patch` does.
    Params(Vec<LfoParams>),
}

/// An LFO script: parameters, routes on the LFOs' own parameters, events by block.
struct Script {
    name: &'static str,
    params: Vec<LfoParams>,
    routes: Vec<ModRoute>,
    used: u8,
    /// Seconds of audio.
    seconds: f32,
    /// Events at fractions of the length.
    events: Vec<(f32, Event)>,
}

/// Runs `script` with `split` through the production `Lfos` and the reference in lockstep and
/// compares every output bit for bit. Returns how many values were compared.
fn lockstep(script: &Script, split: Split) -> usize {
    let mut prod = Lfos::new();
    let mut refr = lfo_reference::Lfos::new();
    let mut params = script.params.clone();
    let routes = Routes::new(&script.routes);
    let slots = *routes.slots();
    let modulated = Modulated::new(&script.routes).lfos();
    let mut sounding = [false; MAX_VOICES];
    let mut dice = Dice(0x5eed);
    let mut sizes = split.sizes();
    let total = (script.seconds * SR) as usize;
    let mut events: Vec<(usize, Event)> = script
        .events
        .iter()
        .map(|(at, e)| ((at * total as f32) as usize, e.clone()))
        .collect();
    events.sort_by_key(|(at, _)| *at);
    let mut next = 0;
    let mut pos = 0;
    let mut block = 0;
    let mut compared = 0;
    let context = |block: usize, pos: usize| {
        format!(
            "script {}, {}, block {block} (sample {pos})",
            script.name,
            split.name()
        )
    };
    let mut same = |a: f32, b: f32, what: &dyn Fn() -> String| {
        assert_eq!(a.to_bits(), b.to_bits(), "{}: {a} vs {b}", what());
        compared += 1;
    };
    while pos < total {
        while next < events.len() && events[next].0 <= pos {
            match &events[next].1 {
                Event::NoteOn(v) => {
                    prod.note_on(*v, &params);
                    refr.note_on(*v, &params);
                    sounding[*v] = true;
                }
                Event::Silence(v) => sounding[*v] = false,
                Event::Seed(seed) => {
                    prod.seed(*seed);
                    refr.seed(*seed);
                }
                Event::Params(new) => {
                    for v in (0..MAX_VOICES).filter(|&v| sounding[v]) {
                        for (l, p) in new.iter().enumerate().skip(params.len()) {
                            prod.start(l, v, p);
                            refr.start(l, v, p);
                        }
                    }
                    params = new.clone();
                }
            }
            next += 1;
        }
        let n = sizes().min(total - pos);
        let seconds = n as f32 / SR;
        prod.advance_global(&params, seconds);
        refr.advance_global(&params, seconds);
        for v in (0..MAX_VOICES).filter(|&v| sounding[v]) {
            let mut values = || SourceValues {
                velocity: dice.unit(),
                aftertouch: dice.unit(),
                mod_wheel: dice.unit(),
                pitch_bend: 2.0 * dice.unit() - 1.0,
                ..SourceValues::ZERO
            };
            let (v0, v1) = (values(), values());
            let o0 = routes.lfo_offsets(&slots, &v0);
            let o1 = routes.lfo_offsets(&slots, &v1);
            let a = prod.advance_voice(v, &params, script.used, modulated, (&o0, &o1), seconds);
            let b = refr.advance_voice(v, &params, script.used, modulated, (&o0, &o1), seconds);
            for l in 0..MAX_LFOS {
                same(a.0[l], b.0[l], &|| {
                    format!("{}: voice {v} LFO {l} start", context(block, pos))
                });
                same(a.1[l], b.1[l], &|| {
                    format!("{}: voice {v} LFO {l} end", context(block, pos))
                });
            }
        }
        for v in (0..MAX_VOICES).filter(|&v| sounding[v]) {
            for l in 0..params.len() {
                let (a, b) = (prod.position(l, v), refr.position(l, v));
                for k in 0..3 {
                    same(a[k], b[k], &|| {
                        format!("{}: voice {v} LFO {l} position[{k}]", context(block, pos))
                    });
                }
            }
        }
        pos += n;
        block += 1;
    }
    compared
}

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

fn scripts() -> Vec<Script> {
    use LfoWaveform::*;
    let notes = vec![
        (0.0, Event::NoteOn(0)),
        (0.15, Event::NoteOn(3)),
        (0.3, Event::Seed(0)),
        (0.35, Event::NoteOn(0)), // retrigger
        (0.45, Event::NoteOn(7)),
        (0.55, Event::Silence(3)),
        (0.6, Event::Seed(0xdead_beef)),
        (0.7, Event::NoteOn(3)),
        (0.85, Event::NoteOn(15)),
    ];
    let mut later = notes.clone();
    // The free-running phases run alone for a while before the first note.
    later[0].0 = 0.1;
    vec![
        Script {
            name: "all shapes",
            params: all_shapes(),
            routes: vec![],
            used: 0xff,
            seconds: 1.5,
            events: later,
        },
        Script {
            name: "some used",
            params: all_shapes(),
            routes: vec![],
            used: 0b1010_0110,
            seconds: 0.5,
            events: notes.clone(),
        },
        Script {
            name: "modulated",
            params: vec![
                lfo(Sine, 3.0),
                LfoParams {
                    key_sync: false,
                    ..lfo(SampleHold, 9.0)
                },
                LfoParams {
                    fade_in: 0.4,
                    ..lfo(Triangle, 1.0)
                },
                LfoParams {
                    fade_in: 0.0,
                    delay: 0.1,
                    amount: 0.3,
                    ..lfo(Square, 4.0)
                },
                lfo(SawDown, 0.5),
                LfoParams {
                    amount: 0.0,
                    ..lfo(SampleHold, 50.0)
                },
            ],
            routes: vec![
                route(
                    ModSource::Velocity,
                    ModTarget::LfoRate(0),
                    3.0,
                    ModSource::None,
                ),
                route(
                    ModSource::ModWheel,
                    ModTarget::LfoRate(1),
                    -2.0,
                    ModSource::None,
                ),
                route(
                    ModSource::PitchBend,
                    ModTarget::LfoRate(4),
                    1.5,
                    ModSource::None,
                ),
                // The deepest rate modulation: many periods per block at long blocks.
                route(
                    ModSource::Velocity,
                    ModTarget::LfoRate(5),
                    8.0,
                    ModSource::None,
                ),
                route(
                    ModSource::Aftertouch,
                    ModTarget::LfoAmount(0),
                    -0.7,
                    ModSource::None,
                ),
                route(
                    ModSource::Velocity,
                    ModTarget::LfoAmount(3),
                    0.5,
                    ModSource::ModWheel,
                ),
                route(
                    ModSource::ModWheel,
                    ModTarget::LfoAmount(5),
                    1.0,
                    ModSource::None,
                ),
                route(
                    ModSource::ModWheel,
                    ModTarget::LfoFade(2),
                    0.5,
                    ModSource::None,
                ),
                route(
                    ModSource::Velocity,
                    ModTarget::LfoFade(3),
                    0.3,
                    ModSource::None,
                ),
                route(
                    ModSource::Aftertouch,
                    ModTarget::LfoFade(4),
                    -0.4,
                    ModSource::Velocity,
                ),
            ],
            used: 0b11_1111,
            seconds: 1.0,
            events: notes.clone(),
        },
        Script {
            name: "added while sounding",
            params: all_shapes()[..2].to_vec(),
            routes: vec![],
            used: 0xff,
            seconds: 1.0,
            events: {
                let mut e = notes.clone();
                e.push((0.25, Event::Params(all_shapes()[..5].to_vec())));
                // New rates and shapes for the same LFOs (an edited patch).
                let mut edited = all_shapes();
                edited[0].rate = 17.0;
                edited[1].waveform = SampleHold;
                edited[2].key_sync = false;
                e.push((0.5, Event::Params(edited[..5].to_vec())));
                e.push((0.65, Event::Params(all_shapes())));
                e
            },
        },
    ]
}

#[test]
fn hexa_lfos_match_the_reference_bit_for_bit() {
    for script in scripts() {
        for split in SPLITS {
            let compared = lockstep(&script, split);
            assert!(
                compared > 1000,
                "{} {}: {compared}",
                script.name,
                split.name()
            );
        }
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
