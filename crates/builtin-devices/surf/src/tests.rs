//! surf's tests: parameters, layout and catalog, the band-limited bank, the oscillator (frames,
//! interpolation, aliasing, sweeps), tuning, the matrix on the position, presets, state, realtime
//! safety and determinism. Rendering goes through the real [`InternalProcessor`]; volt's own
//! tests cover the shared voice (filter, envelopes, LFOs, unison, glide, stealing).

use assert_no_alloc::assert_no_alloc;
use komponist_i18n::Lang;
use komponist_model::params::layout::{BAND, ENV_VIEW, LFO_VIEW, OSC_VIEW, SEL, validate};
use komponist_model::params::va_curve::{dst, src};
use komponist_model::params::wavetable::{self, FRAMES, Recipe, TABLES};
use komponist_model::params::{ParamContext, ParamId};
use komponist_model::{Meter, Tick, TickRange};
use komponist_processor::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, Processor, StereoIo, TransportInfo,
};
use komponist_volt::Oscillators;

use super::bank::{self, LEVELS, Mip, harmonics, level_bits};
use super::osc::Wavetable;
use super::params::{COUNT, OSC_A_FINE, OSC_A_OCT, params, table};
use super::*;
use komponist_device_core::InternalProcessor;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub(super) const RATE: f32 = 48_000.0;
const BLOCK: usize = 4096;

pub(super) fn ctx(frames: usize, rate: f32) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
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

pub(super) fn on(key: u8, velocity: u8) -> EventKind {
    EventKind::NoteOn {
        key,
        velocity,
        channel: 0,
        note_id: NOTE_ID_NONE,
    }
}

fn off(key: u8) -> EventKind {
    EventKind::NoteOff {
        key,
        channel: 0,
        note_id: NOTE_ID_NONE,
    }
}

fn cc(number: u8, value: u8) -> EventKind {
    EventKind::Midi([0xB0, number, value])
}

/// The wire id of parameter `key`.
pub(super) fn wire(key: &str) -> u32 {
    SPEC.wire(&ParamId::new(key)).expect(key)
}

/// A processor with the defaults and `changes`, activated at `rate`.
pub(super) fn processor_at(changes: &[(&str, f64)], rate: f32) -> InternalProcessor {
    let mut values = SPEC.defaults();
    for (key, value) in changes {
        values[wire(key) as usize] = *value;
    }
    let mut p = InternalProcessor::from_dsp(&SPEC, &values, crate::dsp());
    p.activate(rate, BLOCK).unwrap();
    p
}

pub(super) fn processor(changes: &[(&str, f64)]) -> InternalProcessor {
    processor_at(changes, RATE)
}

/// Oscillator A alone, no sub, the filter wide open, no drift: what the table gives.
const CLEAN: &[(&str, f64)] = &[
    ("osc_mix", 0.0),
    ("sub_level", 0.0),
    ("filter_type", 1.0),
    ("cutoff", 20_000.0),
    ("resonance", 0.0),
    ("drive", 0.0),
    ("env_amount", 0.0),
    ("key_amount", 0.0),
    ("analog", 0.0),
    ("wheel_vib", 0.0),
    ("amp_a", 0.001),
    ("amp_s", 1.0),
    ("volume", 0.0),
    ("velocity", 0.0),
];

