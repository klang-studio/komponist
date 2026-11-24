//! A mixed built-in effects chain renders bit-identically with and without engine workers.

use komponist_builtin::Internal;
use komponist_engine::Processor;
use komponist_engine::testing::TestSine;
use komponist_model::{Tick, TickRange};

use crate::common::{BAR, RATE, effect, program, track};
use crate::material::material;
use crate::parallel_support::{Setup, assert_bit_exact, play};

/// 8 tracks, each a `TestSine` and five internal effects in a chain (EQ with active bands,
/// tempo-synced Delay, Hall, the compressor `presse` with lookahead, SC-HP, partial link and mix,
/// lookahead Limiter; WP-90, WP-93), so the transport tempo, the latencies and the effects'
/// control-rate counters all go through the worker pool.
fn effects() -> Setup {
    let mut tracks = Vec::new();
    let mut processors: Vec<(u32, Box<dyn Processor>)> = Vec::new();
    for i in 0..8u32 {
        let mut t = track(u64::from(i) + 1, i, material(i));
        let mut eq = Internal::Eq.defaults();
        eq[Internal::Eq.wire(&"b1_type".into()).unwrap() as usize] = 0.0;
        eq[Internal::Eq.wire(&"b3_gain".into()).unwrap() as usize] = 6.0 - f64::from(i);
        let mut delay = Internal::Delay.defaults();
        delay[Internal::Delay.wire(&"sync".into()).unwrap() as usize] = 1.0;
        delay[Internal::Delay.wire(&"note".into()).unwrap() as usize] = f64::from(2 + i % 6);
        delay[Internal::Delay.wire(&"mix".into()).unwrap() as usize] = 0.5;
        let mut limiter = Internal::Limiter.defaults();
        limiter[Internal::Limiter.wire(&"gain".into()).unwrap() as usize] = 12.0;
        let mut presse = Internal::Presse.defaults();
        for (key, value) in [
            ("threshold", -30.0),
            ("ratio", 6.0),
            ("detector", f64::from(i % 2)),
            ("link", 0.5),
            ("lookahead", 1.0),
            ("sc_hpf", 100.0),
            ("mix", 0.8),
        ] {
            presse[Internal::Presse.wire(&key.into()).unwrap() as usize] = value;
        }
        let chain = [
            (Internal::Eq, eq),
            (Internal::Delay, delay),
            (Internal::Hall, Internal::Hall.defaults()),
            (Internal::Presse, presse),
            (Internal::Limiter, limiter),
        ];
        for (k, (kind, values)) in chain.into_iter().enumerate() {
            let slot = 100 + i * 5 + k as u32;
            let mut p = kind.processor(&values);
            p.activate(RATE, 256).unwrap();
            t.chain.push(effect(slot));
            processors.push((slot, Box::new(p)));
        }
        // All tracks have the same latency (the lookaheads): nothing to compensate.
        t.pdc_delay = 0;
        tracks.push(t);
        processors.push((i, Box::new(TestSine::new())));
    }
    let mut p = program(127.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
    p.loop_on = true;
    (p, processors)
}

#[test]
fn internal_effects_are_bit_exact_with_workers() {
    let frames = 2 * 48_000;
    let single = play(0, frames, effects);
    for workers in [2, 4] {
        let parallel = play(workers, frames, effects);
        assert_bit_exact(&single, &parallel, &format!("effects, {workers} workers"));
    }
}
