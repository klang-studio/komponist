//! Tests of the internal devices: catalog data, parameters, layouts, state, and the processors
//! (pass-through bit-exact, no allocation in `process` and `reset`).

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{Event, EventKind, ProcessContext, Processor, StereoIo, TransportInfo};
use komponist_model::params::layout::{BAND, validate};
use komponist_model::params::{ParamContext, ParamId};
use komponist_model::{Meter, Tick, TickRange};

use super::*;

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: f32 = 48_000.0;
const BLOCK: usize = 256;

fn ctx(frames: usize) -> ProcessContext {
    ProcessContext {
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
        offline: false,
        deadline: None,
    }
}

fn activated(kind: Internal, values: &[f64]) -> InternalProcessor {
    let mut p = kind.processor(values);
    p.activate(RATE, BLOCK).unwrap();
    p
}

/// A test signal that is never 0 and stays below 0.9.
fn signal(frames: usize, offset: usize) -> [Vec<f32>; 2] {
    let wave = |phase: f32| {
        (0..frames)
            .map(|i| 0.1 + 0.8 * ((i + offset) as f32 * phase).sin().abs())
            .collect()
    };
    [wave(0.031), wave(0.047)]
}

/// One call inside `assert_no_alloc`; returns the output.
fn run(p: &mut InternalProcessor, events: &[Event], input: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    let frames = input[0].len();
    let mut out = [vec![0.0; frames], vec![0.0; frames]];
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    assert_no_alloc(|| p.process(&ctx(frames), events, &mut io));
    out
}

fn param_event(kind: Internal, key: &str, value: f64, offset: u32) -> Event {
    let id = kind.wire(&ParamId::new(key)).expect("known parameter");
    Event {
        offset,
        kind: EventKind::Param { id, value },
    }
}

#[test]
fn the_catalog_data_is_unique_and_resolves_both_ways() {
    let names: Vec<&str> = Internal::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(
        names,
        ["EQ", "Delay", "Hall", "Limiter", "Arpeggiator", "Tonleiter"]
    );
    for kind in Internal::ALL {
        assert_eq!(Internal::from_id(kind.id()), Some(kind));
        let plugin = kind.plugin_ref();
        assert_eq!(
            (plugin.format, plugin.name.as_str()),
            (PluginFormat::Builtin, kind.name())
        );
        assert_eq!(Internal::from_plugin(&plugin), Some(kind));
        assert!(!kind.features().is_empty());
    }
    let mut ids: Vec<&str> = Internal::ALL.iter().map(|k| k.id()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), Internal::ALL.len());
    assert_eq!(Internal::from_id("pisynth"), None);
    let mut clap = Internal::Eq.plugin_ref();
    clap.format = PluginFormat::Clap;
    assert_eq!(Internal::from_plugin(&clap), None);
}

#[test]
fn parameters_have_unique_ids_and_defaults_inside_their_range() {
    for kind in Internal::ALL {
        let params = kind.params();
        assert!(!params.is_empty());
        let mut ids: Vec<&str> = params.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), params.len(), "{kind:?}");
        for p in params {
            let (min, max) = p.range();
            assert!(
                (min..=max).contains(&p.default),
                "{kind:?} {}: default {} outside {min}..{max}",
                p.id,
                p.default
            );
            assert_eq!(p.snap(p.default), p.default, "{kind:?} {}", p.id);
            assert!(!p.name.is_empty());
            assert!(!p.format(p.default).is_empty());
        }
    }
}

#[test]
fn layouts_validate_for_every_band() {
    for kind in Internal::ALL {
        let known = |id: &ParamId| kind.wire(id).is_some();
        for band in 1..=eq::BANDS {
            let ctx = ParamContext::new().with(BAND, band);
            let errors = validate(kind.layout(), &ctx, &known);
            assert!(errors.is_empty(), "{kind:?} band {band}: {errors:?}");
        }
    }
}