fn merged(extra: &[(&str, f64)]) -> Vec<(&'static str, f64)> {
    let keys: Vec<&'static str> = SPEC
        .params()
        .iter()
        .map(|p| -> &'static str { Box::leak(p.id.as_str().to_owned().into_boxed_str()) })
        .collect();
    let mut all: Vec<(&'static str, f64)> = CLEAN.to_vec();
    for (key, value) in extra {
        let key = keys.iter().find(|k| *k == key).copied().expect(key);
        all.retain(|(k, _)| *k != key);
        all.push((key, *value));
    }
    all
}

/// Renders `frames` in one call at `rate` (events at their offsets), inside `assert_no_alloc`.
fn render_at(
    p: &mut InternalProcessor,
    frames: usize,
    events: &[(u32, EventKind)],
    rate: f32,
) -> [Vec<f32>; 2] {
    let input = [vec![0.0f32; frames], vec![0.0f32; frames]];
    let mut out = [vec![0.0f32; frames], vec![0.0f32; frames]];
    let list: Vec<Event> = events
        .iter()
        .map(|(offset, kind)| Event {
            offset: *offset,
            kind: *kind,
        })
        .collect();
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    assert_no_alloc(|| p.process(&ctx(frames, rate), &list, &mut io));
    out
}

pub(super) fn render(
    p: &mut InternalProcessor,
    frames: usize,
    events: &[(u32, EventKind)],
    _bpm: f64,
) -> [Vec<f32>; 2] {
    render_at(p, frames, events, RATE)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// Upward zero crossings per second.
fn frequency(x: &[f32], rate: f32) -> f64 {
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f64 * f64::from(rate) / x.len() as f64
}

/// Power spectrum of `x` (a power-of-two length), DFT by `realfft`.
fn power(x: &[f32]) -> Vec<f64> {
    let mut planner = realfft::RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(x.len());
    let mut input: Vec<f64> = x.iter().map(|v| f64::from(*v)).collect();
    let mut out = fft.make_output_vec();
    fft.process(&mut input, &mut out).unwrap();
    out.iter().map(|c| c.norm_sqr()).collect()
}

// ------------------------------------------------------------------------------------------------
// Parameters, layout, catalog

#[test]
fn the_parameter_table_is_volts_with_the_oscillators_as_wavetables() {
    let list = params(Lang::De);
    let volt = komponist_volt::params::params(Lang::De);
    assert_eq!(list.len(), COUNT);
    assert_eq!(COUNT, komponist_volt::params::COUNT + 2);
    let key = |i: usize| list[i].id.as_str();
    assert_eq!(key(komponist_volt::id::OSC_A_WAVE), "osc_a_table");
    assert_eq!(key(komponist_volt::id::OSC_B_WAVE), "osc_b_table");
    assert_eq!(key(komponist_volt::id::OSC_A_PW), "osc_a_pos");
    assert_eq!(key(komponist_volt::id::OSC_B_PW), "osc_b_pos");
    assert_eq!(key(OSC_A_OCT), "osc_a_oct");
    assert_eq!(key(OSC_A_FINE), "osc_a_fine");
    // Everything else is volt's, in volt's places: the shared engine reads it there.
    for (i, (s, v)) in list.iter().zip(&volt).enumerate() {
        if ![0, 1, 2, 3].contains(&i) {
            assert_eq!(s.id, v.id, "wire id {i}");
            assert_eq!(s.range(), v.range(), "{}", s.id);
        }
    }
    let get = |k: &str| list.iter().find(|p| p.id.as_str() == k).unwrap();
    assert_eq!(get("osc_a_table").steps(), TABLES.len() as u32 - 1);
    assert_eq!(get("osc_a_pos").range(), (0.0, 1.0));
    assert_eq!(get("osc_a_oct").range(), (-3.0, 3.0));
    let text = |k: &str, v: f64| get(k).format(v);
    assert_eq!(text("osc_a_table", table::VOWELS), "Vokale");
    assert_eq!(text("osc_b_pos", 0.42), "42 %");
    assert_eq!(text("osc_a_oct", -1.0), "\u{2212}1 Okt.");
    assert_eq!(text("mod1_dst", dst::PW_A as f64), "Position A");
    assert_eq!(text("mod4_dst", dst::PW_B as f64), "Position B");
    let en = params(Lang::En);
    let en_text = |k: &str, v: f64| en.iter().find(|p| p.id.as_str() == k).unwrap().format(v);
    assert_eq!(en_text("osc_a_table", table::ODD_EVEN), "Odd–Even");
    assert_eq!(en[OSC_A_FINE].name, "A Fine");
}

/// Every table has a German and an English name, and the German names are the model's.
#[test]
fn the_tables_are_named_in_both_languages() {
    assert_eq!(params::table_names(Lang::De), TABLES);
    let en = params::table_names(Lang::En);
    assert!(en.iter().all(|n| !n.is_empty()));
    let mut unique = en.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), TABLES.len());
}

fn context(osc: &str, env: &str, lfo: &str) -> ParamContext {
    ParamContext::new()
        .with(BAND, 1)
        .with(SEL, 1)
        .with(OSC_VIEW, osc)
        .with(ENV_VIEW, env)
        .with(LFO_VIEW, lfo)
}

#[test]
fn the_layout_validates_for_both_oscillators_and_shows_every_parameter() {
    let known = |p: &ParamId| SPEC.wire(p).is_some();
    let mut shown = std::collections::BTreeSet::new();
    for osc in ["a", "b"] {
        for env in ["amp", "filt", "mod"] {
            for lfo in ["1", "2"] {
                let ctx = context(osc, env, lfo);
                let errors = validate(SPEC.layout(), &ctx, &known);
                assert!(errors.is_empty(), "{osc} {env} {lfo}: {errors:?}");
                for cell in SPEC.layout().pages.iter().flat_map(|p| &p.cells) {
                    if let Ok(Some(id)) = cell.resolved(&ctx) {
                        shown.insert(id.as_str().to_owned());
                    }
                }
            }
        }
    }
    for info in SPEC.params() {
        let divider = info.id.as_str().ends_with("_div");
        let rate = info.id.as_str().replace("_div", "_rate");
        assert!(
            shown.contains(info.id.as_str()) || (divider && shown.contains(&rate)),
            "{} is not on a page",
            info.id
        );
    }
    let pages: Vec<(&str, u8)> = SPEC
        .layout()
        .pages
        .iter()
        .map(|p| (p.name.as_str(), p.cols))
        .collect();
    assert_eq!(pages, [("Klang", 10), ("Hüllkurven", 8), ("Matrix", 8)]);
}

#[test]
fn surf_is_an_instrument_in_the_catalog() {
    let catalog = &SPEC.catalog;
    assert_eq!(catalog.name, "Surf");
    assert_eq!(catalog.id, "surf");
    assert_eq!(Lang::En.device_name_surf(), "Surf");
    assert_eq!(catalog.role, komponist_model::DeviceRole::Instrument);
    assert!(catalog.features.contains(&"wavetable"));
}

// ------------------------------------------------------------------------------------------------
// The bank

#[test]
fn the_levels_halve_their_harmonics_and_keep_four_samples_per_harmonic() {
    assert_eq!(harmonics(0), 512);
    assert_eq!(level_bits(0), 11);
    assert_eq!(harmonics(LEVELS - 1), 1);
    for l in 0..LEVELS {
        assert!((1usize << level_bits(l)) >= 4 * harmonics(l));
        assert!(level_bits(l) >= 8);
    }
}

#[test]
fn every_frame_is_finite_guarded_and_loud_enough_without_clipping() {
    let bank = bank::bank();
    for (t, name) in TABLES.iter().enumerate() {
        for f in 0..FRAMES {
            for l in 0..LEVELS {
                let s = bank.table(t).frame(f, l);
                assert_eq!(s.len(), (1 << level_bits(l)) + 1);
                assert_eq!(s[s.len() - 1], s[0], "guard {t}/{f}/{l}");
                assert!(s.iter().all(|v| v.is_finite()));
                assert!(peak(s) <= 1.0 + 1e-5, "{name}/{f}/{l}: peak {}", peak(s));
            }
            let widest = bank.table(t).frame(f, 0);
            let level = rms(&widest[..widest.len() - 1]);
            assert!(level > 0.12, "{name} frame {f}: quiet ({level})");
        }
    }
}

#[test]
fn the_bank_matches_the_recipes() {
    // Frame 0 of the basic table is a sine of the target level, the last a square's harmonics.
    let s = bank::bank().table(0).frame(0, 0);
    let n = s.len() - 1;
    let a = peak(&s[..n]);
    for (i, v) in s[..n].iter().enumerate() {
        let want = a * (std::f32::consts::TAU * i as f32 / n as f32).sin();
        assert!((v - want).abs() < 1e-4, "{i}: {v} vs {want}");
    }
    // A shape recipe follows its waveform: the widest level correlates with the recipe's preview
    // (the same formula, coarse; Gibbs ripples and the scale differ).
    let Recipe::Shape(_) = wavetable::recipe(table::SYNC as usize) else {
        panic!("sync is a shape");
    };
    for f in [0, FRAMES / 2, FRAMES - 1] {
        let s = bank::bank().table(table::SYNC as usize).frame(f, 0);
        let n = s.len() - 1;
        let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
        for i in 0..128 {
            let x = f64::from(s[i * n / 128]);
            let y = wavetable::preview_at(
                table::SYNC as usize,
                wavetable::frame_position(f),
                i as f64 / 128.0,
            );
            xy += x * y;
            xx += x * x;
            yy += y * y;
        }
        let r = xy / (xx * yy).sqrt();
        // A mirrored or shifted frame (a wrong FFT convention) would be far below.
        assert!(r > 0.9, "sync frame {f}: correlation {r}");
    }
}

#[test]
fn the_levels_never_alias_and_cross_without_a_step() {
    let mut last = Mip::for_increment(1);
    for i in 1..4000 {
        // 1 Hz … 22 kHz at 48 kHz, in small steps.
        let hz = (i as f64 * 0.0036).exp2().min(22_000.0);
        let inc = (hz / 48_000.0 * 4_294_967_296.0) as u32;
        let mip = Mip::for_increment(inc);
        let f = hz / 48_000.0;
        for level in [mip.a, mip.b] {
            assert!(
                harmonics(level) as f64 * f <= 0.5 + 1e-6 || level == LEVELS - 1,
                "{hz} Hz: level {level}"
            );
        }
        // The brightness changes smoothly: the weighted level index never jumps.
        let index = |m: Mip| m.a as f32 + (m.b as f32 - m.a as f32) * m.w;
        assert!((index(mip) - index(last)).abs() < 0.05, "{hz} Hz");
        last = mip;
    }
}

// ------------------------------------------------------------------------------------------------
// The oscillator

/// Oscillator `table` at `position` and `hz` for `n` samples at 48 kHz, straight from
/// [`Wavetable`].
fn osc(table: usize, position: f32, hz: f64, n: usize) -> Vec<f32> {
    bank::bank();
    let inc = (hz / 48_000.0 * 4_294_967_296.0).round() as u32;
    let o = Wavetable::prepare(&Default::default(), table, inc, position, RATE);
    let o = Wavetable::prepare(&o, table, inc, position, RATE);
    let mut phase = 0u32;
    (0..n)
        .map(|_| {
            let s = Wavetable::sample(&o, table, phase, 1.0);
            phase = phase.wrapping_add(inc);
            s
        })
        .collect()
}

#[test]
fn between_frames_the_oscillator_blends_linearly() {
    let step = 1.0 / (FRAMES - 1) as f32;
    let (lo, hi) = (
        osc(6, 3.0 * step, 110.0, 512),
        osc(6, 4.0 * step, 110.0, 512),
    );
    let mid = osc(6, 3.5 * step, 110.0, 512);
    for i in 0..512 {
        let want = 0.5 * (lo[i] + hi[i]);
        assert!((mid[i] - want).abs() < 1e-5, "{i}");
    }
}

/// Power that does not fall on a harmonic of `fundamental_bin`, relative to the whole.
fn alias_ratio(x: &[f32], fundamental_bin: usize) -> f64 {
    let p = power(x);
    let total: f64 = p.iter().sum();
    let stray: f64 = p
        .iter()
        .enumerate()
        .filter(|(k, _)| {
            let r = k % fundamental_bin;
            r > 1 && r < fundamental_bin - 1
        })
        .map(|(_, v)| v)
        .sum();
    stray / total
}

#[test]
fn bright_tables_stay_free_of_aliasing_up_high() {
    const N: usize = 16_384;
    // A bin-aligned fundamental of about 2.6 kHz (key 100) and of about 7 kHz.
    for bin in [887, 2389] {
        let hz = bin as f64 * 48_000.0 / N as f64;
        for t in [
            table::SYNC,
            table::PULSE,
            table::GLASS,
            table::BITS,
            table::BASIC,
        ] {
            for pos in [0.0, 0.5, 1.0] {
                let x = osc(t as usize, pos, hz, N);
                // Up high a frame may keep no harmonic at all (sync at ratio 8 has only every
                // eighth): silence does not alias.
                if rms(&x) < 1e-3 {
                    continue;
                }
                let ratio = alias_ratio(&x, bin);
                assert!(
                    ratio < 1e-5,
                    "{} at {pos}, {hz:.0} Hz: alias {:.1} dB",
                    TABLES[t as usize],
                    10.0 * ratio.log10()
                );
            }
        }
    }
}

#[test]
fn the_position_changes_the_sound_and_a_sweep_does_not_click() {
    let a = render(
        &mut processor(&merged(&[
            ("osc_a_table", table::VOWELS),
            ("osc_a_pos", 0.0),
        ])),
        24_000,
        &[(0, on(48, 100))],
        120.0,
    );
    let b = render(
        &mut processor(&merged(&[
            ("osc_a_table", table::VOWELS),
            ("osc_a_pos", 1.0),
        ])),
        24_000,
        &[(0, on(48, 100))],
        120.0,
    );
    let diff: f64 = a[0][4_000..]
        .iter()
        .zip(&b[0][4_000..])
        .map(|(x, y)| f64::from((x - y).abs()))
        .sum::<f64>()
        / 20_000.0;
    assert!(diff > 0.05, "the position does nothing ({diff})");
    // A fast LFO sweeps the whole table: the largest step between two samples stays within that
    // of a still note somewhere on the table (a step at the interval borders would stand out).
    let max_step = |x: &[f32]| {
        x[2_000..]
            .windows(2)
            .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()))
    };
    let still = (0..=10)
        .map(|i| {
            let out = render(
                &mut processor(&merged(&[
                    ("osc_a_table", table::SYNC),
                    ("osc_a_pos", f64::from(i) / 10.0),
                ])),
                12_000,
                &[(0, on(40, 100))],
                120.0,
            );
            max_step(&out[0])
        })
        .fold(0.0f32, f32::max);
    let swept = render(
        &mut processor(&merged(&[
            ("osc_a_table", table::SYNC),
            ("osc_a_pos", 0.5),
            ("lfo1_rate", 8.0),
            ("mod1_src", src::LFO1 as f64),
            ("mod1_dst", dst::PW_A as f64),
            ("mod1_amt", 1.0),
        ])),
        24_000,
        &[(0, on(40, 100))],
        120.0,
    );
    assert!(
        max_step(&swept[0]) < 1.1 * still,
        "sweep {} vs still {still}",
        max_step(&swept[0]),
    );
}

