//! CPU cost, measured in release builds:
//! `cargo test --release -p komponist-builtin --lib knurr::bench -- --ignored --nocapture`.
//! The numbers go into `docs/PERFORMANCE.md` (§8).

use std::hint::black_box;
use std::time::Instant;

use super::tests::{RATE, on, processor, render, wire};
use komponist_engine::EventKind;

/// Microseconds per call of `block` frames, averaged over about 2 s of `keys` held with the
/// parameters changed as given.
fn per_block(changes: &[(&str, f64)], keys: &[u8], block: usize, events: &[EventKind]) -> f64 {
    let mut p = processor(changes);
    let held: Vec<_> = keys.iter().map(|&k| (0, on(k, 100))).collect();
    // Let the attacks pass and the filter settle.
    render(&mut p, 9600, &held, 120.0);
    let calls = (2.0 * RATE) as usize / block;
    let list: Vec<_> = events.iter().map(|&e| (0, e)).collect();
    let started = Instant::now();
    for _ in 0..calls {
        black_box(render(&mut p, block, &list, 120.0));
    }
    started.elapsed().as_secs_f64() * 1e6 / calls as f64
}

/// The cost of `voices` keys in µs per call, minus the cost of the same call with no key held
/// (the silent call and the render's own buffer handling), divided by the keys.
fn per_voice(changes: &[(&str, f64)], voices: usize, block: usize) -> (f64, f64) {
    let keys: Vec<u8> = (0..voices).map(|i| 36 + 5 * i as u8).collect();
    let full = per_block(changes, &keys, block, &[]);
    let empty = per_block(changes, &[], block, &[]);
    let per_key = (full - empty) / voices as f64;
    // ns per key per sample, and the share of one core at 48 kHz.
    let ns = per_key * 1000.0 / block as f64;
    (ns, ns * 1e-9 * f64::from(RATE) * 100.0)
}

#[test]
#[ignore]
fn cpu_per_voice() {
    let wide = |u: f64| [("unison", u), ("spread", 0.6), ("detune", 14.0)];
    let cases: [(&str, Vec<(&str, f64)>); 7] = [
        ("unison 1, LP 24", vec![("unison", 1.0)]),
        (
            "unison 1, LP 12",
            vec![("unison", 1.0), ("filter_type", 1.0)],
        ),
        ("unison 3 + spread, LP 24", wide(3.0).to_vec()),
        ("unison 5 + spread, LP 24", wide(5.0).to_vec()),
        ("unison 7 + spread, LP 24", wide(7.0).to_vec()),
        (
            "unison 1, matrix 4 slots, 2 LFOs, Analog 100 %",
            vec![
                ("unison", 1.0),
                ("analog", 1.0),
                ("mod1_src", 1.0),
                ("mod1_dst", 6.0),
                ("mod1_amt", 0.5),
                ("mod2_src", 2.0),
                ("mod2_dst", 3.0),
                ("mod2_amt", 0.5),
                ("mod3_src", 3.0),
                ("mod3_dst", 1.0),
                ("mod3_amt", 0.1),
                ("mod4_src", 8.0),
                ("mod4_dst", 5.0),
                ("mod4_amt", 0.5),
            ],
        ),
        ("worst: unison 7 + spread, matrix, Analog", {
            let mut v = wide(7.0).to_vec();
            v.extend([
                ("analog", 1.0),
                ("mod1_src", 1.0),
                ("mod1_dst", 6.0),
                ("mod1_amt", 0.5),
            ]);
            v
        }),
    ];
    let _ = wire("cutoff");
    for (name, changes) in &cases {
        // Unison 7 leaves six voices (the cap of 48 oscillator groups): measure what sounds.
        let sounding = if name.contains("unison 7") { 6 } else { 8 };
        let (ns, share) = per_voice(changes, sounding, 256);
        println!(
            "{name:48} block 256: {ns:7.0} ns per key and sample = {share:5.2} % of a core per key"
        );
    }
    // 8 keys chord of the whole device, the way the proposal counts.
    for (name, changes) in &cases[..5] {
        // The cap of 48 oscillator groups: unison 7 plays six keys, the rest eight.
        let all = [36u8, 40, 43, 47, 50, 52, 55, 59];
        let keys = &all[..if name.contains("unison 7") { 6 } else { 8 }];
        let us = per_block(changes, keys, 256, &[]);
        println!(
            "{name:48} {} keys, 256 frames: {us:7.1} µs = {:4.1} % of 5.33 ms",
            keys.len(),
            us / 53.3
        );
    }
    // Events split the render: a parameter change on every call.
    let keys = [36u8, 40, 43, 47, 50, 52, 55, 59];
    let id = wire("cutoff");
    let moving = per_block(&[], &keys, 256, &[EventKind::Param { id, value: 900.0 }]);
    let still = per_block(&[], &keys, 256, &[]);
    println!("one parameter event per call: {:+.2} µs", moving - still);
}
