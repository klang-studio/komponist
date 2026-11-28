//! The LFO behaviour of General-Sampler pinned before #362 moves sampler-core's LFO onto
//! `klang_synth::CyclicLfo`: render digests (FNV-1a over the output bits) of every LFO shape on
//! every target, committed for the reference platform (Linux x86_64); elsewhere they are printed
//! and not compared. General-Sampler's LFO runs free: its phase advances once per call
//! (`Engine::advance_lfo_phases`), so every block split has its own digest.

use assert_no_alloc::assert_no_alloc;
use komponist_model::params::ParamId;
use komponist_model::{Meter, Tick, TickRange};
use komponist_processor::{Event, EventKind, ProcessContext, Processor, StereoIo, TransportInfo};

use super::baseline_support::{Dice, Digest, SPLITS, Split, check_digests};
use super::{BLOCK, Fixture, RATE, TestSampler, note_off, note_on, ready};
use crate::{SPEC, Settings};

/// One second at [`RATE`].
const FRAMES: usize = RATE as usize;

/// A falling ramp (period 89 frames) with noise and a decay, one second long.
fn source() -> Vec<f32> {
    let mut dice = Dice(362);
    (0..FRAMES)
        .map(|i| {
            let ramp = 1.0 - 2.0 * (i % 89) as f32 / 89.0;
            let noise = 2.0 * dice.unit() - 1.0;
            let decay = 1.0 - i as f32 / FRAMES as f32;
            (0.5 * ramp + 0.2 * noise) * (0.3 + 0.7 * decay)
        })
        .collect()
}

fn param(name: &str, value: f64, offset: u32) -> Event {
    Event {
        offset,
        kind: EventKind::Param {
            id: SPEC.wire(&ParamId::new(name)).unwrap(),
            value,
        },
    }
}

/// Notes on and off the block grid (a retrigger of the first key, overlapping gates), a rate and
/// a shape change while voices sound, and a late note.
fn performance() -> Vec<(usize, Event)> {
    vec![
        (0, note_on(0, 60, 1)),
        (1_003, note_on(0, 67, 2)),
        (4_001, note_on(0, 55, 3)),
        (6_000, param("lfo_rate", 9.1, 0)),
        (7_777, note_on(0, 60, 4)),
        (8_500, note_off(0, 67, 2)),
        (9_001, param("lfo_shape", 5.0, 0)),
        (10_000, note_off(0, 60, 1)),
        (10_000, note_off(0, 55, 3)),
        (12_011, note_on(0, 72, 5)),
        (14_000, note_off(0, 60, 4)),
        (15_000, note_off(0, 72, 5)),
    ]
}

/// The parameter changes of one patch: an LFO shape, rate, target and depth, and what makes the
/// target audible.
fn patches() -> Vec<(&'static str, Vec<(&'static str, f64)>)> {
    let lfo = |shape: f64, rate: f64, target: f64, depth: f64| {
        vec![
            ("lfo_shape", shape),
            ("lfo_rate", rate),
            ("lfo_target", target),
            ("lfo_depth", depth),
            ("amp_release", 50.0),
        ]
    };
    let filtered = |mut changes: Vec<(&'static str, f64)>| {
        changes.extend([("filter", 1.0), ("cutoff", 900.0), ("resonance", 0.5)]);
        changes
    };
    vec![
        ("sine-pitch", lfo(0.0, 5.5, 1.0, 0.05)),
        ("triangle-cutoff", filtered(lfo(1.0, 0.37, 2.0, 0.6))),
        ("saw-up-start", lfo(2.0, 13.3, 3.0, 0.4)),
        ("saw-down-pitch", lfo(3.0, 30.0, 1.0, -0.03)),
        ("square-cutoff", filtered(lfo(4.0, 2.2, 2.0, -0.5))),
        ("sample-hold-pitch", lfo(5.0, 7.7, 1.0, 0.1)),
        ("sample-hold-cutoff-velocity", {
            let mut changes = filtered(lfo(5.0, 0.05, 2.0, 0.8));
            changes.extend([("velocity_target", 1.0), ("velocity_depth", 0.02)]);
            changes
        }),
    ]
}

/// Renders [`performance`] through a fresh General-Sampler with `changes` and `split` (each
/// call inside `assert_no_alloc`, also cut at the events) and hashes left and right.
fn render_digest(path: &std::path::Path, changes: &[(&str, f64)], split: Split) -> u64 {
    let mut sampler: TestSampler = ready(path, changes, &Settings::default(), 120.0);
    let plays = performance();
    let context = |frames: usize| ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: true,
        deadline: None,
    };
    let input = [0.0; BLOCK];
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    let mut events: Vec<Event> = Vec::with_capacity(plays.len());
    let mut digest = Digest::new();
    let mut sizes = split.sizes();
    let (mut pos, mut next, mut peak) = (0, 0, 0.0f32);
    while pos < FRAMES {
        events.clear();
        while next < plays.len() && plays[next].0 <= pos {
            events.push(plays[next].1);
            next += 1;
        }
        let until = plays.get(next).map_or(FRAMES, |p| p.0.min(FRAMES));
        let n = sizes().min(until - pos);
        let mut io = StereoIo {
            input: [&input[..n], &input[..n]],
            output: [&mut left[..n], &mut right[..n]],
        };
        let ctx = context(n);
        assert_no_alloc(|| sampler.processor.process(&ctx, &events, &mut io));
        digest.samples(&left[..n]);
        digest.samples(&right[..n]);
        peak = left[..n]
            .iter()
            .chain(&right[..n])
            .fold(peak, |m, s| m.max(s.abs()));
        pos += n;
    }
    assert!(peak > 0.01, "silent render");
    digest.0
}

