//! CPU cost per block, measured in release builds:
//! `cargo test --release -p komponist-builtin --test pisynth bench -- --ignored --nocapture`.

use std::hint::black_box;
use std::time::Instant;

use komponist_builtin::{Macro, PisynthProcessor, preset_patch, presets};
use komponist_engine::{Event, EventKind, Processor};

use crate::common::*;

/// Average microseconds per call of `block` frames over 4 s of a sustained chord of `keys` on
/// preset `index`, with `per_block` events at offset 0 of every call.
fn per_block(index: usize, keys: &[u8], block: usize, per_block: &[EventKind]) -> f64 {
    let mut p = activated(preset_patch(index).unwrap());
    let chord: Vec<_> = keys.iter().map(|&k| (0, note_on(k))).collect();
    let _ = play(&mut p, block, block, &chord);
    let calls = frames(4.0) / block;
    let events: Vec<Event> = per_block
        .iter()
        .map(|&kind| Event { offset: 0, kind })
        .collect();
    let mut out = Out::default();
    let started = Instant::now();
    for _ in 0..calls {
        out.l.clear();
        out.r.clear();
        call(&mut p, block, &events, &mut out);
        black_box(&out);
    }
    started.elapsed().as_secs_f64() * 1e6 / calls as f64
}

#[test]
#[ignore]
fn cpu_per_block() {
    let chord = [48, 52, 55, 59, 62, 64, 67, 71];
    let block_ms = |block: usize| block as f64 / f64::from(RATE) * 1000.0;
    for name in ["Init", "E-Piano", "Strings", "Supersaw"] {
        let index = presets().find(|p| p.name == name).unwrap().index;
        for block in [128, 256, 512] {
            let us = per_block(index, &chord, block, &[]);
            println!(
                "{name:10} 8 keys, block {block:4}: {us:7.1} µs = {:4.1} % of {:.2} ms",
                us / 10.0 / block_ms(block),
                block_ms(block)
            );
        }
    }
    let index = presets().find(|p| p.name == "E-Piano").unwrap().index;
    let base = per_block(index, &chord, 256, &[]);
    let macro_change = per_block(index, &chord, 256, &[param(Macro::Index, 0.3)]);
    println!(
        "E-Piano block 256 with one macro change per block: {:+.2} µs",
        macro_change - base
    );
    // The macro change alone: silent synth, 32-frame calls, alternating values.
    let silent = |events: &[EventKind]| {
        let mut p = activated(preset_patch(index).unwrap());
        let mut out = Out::default();
        let started = Instant::now();
        for i in 0..20_000 {
            out.l.clear();
            out.r.clear();
            let list: Vec<Event> = events
                .iter()
                .map(|&kind| match kind {
                    EventKind::Param { id, .. } => Event {
                        offset: 0,
                        kind: EventKind::Param {
                            id,
                            value: 0.1 + 0.1 * f64::from(i % 2),
                        },
                    },
                    kind => Event { offset: 0, kind },
                })
                .collect();
            call(&mut p, 32, &list, &mut out);
        }
        started.elapsed().as_secs_f64() * 1e6 / 20_000.0
    };
    let idle = silent(&[]);
    let one = silent(&[param(Macro::Index, 0.0)]);
    let algorithm = silent(&[param(Macro::Algorithm, 0.0), param(Macro::Release, 0.0)]);
    println!(
        "macro change (copy + set + set_patch): {:.2} µs; Algorithmus + Release: {:.2} µs",
        one - idle,
        algorithm - idle
    );

    let mut p = activated(preset_patch(index).unwrap());
    let started = Instant::now();
    for _ in 0..200 {
        p.reset();
    }
    println!(
        "reset: {:.1} µs",
        started.elapsed().as_secs_f64() * 1e6 / 200.0
    );
    let started = Instant::now();
    for _ in 0..20 {
        black_box(PisynthProcessor::new(preset_patch(index).unwrap()));
    }
    println!(
        "new (UI thread): {:.1} µs",
        started.elapsed().as_secs_f64() * 1e6 / 20.0
    );
}

/// The first calls of a fresh processor (WP-61 soak: a rare overrun after undo/redo inserted a
/// new pisynth): `new` + `activate` on the UI thread, then the first and later 256-frame calls
/// with a chord, each preset.
#[test]
#[ignore]
fn first_calls_of_a_new_processor() {
    let chord = [48, 52, 55, 59];
    let events: Vec<Event> = chord
        .iter()
        .map(|&k| Event {
            offset: 0,
            kind: note_on(k),
        })
        .collect();
    for info in presets().take(8) {
        let t0 = Instant::now();
        let mut p = activated(preset_patch(info.index).unwrap());
        let made = t0.elapsed();
        let mut out = Out::default();
        out.l.reserve(256 * 8);
        out.r.reserve(256 * 8);
        let mut times = Vec::new();
        for i in 0..6 {
            let t = Instant::now();
            call(&mut p, 256, if i == 0 { &events } else { &[] }, &mut out);
            times.push(t.elapsed());
        }
        println!(
            "{:10} new+activate {made:>9.1?}, calls {times:.1?}",
            info.name
        );
    }
}