#[test]
fn every_parameter_of_a_device_is_reachable_from_its_layout_or_the_eq_graph() {
    use komponist_model::params::layout::CellParam;
    for kind in [
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
        Internal::Arpeggiator,
        Internal::Tonleiter,
    ] {
        let mut shown = std::collections::BTreeSet::new();
        for cell in kind.layout().pages.iter().flat_map(|p| &p.cells) {
            match &cell.param {
                CellParam::Id(id) => {
                    shown.insert(id.as_str().to_owned());
                }
                CellParam::Graph(b) => {
                    shown.extend(b.bind.iter().map(|(_, id)| id.clone()));
                }
                _ => {}
            }
        }
        for p in kind.params() {
            assert!(
                shown.contains(p.id.as_str()),
                "{kind:?}: {} is not on the card",
                p.id
            );
        }
    }
}

#[test]
fn state_round_trips_and_tolerates_missing_unknown_and_wild_values() {
    for kind in Internal::ALL {
        let defaults = kind.defaults();
        assert_eq!(kind.decode_state(None).unwrap(), defaults);
        let state = kind.encode_state(&defaults);
        assert_eq!(
            kind.decode_state(Some(&state)).unwrap(),
            defaults,
            "{kind:?}"
        );
        // Readable JSON keyed by parameter id.
        let json: serde_json::Value = serde_json::from_slice(&state).unwrap();
        for p in kind.params() {
            assert!(json["params"][p.id.as_str()].is_number(), "{}", p.id);
        }
    }
    // A changed value survives; a missing id gets its default, an unknown one is ignored, a value
    // outside the range is clamped, a stepped one snapped.
    let eq = Internal::Eq;
    let freq = eq.wire(&ParamId::new("b3_freq")).unwrap() as usize;
    let on = eq.wire(&ParamId::new("b6_on")).unwrap() as usize;
    let mut values = eq.defaults();
    values[freq] = 1234.0;
    let state = eq.encode_state(&values);
    assert_eq!(eq.decode_state(Some(&state)).unwrap()[freq], 1234.0);
    let loaded = eq
        .decode_state(Some(
            br#"{"params": {"b3_freq": 99999, "b6_on": 0.7, "gone": 1}}"#,
        ))
        .unwrap();
    assert_eq!(loaded[freq], 20_000.0);
    assert_eq!(loaded[on], 1.0);
    assert_eq!(loaded[0], eq.defaults()[0]);
    assert!(eq.decode_state(Some(b"{\"params\": ")).is_err());
    assert!(eq.decode_state(Some(b"[1]")).is_err());
    assert!(eq.decode_state(Some(b"{}")).is_err());
    // `with_value` edits one value and snaps it.
    let state = eq.with_value(None, freq as u32, 500.0).unwrap();
    assert_eq!(eq.decode_state(Some(&state)).unwrap()[freq], 500.0);
    let state = eq.with_value(Some(&state), on as u32, 0.2).unwrap();
    let values = eq.decode_state(Some(&state)).unwrap();
    assert_eq!((values[freq], values[on]), (500.0, 0.0));
    assert_eq!(eq.with_value(None, 999, 1.0), None);
    // A broken state is replaced by the defaults plus the change.
    let state = eq.with_value(Some(b"junk"), freq as u32, 700.0).unwrap();
    assert_eq!(eq.decode_state(Some(&state)).unwrap()[freq], 700.0);
}

#[test]
fn eq_hall_and_a_default_limiter_pass_audio_bit_exact() {
    let input = signal(BLOCK, 0);
    for kind in [Internal::Eq, Internal::Hall, Internal::Limiter] {
        let mut p = activated(kind, &kind.defaults());
        let out = run(&mut p, &[], &input);
        assert_eq!(out, input, "{kind:?}");
    }
    // Hall ignores every parameter.
    let mut hall = activated(Internal::Hall, &Internal::Hall.defaults());
    let events = [param_event(Internal::Hall, "mix", 1.0, 10)];
    assert_eq!(run(&mut hall, &events, &input), input);
}