#[test]
fn the_matrix_moves_the_position_and_the_wheel_too() {
    // Wheel → Position A 100 %: the vowel table's last frame instead of its first.
    let changes = merged(&[
        ("osc_a_table", table::VOWELS),
        ("osc_a_pos", 0.0),
        ("mod1_src", src::WHEEL as f64),
        ("mod1_dst", dst::PW_A as f64),
        ("mod1_amt", 1.0),
    ]);
    let still = render(&mut processor(&changes), 12_000, &[(0, on(48, 100))], 120.0);
    let wheel = render(
        &mut processor(&changes),
        12_000,
        &[(0, cc(1, 127)), (0, on(48, 100))],
        120.0,
    );
    let end = render(
        &mut processor(&merged(&[
            ("osc_a_table", table::VOWELS),
            ("osc_a_pos", 1.0),
        ])),
        12_000,
        &[(0, on(48, 100))],
        120.0,
    );
    let distance = |x: &[f32], y: &[f32]| -> f64 {
        x[6_000..]
            .iter()
            .zip(&y[6_000..])
            .map(|(a, b)| f64::from((a - b).abs()))
            .sum::<f64>()
    };
    assert!(distance(&wheel[0], &end[0]) < 0.05 * distance(&still[0], &end[0]));
}

#[test]
fn both_oscillators_have_their_own_octave_and_fine_tune() {
    for (oct, want) in [(-1.0, 110.0), (0.0, 220.0), (1.0, 440.0)] {
        // A only.
        let out = render(
            &mut processor(&merged(&[("osc_a_pos", 0.0), ("osc_a_oct", oct)])),
            48_000,
            &[(0, on(57, 100))],
            120.0,
        );
        let f = frequency(&out[0][4_800..], RATE);
        assert!((f - want).abs() < want * 0.01, "A {oct}: {f}");
        // B only: its tuning does not follow A's.
        let out = render(
            &mut processor(&merged(&[
                ("osc_mix", 1.0),
                ("osc_b_pos", 0.0),
                ("osc_b_fine", 0.0),
                ("osc_b_oct", oct),
                ("osc_a_oct", 2.0),
            ])),
            48_000,
            &[(0, on(57, 100))],
            120.0,
        );
        let f = frequency(&out[0][4_800..], RATE);
        assert!((f - want).abs() < want * 0.01, "B {oct}: {f}");
    }
    let out = render(
        &mut processor(&merged(&[("osc_a_pos", 0.0), ("osc_a_fine", 100.0)])),
        48_000,
        &[(0, on(57, 100))],
        120.0,
    );
    let f = frequency(&out[0][4_800..], RATE);
    assert!((f - 233.08).abs() < 2.0, "+100 ct: {f}");
}

