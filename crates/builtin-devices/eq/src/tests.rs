//! The EQ's sample tap for the live spectrum (WP-92): off unless the UI looks and „Analyse“ is on,
//! the EQ's mono output when it runs, no allocation, old projects load with it on.

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{ProcessContext, Processor, SampleTap, StereoIo, TransportInfo};
use komponist_model::params::ParamId;
use komponist_model::{Meter, Tick, TickRange};

use std::sync::Arc;

use komponist_device_core::InternalProcessor;

use super::{ANALYSE, SPEC};

#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BLOCK: usize = 256;

fn ctx(rate: f32) -> ProcessContext {
    ProcessContext {
        frames: BLOCK as u32,
        sample_rate: rate,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: false,
        deadline: None,
    }
}

/// An activated EQ, its tap, and a function that runs one block of the stereo ramp
/// (left 0.25, right 0.75) and returns the EQ's output.
fn eq_at(rate: f32, values: &[f64]) -> (InternalProcessor, Arc<SampleTap>) {
    let mut p = InternalProcessor::new(&SPEC, values);
    p.activate(rate, BLOCK).unwrap();
    let tap = Arc::clone(p.meter().tap().expect("the EQ has a tap"));
    (p, tap)
}

fn block(p: &mut InternalProcessor, rate: f32) -> [Vec<f32>; 2] {
    let (l, r) = (vec![0.25_f32; BLOCK], vec![0.75_f32; BLOCK]);
    let mut out = [vec![0.0; BLOCK], vec![0.0; BLOCK]];
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&l, &r],
        output: [ol, or],
    };
    assert_no_alloc(|| p.process(&ctx(rate), &[], &mut io));
    out
}

#[test]
fn analyse_is_the_last_parameter_and_on_by_default() {
    let params = SPEC.params();
    assert_eq!(SPEC.wire(&ParamId::new("analyse")), Some(ANALYSE as u32));
    assert_eq!(ANALYSE, params.len() - 1);
    assert_eq!(params[ANALYSE].default, 1.0);
}

#[test]
fn a_state_saved_before_the_parameter_existed_loads_with_analyse_on() {
    let state = br#"{"params": {"out": -3.0, "b1_freq": 100.0}}"#;
    let values = SPEC.decode_state(Some(state)).unwrap();
    assert_eq!(values[ANALYSE], 1.0);
    assert_eq!(values[0], -3.0);
    let (_p, tap) = eq_at(48_000.0, &values);
    assert_eq!(tap.rate(), 48_000.0);
}

#[test]
fn a_hidden_eq_copies_nothing_and_a_visible_one_taps_its_mono_output() {
    let (mut p, tap) = eq_at(44_100.0, &SPEC.defaults());
    assert_eq!(tap.rate(), 44_100.0);
    for _ in 0..4 {
        block(&mut p, 44_100.0);
    }
    assert_eq!(tap.written(), 0, "no interest, no work");

    tap.want();
    let out = block(&mut p, 44_100.0);
    assert_eq!(tap.written(), BLOCK as u64);
    let mut window = vec![0.0; 64];
    tap.read_latest(&mut window).unwrap();
    for (i, s) in window.iter().enumerate() {
        let i = BLOCK - 64 + i;
        assert_eq!(*s, 0.5 * (out[0][i] + out[1][i]));
    }
}

#[test]
fn analyse_off_stops_the_tap_even_while_the_ui_looks() {
    let mut values = SPEC.defaults();
    values[ANALYSE] = 0.0;
    let (mut p, tap) = eq_at(48_000.0, &values);
    tap.want();
    block(&mut p, 48_000.0);
    assert_eq!(tap.written(), 0);
    assert!(tap.is_wanted(), "the lease is not used up either");
}
