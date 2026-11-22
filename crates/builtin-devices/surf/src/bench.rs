//! CPU cost against volt, measured in release builds:
//! `cargo test --release -p komponist-surf --lib bench -- --ignored --nocapture`.
//! The budget of the proposal: at most 1.5 × volt with the same settings.

use std::hint::black_box;
use std::time::Instant;

use komponist_device_core::InternalProcessor;
use komponist_engine::{Processor, StereoIo};
use komponist_model::params::ParamId;

use super::tests::{RATE, ctx, on, render};

fn make(spec: &'static komponist_device_core::Spec, changes: &[(&str, f64)]) -> InternalProcessor {
    let mut values = spec.defaults();
    for (key, value) in changes {
        if let Some(w) = spec.wire(&ParamId::new(*key)) {
            values[w as usize] = *value;
        }
    }
    let dsp = if std::ptr::eq(spec, &crate::SPEC) {
        crate::dsp()
    } else {
        komponist_volt::dsp()
    };
    let mut p = InternalProcessor::from_dsp(spec, &values, dsp);
    p.activate(RATE, 4096).unwrap();
    p
}

/// Microseconds per call of `block` frames with `keys` held, averaged over about 2 s. Only
/// `Processor::process` is timed: the buffers, the context and the (empty) event list are made
/// before the clock starts.
fn per_block(
    spec: &'static komponist_device_core::Spec,
    changes: &[(&str, f64)],
    keys: &[u8],
    block: usize,
) -> f64 {
    let mut p = make(spec, changes);
    let held: Vec<_> = keys.iter().map(|&k| (0, on(k, 100))).collect();
    render(&mut p, 9600, &held, 120.0);
    let calls = (2.0 * RATE) as usize / block;
    let context = ctx(block, RATE);
    let input = [vec![0.0f32; block], vec![0.0f32; block]];
    let mut out = [vec![0.0f32; block], vec![0.0f32; block]];
    let started = Instant::now();
    for _ in 0..calls {
        let [left, right] = &mut out;
        let mut io = StereoIo {
            input: [&input[0], &input[1]],
            output: [left, right],
        };
        p.process(&context, &[], &mut io);
        black_box(&out);
    }
    started.elapsed().as_secs_f64() * 1e6 / calls as f64
}

#[test]
#[ignore]
fn cpu_against_volt() {
    let wide = |u: f64| vec![("unison", u), ("spread", 0.6), ("detune", 14.0)];
    let moving = vec![
        ("mod1_src", 1.0),
        ("mod1_dst", 3.0),
        ("mod1_amt", 0.5),
        ("mod2_src", 2.0),
        ("mod2_dst", 4.0),
        ("mod2_amt", 0.5),
    ];
    let cases: [(&str, Vec<(&str, f64)>); 4] = [
        ("unison 1", vec![("unison", 1.0)]),
        ("unison 3 + spread", wide(3.0)),
        ("unison 7 + spread", wide(7.0)),
        ("unison 3, both positions moving", {
            let mut v = wide(3.0);
            v.extend(moving);
            v
        }),
    ];
    let all = [
        36u8, 40, 43, 47, 50, 52, 55, 59, 62, 64, 67, 71, 74, 76, 79, 83,
    ];
    for (name, changes) in &cases {
        // Unison 7 plays six keys at most (48 oscillator groups): one row for it.
        let counts: &[usize] = if name.contains("unison 7") {
            &[6]
        } else {
            &[8, 16]
        };
        for &voices in counts {
            let keys = &all[..voices];
            let mut changes = changes.clone();
            changes.push(("voices", 16.0));
            let surf = per_block(&crate::SPEC, &changes, keys, 128);
            let volt = per_block(&komponist_volt::SPEC, &changes, keys, 128);
            println!(
                "{name:34} {:2} keys, 128 frames: surf {surf:7.1} µs ({:4.1} % of 2.67 ms), volt {volt:7.1} µs, ratio {:.2}",
                keys.len(),
                surf / 26.7,
                surf / volt
            );
        }
    }
}