#[test]
fn the_eq_applies_only_its_output_gain() {
    let input = signal(BLOCK, 0);
    let mut p = activated(Internal::Eq, &Internal::Eq.defaults());
    let events = [param_event(Internal::Eq, "out", -6.0, 100)];
    let out = run(&mut p, &events, &input);
    assert_eq!(out[0][..100], input[0][..100]);
    let gain = db_to_gain(-6.0);
    for i in 100..BLOCK {
        assert_eq!(out[0][i], input[0][i] * gain);
    }
    // A band's gain does not change the placeholder's audio.
    let mut p = activated(Internal::Eq, &Internal::Eq.defaults());
    let events = [param_event(Internal::Eq, "b3_gain", 12.0, 0)];
    assert_eq!(run(&mut p, &events, &input), input);
}

#[test]
fn the_limiter_clips_at_the_ceiling() {
    let kind = Internal::Limiter;
    let mut values = kind.defaults();
    values[kind.wire(&ParamId::new("ceiling")).unwrap() as usize] = -6.0;
    values[kind.wire(&ParamId::new("gain")).unwrap() as usize] = 12.0;
    let mut p = activated(kind, &values);
    let loud = [vec![0.9, -0.9, 0.01, 0.0], vec![0.5, -0.5, 0.0, 0.2]];
    let out = run(&mut p, &[], &loud);
    let ceiling = db_to_gain(-6.0);
    assert_eq!(out[0][0], ceiling);
    assert_eq!(out[0][1], -ceiling);
    assert_eq!(out[1][0], ceiling, "0.5 with +12 dB is above the ceiling");
    assert_eq!(out[0][3], 0.0);
    assert!(out[0][2] > 0.01 && out[0][2] < ceiling);
    assert!(out.iter().flatten().all(|s| s.abs() <= ceiling));
}

#[test]
fn the_delay_echoes_after_its_time_and_dies_out() {
    let kind = Internal::Delay;
    let mut values = kind.defaults();
    let set = |values: &mut Vec<f64>, key: &str, v: f64| {
        values[kind.wire(&ParamId::new(key)).unwrap() as usize] = v;
    };
    set(&mut values, "time", 10.0);
    set(&mut values, "mix", 1.0);
    set(&mut values, "feedback", 0.5);
    set(&mut values, "ping_pong", 0.0);
    let mut p = activated(kind, &values);
    let frames = 2048;
    let mut input = [vec![0.0; frames], vec![0.0; frames]];
    input[0][0] = 1.0;
    let out = run(&mut p, &[], &input);
    let t = (10.0 * RATE / 1000.0) as usize;
    assert_eq!(out[0][..t - 1].iter().map(|s| s.abs()).sum::<f32>(), 0.0);
    assert!((out[0][t] - 1.0).abs() < 1e-6, "{}", out[0][t]);
    assert!((out[0][2 * t] - 0.5).abs() < 1e-6, "{}", out[0][2 * t]);
    assert_eq!(out[1].iter().map(|s| s.abs()).sum::<f32>(), 0.0);
    // Ping-pong sends the repeats to the other side.
    set(&mut values, "ping_pong", 1.0);
    let mut p = activated(kind, &values);
    let out = run(&mut p, &[], &input);
    assert!((out[0][t] - 1.0).abs() < 1e-6);
    assert!((out[1][2 * t] - 0.5).abs() < 1e-6, "{}", out[1][2 * t]);
    assert!(out[0][2 * t].abs() < 1e-6);
    // The tail is silent after `reset`, and 0 % mix passes the input bit-exact.
    p.reset();
    let silent = [vec![0.0; BLOCK], vec![0.0; BLOCK]];
    assert_eq!(run(&mut p, &[], &silent), silent);
    let events = [param_event(kind, "mix", 0.0, 0)];
    let input = signal(BLOCK, 0);
    assert_eq!(run(&mut p, &events, &input), input);
}

#[test]
fn parameter_events_apply_at_their_frame_and_unknown_ones_are_ignored() {
    let kind = Internal::Limiter;
    let mut p = activated(kind, &kind.defaults());
    let ceiling = kind.wire(&ParamId::new("ceiling")).unwrap();
    let events = [
        Event {
            offset: 4,
            kind: EventKind::Param {
                id: ceiling,
                value: -12.0,
            },
        },
        Event {
            offset: 4,
            kind: EventKind::Param { id: 99, value: 1.0 },
        },
        Event {
            offset: 6,
            kind: EventKind::AllNotesOff,
        },
    ];
    let input = [vec![0.99; 8], vec![0.99; 8]];
    let out = run(&mut p, &events, &input);
    let (before, after) = (db_to_gain(-0.3), db_to_gain(-12.0));
    assert!(out[0][..4].iter().all(|s| *s == before));
    assert!(out[0][4..].iter().all(|s| *s == after));
}