#[test]
fn general_sampler_renders_match_the_committed_digests() {
    let fixture = Fixture::new("lfo-baseline", &source());
    let mut actual = Vec::new();
    for (name, changes) in patches() {
        for split in SPLITS {
            actual.push((
                format!("{name}/{}", split.name()),
                render_digest(&fixture.0, &changes, split),
            ));
        }
    }
    check_digests("General-Sampler", &actual, GENERAL_SAMPLER_DIGESTS);
}

/// The committed digests (Linux x86_64), from `01db1cd8`, before #362.
const GENERAL_SAMPLER_DIGESTS: &[(&str, u64)] = &[
    ("sine-pitch/split-1", 0xc50672ebf1e9f0dd),
    ("sine-pitch/split-31", 0x1d9804a1e26ca785),
    ("sine-pitch/split-32", 0x1b28961509dfc59d),
    ("sine-pitch/split-33", 0xcc17f24d00d11b3d),
    ("sine-pitch/split-64", 0x546a6acdbca9b0b1),
    ("sine-pitch/split-256", 0xcd8aa6057aa78725),
    ("sine-pitch/irregular-362-256", 0x0ea60588b2d83bb9),
    ("triangle-cutoff/split-1", 0x3b8f3dbe96943c85),
    ("triangle-cutoff/split-31", 0x1fb0c1b3df3d4875),
    ("triangle-cutoff/split-32", 0x490e5a4afb8db49d),
    ("triangle-cutoff/split-33", 0x72a92ef27495e4c1),
    ("triangle-cutoff/split-64", 0xa8479ea9252551fd),
    ("triangle-cutoff/split-256", 0x0f5ffa2ccc42c44d),
    ("triangle-cutoff/irregular-362-256", 0xef1db91c84375575),
    ("saw-up-start/split-1", 0x9d25b1f9f49eedf5),
    ("saw-up-start/split-31", 0x826fa5e4ee1e0db9),
    ("saw-up-start/split-32", 0xbdb92ac9f991c145),
    ("saw-up-start/split-33", 0x0fe213e3915e922d),
    ("saw-up-start/split-64", 0x2a18fbf58be12d35),
    ("saw-up-start/split-256", 0x7934986a1507bc1d),
    ("saw-up-start/irregular-362-256", 0xb0f827608046ad31),
    ("saw-down-pitch/split-1", 0x5ec280b5978bd459),
    ("saw-down-pitch/split-31", 0x3f25f6911338a769),
    ("saw-down-pitch/split-32", 0x31a974e9eed87a71),
    ("saw-down-pitch/split-33", 0x2260abc4269a2af5),
    ("saw-down-pitch/split-64", 0x5e2cca9921827531),
    ("saw-down-pitch/split-256", 0x78ecab56d44ffabd),
    ("saw-down-pitch/irregular-362-256", 0xcddd6a346a35bea9),
    ("square-cutoff/split-1", 0x16a9eed6a3080455),
    ("square-cutoff/split-31", 0x832910560d77df79),
    ("square-cutoff/split-32", 0x2c2931da6d741ec5),
    ("square-cutoff/split-33", 0x8b4c2d1b3cf6874d),
    ("square-cutoff/split-64", 0x526deb5c5ab223d5),
    ("square-cutoff/split-256", 0x84a2c804b21393e9),
    ("square-cutoff/irregular-362-256", 0xb0b26a96a080c1e9),
    ("sample-hold-pitch/split-1", 0xad44b745fdd4feb9),
    ("sample-hold-pitch/split-31", 0xf5a111ec2e7a6991),
    ("sample-hold-pitch/split-32", 0x96c323f59ff6c0ed),
    ("sample-hold-pitch/split-33", 0x1209ad57418412f5),
    ("sample-hold-pitch/split-64", 0xaf5c5f02e9ef362d),
    ("sample-hold-pitch/split-256", 0xa2a1f514ab1e1b71),
    ("sample-hold-pitch/irregular-362-256", 0x34304170ed852231),
    ("sample-hold-cutoff-velocity/split-1", 0xea364d011f436d61),
    ("sample-hold-cutoff-velocity/split-31", 0x7e2a127d3ef3f5cd),
    ("sample-hold-cutoff-velocity/split-32", 0x9057a49ff79cd80d),
    ("sample-hold-cutoff-velocity/split-33", 0x28fa6999996d764d),
    ("sample-hold-cutoff-velocity/split-64", 0x53315a19e1b78d11),
    ("sample-hold-cutoff-velocity/split-256", 0x0eb922fc876cfe29),
    (
        "sample-hold-cutoff-velocity/irregular-362-256",
        0x9beda88decb343fd,
    ),
];