#[test]
fn every_supported_sample_rate_plays_in_tune() {
    for rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0] {
        let mut p = processor_at(&merged(&[("osc_a_pos", 0.0)]), rate);
        let n = rate as usize;
        let out = render_at(&mut p, n, &[(0, on(69, 100))], rate);
        let f = frequency(&out[0][n / 10..], rate);
        assert!((f - 440.0).abs() < 4.0, "{rate}: {f}");
    }
}

// ------------------------------------------------------------------------------------------------
// Realtime, determinism

#[test]
fn nothing_allocates_and_everything_stays_finite_while_tables_switch() {
    let mut p = processor(&[("unison", 7.0), ("mod1_src", 8.0), ("mod1_dst", 3.0)]);
    let chord: Vec<(u32, EventKind)> = (0..16)
        .map(|i| (i * 17, on(30 + 5 * i as u8, 120)))
        .collect();
    render(&mut p, 2_048, &chord, 120.0);
    for round in 0..64u32 {
        let events = [
            (
                0,
                EventKind::Param {
                    id: wire("osc_a_table"),
                    value: f64::from(round % 16),
                },
            ),
            (
                100,
                EventKind::Param {
                    id: wire("osc_b_pos"),
                    value: f64::from(round % 5) * 0.25,
                },
            ),
            (300, on(20 + (round * 7 % 90) as u8, 127)),
        ];
        let out = render(&mut p, 512, &events, 120.0);
        assert!(out[0].iter().chain(&out[1]).all(|s| s.is_finite()));
        assert!(peak(&out[0]) < 8.0);
    }
}