#[test]
fn processing_and_reset_do_not_allocate_in_any_device() {
    let input = signal(BLOCK, 0);
    for kind in Internal::ALL {
        let mut p = activated(kind, &kind.defaults());
        let events: Vec<Event> = kind
            .params()
            .iter()
            .enumerate()
            .map(|(i, info)| Event {
                offset: (i % BLOCK) as u32,
                kind: EventKind::Param {
                    id: i as u32,
                    value: info.range().1,
                },
            })
            .collect();
        let mut sorted = events;
        sorted.sort_by_key(|e| e.offset);
        for round in 0..4 {
            let block = signal(BLOCK, round * BLOCK);
            let out = run(&mut p, if round == 0 { &sorted } else { &[] }, &block);
            assert!(out.iter().flatten().all(|s| s.is_finite()), "{kind:?}");
        }
        assert_no_alloc(|| p.reset());
        let _ = run(&mut p, &[], &input);
    }
}

#[test]
fn activation_checks_the_sample_rate_and_the_delay_keeps_its_time() {
    let mut p = Internal::Delay.processor(&Internal::Delay.defaults());
    assert!(p.activate(100.0, BLOCK).is_err());
    p.activate(96_000.0, BLOCK).unwrap();
    assert_eq!(p.sample_rate(), 96_000.0);
    assert_eq!(p.latency(), 0);
    // A different rate keeps the time in milliseconds: 375 ms = 36 000 frames at 96 kHz.
    let mut input = [vec![0.0; 40_000], vec![0.0; 40_000]];
    input[0][0] = 1.0;
    let mut values = Internal::Delay.defaults();
    values[3] = 1.0;
    values[1] = 0.0;
    let mut p = Internal::Delay.processor(&values);
    p.activate(96_000.0, 40_000).unwrap();
    let frames = input[0].len();
    let mut out = [vec![0.0; frames], vec![0.0; frames]];
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    let mut c = ctx(frames);
    c.sample_rate = 96_000.0;
    p.process(&c, &[], &mut io);
    assert!((out[0][36_000] - 1.0).abs() < 1e-6);
}

#[test]
fn the_meter_feed_reports_input_reduction_and_output_without_allocating() {
    let kind = Internal::Limiter;
    let mut p = activated(kind, &kind.defaults());
    let meter = p.meter();
    // Quiet signal: no reduction, output equals input.
    let quiet: [Vec<f32>; 2] = [vec![0.25; BLOCK], vec![0.1; BLOCK]];
    run(&mut p, &[], &quiet);
    let (i, gr, o) = meter.take();
    assert_eq!((i, gr, o), (0.25, 0.0, 0.25));
    // Taken: the next take is empty.
    assert_eq!(meter.take(), (0.0, 0.0, 0.0));
    // Twice the ceiling is about 6 dB over it.
    let ceiling = db_to_gain(-0.3);
    let loud: [Vec<f32>; 2] = [vec![2.0 * ceiling; BLOCK], vec![0.0; BLOCK]];
    let out = run(&mut p, &[], &loud);
    let (i, gr, o) = meter.take();
    assert!((i - 2.0 * ceiling).abs() < 1e-6);
    assert!((gr - 6.02).abs() < 0.01, "{gr}");
    assert!((o - ceiling).abs() < 1e-6 && out[0][0] <= ceiling + 1e-6);
    // A device that does not reduce reports 0 dB and its own peaks.
    let eq = Internal::Eq;
    let mut e = activated(eq, &eq.defaults());
    let m = e.meter();
    run(&mut e, &[], &quiet);
    let (i, gr, _) = m.take();
    assert_eq!((i, gr), (0.25, 0.0));
}