#[test]
fn the_output_does_not_depend_on_how_the_host_splits_the_blocks() {
    let changes = [
        ("osc_a_table", table::CHOIR),
        ("mod1_src", src::LFO1 as f64),
        ("mod1_dst", dst::PW_A as f64),
        ("mod1_amt", 0.6),
        ("unison", 3.0),
    ];
    let events = [(0, on(48, 100)), (700, on(55, 90)), (3_000, off(48))];
    let whole = render(&mut processor(&changes), 6_000, &events, 120.0);
    for block in [1usize, 64, 128, 333] {
        let mut p = processor(&changes);
        let mut out = Vec::new();
        let mut done = 0;
        while done < 6_000 {
            let n = block.min(6_000 - done);
            let here: Vec<(u32, EventKind)> = events
                .iter()
                .filter(|(o, _)| (done..done + n).contains(&(*o as usize)))
                .map(|(o, k)| (*o - done as u32, *k))
                .collect();
            out.extend(render(&mut p, n, &here, 120.0)[0].iter().copied());
            done += n;
        }
        let err = whole[0]
            .iter()
            .zip(&out)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 1e-5, "block {block}: {err}");
    }
}

// ------------------------------------------------------------------------------------------------
// Presets and state

#[test]
fn the_presets_load_sound_and_move_the_table() {
    let list = presets::presets();
    assert_eq!(list.len(), 24);
    let mut names: Vec<&str> = list.iter().map(|p| p.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 24);
    let mut categories: Vec<&str> = list.iter().filter_map(|p| p.category).collect();
    categories.dedup();
    assert_eq!(
        categories,
        ["Bass", "Tasten", "Pad", "Lead", "Pluck", "Ambient & Glitch"]
    );
    for c in &categories {
        assert_ne!(komponist_device_core::category_text(Lang::En, c), "");
    }
    for (index, preset) in list.iter().enumerate() {
        for (key, value) in preset.changes {
            let wire = SPEC
                .wire(&ParamId::new(*key))
                .unwrap_or_else(|| panic!("{}: unknown parameter {key}", preset.name));
            let info = &SPEC.params()[wire as usize];
            assert_eq!(
                info.snap(*value),
                *value,
                "{}: {key} = {value}",
                preset.name
            );
        }
        let state = presets::state(index).unwrap();
        let values = SPEC.decode_state(Some(&state)).unwrap();
        assert_eq!(Some(values.clone()), SPEC.preset_values(index));
        let mut p = InternalProcessor::from_dsp(&SPEC, &values, crate::dsp());
        p.activate(RATE, BLOCK).unwrap();
        let chord = [
            (0, on(45, 100)),
            (0, on(52, 100)),
            (0, on(57, 110)),
            (0, on(64, 90)),
        ];
        let out = render(&mut p, 48_000, &chord, 120.0);
        let (level, top) = (rms(&out[0][2_000..40_000]), peak(&out[0]));
        assert!(level > 0.01, "{}: silent ({level})", preset.name);
        assert!(top < 1.5, "{}: peak {top}", preset.name);
        assert!(out[0].iter().chain(&out[1]).all(|s| s.is_finite()));
    }
    assert!(presets::state(24).is_none());
}

#[test]
fn a_state_round_trips_and_an_unknown_table_falls_back_to_the_last() {
    let mut values = SPEC.defaults();
    values[wire("osc_a_table") as usize] = table::GLASS;
    values[wire("osc_b_table") as usize] = table::RANDOM;
    values[wire("osc_a_pos") as usize] = 0.37;
    values[wire("osc_a_oct") as usize] = -2.0;
    values[wire("osc_a_fine") as usize] = 12.0;
    values[wire("mod2_dst") as usize] = dst::PW_B as f64;
    let state = SPEC.encode_state(&values);
    assert_eq!(SPEC.decode_state(Some(&state)).unwrap(), values);
    // A project from a later version with more tables: the index is held inside the list.
    let text = String::from_utf8(state).unwrap().replace(
        "\"osc_a_table\":12",
        &format!("\"osc_a_table\":{}", TABLES.len() + 3),
    );
    let decoded = SPEC.decode_state(Some(text.as_bytes())).unwrap();
    assert_eq!(
        decoded[wire("osc_a_table") as usize],
        (TABLES.len() - 1) as f64
    );
}

#[test]
fn a_project_renders_the_same_every_time() {
    let changes = [
        ("analog", 1.0),
        ("osc_a_table", table::RANDOM),
        ("unison", 4.0),
    ];
    let events = [(0, on(50, 100)), (0, on(57, 100))];
    let a = render(&mut processor(&changes), 9_600, &events, 120.0);
    let b = render(&mut processor(&changes), 9_600, &events, 120.0);
    assert_eq!(a, b);
}
