//! volt's tests: parameters and layout, the oscillators' aliasing, the filter against the graph's
//! curve, envelopes, LFOs, the matrix, unison, glide, stealing, the wheels, realtime safety and
//! determinism. Rendering goes through the real [`InternalProcessor`].

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, Processor, StereoIo, TransportInfo,
};
use komponist_i18n::Lang;
use komponist_model::params::layout::{BAND, ENV_VIEW, LFO_VIEW, SEL, validate};
use komponist_model::params::va_curve::{self, dst, src};
use komponist_model::params::{ParamContext, ParamId};
use komponist_model::{Meter, Tick, TickRange};

use super::filter::{Coefs, Filter};
use super::params::{COUNT, KEYS, id, params};
use super::*;
use komponist_device_core::InternalProcessor;

// Keep these device tests sensitive to realtime allocation after moving to their own binary.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub(super) const RATE: f32 = 48_000.0;
const BLOCK: usize = 4096;

pub(super) fn ctx(frames: usize, bpm: f64) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: bpm,
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

pub(super) fn off(key: u8) -> EventKind {
    EventKind::NoteOff {
        key,
        channel: 0,
        note_id: NOTE_ID_NONE,
    }
}

pub(super) fn bend(value: i32) -> EventKind {
    let raw = (value + 8192).clamp(0, 16383);
    EventKind::Midi([0xE0, (raw & 0x7F) as u8, (raw >> 7) as u8])
}

pub(super) fn cc(number: u8, value: u8) -> EventKind {
    EventKind::Midi([0xB0, number, value])
}

pub(super) fn pressure(value: u8) -> EventKind {
    EventKind::Midi([0xD0, value, 0])
}

/// The wire id of parameter `key`.
pub(super) fn wire(key: &str) -> u32 {
    SPEC.wire(&ParamId::new(key)).expect(key)
}

/// A processor with the defaults, a few parameters changed, activated at [`RATE`].
pub(super) fn processor(changes: &[(&str, f64)]) -> InternalProcessor {
    let mut values = SPEC.defaults();
    for (key, value) in changes {
        values[wire(key) as usize] = *value;
    }
    let mut p = InternalProcessor::from_dsp(&SPEC, &values, crate::dsp());
    p.activate(RATE, BLOCK).unwrap();
    p
}

/// A clean oscillator: sine on A only, no sub, a wide open filter, no analog drift.
pub(super) const CLEAN: &[(&str, f64)] = &[
    ("osc_a_wave", 0.0),
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

pub(super) fn merged(extra: &[(&str, f64)]) -> Vec<(&'static str, f64)> {
    let mut all: Vec<(&'static str, f64)> = CLEAN.to_vec();
    for (key, value) in extra {
        let key: &'static str = KEYS.iter().find(|k| *k == key).copied().expect(key);
        all.retain(|(k, _)| *k != key);
        all.push((key, *value));
    }
    all
}

/// Renders `frames` in one call (events at their offsets, sorted), inside `assert_no_alloc`.
pub(super) fn render(
    p: &mut InternalProcessor,
    frames: usize,
    events: &[(u32, EventKind)],
    bpm: f64,
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
    assert_no_alloc(|| p.process(&ctx(frames, bpm), &list, &mut io));
    out
}

/// `render` in calls of at most `block` frames, the events cut at the call borders.
pub(super) fn render_split(
    p: &mut InternalProcessor,
    frames: usize,
    block: usize,
    events: &[(u32, EventKind)],
) -> [Vec<f32>; 2] {
    let mut out = [Vec::new(), Vec::new()];
    let mut done = 0;
    while done < frames {
        let n = block.min(frames - done);
        let here: Vec<(u32, EventKind)> = events
            .iter()
            .filter(|(o, _)| (done..done + n).contains(&(*o as usize)))
            .map(|(o, k)| (*o - done as u32, *k))
            .collect();
        let part = render(p, n, &here, 120.0);
        out[0].extend(part[0].iter());
        out[1].extend(part[1].iter());
        done += n;
    }
    out
}

pub(super) fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

pub(super) fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// In-place radix-2 FFT.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((angle * k as f64).cos(), (angle * k as f64).sin());
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// The power spectrum (one-sided, Hann window) of the first `n` samples.
pub(super) fn spectrum(x: &[f32], n: usize) -> Vec<f64> {
    let mut re: Vec<f64> = (0..n)
        .map(|i| {
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos();
            f64::from(x[i]) * w
        })
        .collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    (0..n / 2).map(|i| re[i] * re[i] + im[i] * im[i]).collect()
}

/// The frequency of the strongest bin, refined by a parabola.
pub(super) fn peak_hz(x: &[f32], n: usize) -> f64 {
    let s = spectrum(x, n);
    let (i, _) = s
        .iter()
        .enumerate()
        .skip(1)
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let (a, b, c) = (s[i - 1].ln(), s[i].ln(), s[i + 1].ln());
    let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
    (i as f64 + shift) * f64::from(RATE) / n as f64
}

fn hz(key: f64) -> f64 {
    440.0 * ((key - 69.0) / 12.0).exp2()
}

// ------------------------------------------------------------------------------------------------
// Parameters, layout, catalog

#[test]
fn the_parameter_table_matches_the_wire_ids_and_the_proposal() {
    let list = params(Lang::De);
    assert_eq!(list.len(), COUNT);
    for (i, info) in list.iter().enumerate() {
        assert_eq!(info.id.as_str(), KEYS[i], "wire id {i}");
    }
    assert_eq!(id::OSC_A_WAVE, 0);
    assert_eq!(id::CUTOFF, 10);
    assert_eq!(KEYS[id::VOLUME], "volume");
    assert_eq!(id::VOLUME, COUNT - 1);
    // 71 of the proposal minus the two view switches, which are context selections.
    assert_eq!(COUNT, 69);
    let get = |k: &str| list.iter().find(|p| p.id.as_str() == k).unwrap();
    assert_eq!(get("cutoff").default, 1200.0);
    assert_eq!(get("cutoff").range(), (20.0, 20_000.0));
    assert_eq!(get("volume").range(), (-60.0, 6.0));
    assert_eq!(get("unison").range(), (1.0, 7.0));
    assert_eq!(get("voices").default, 8.0);
    assert_eq!(get("mod1_src").steps(), 8);
    assert_eq!(get("mod1_dst").steps(), 12);
    assert_eq!(get("lfo1_div").steps(), 7);
    assert_eq!(get("lfo2_shape").default, 1.0);
}

#[test]
fn texts_read_like_the_mockups() {
    let list = params(Lang::De);
    let text = |k: &str, v: f64| list.iter().find(|p| p.id.as_str() == k).unwrap().format(v);
    assert_eq!(text("cutoff", 1200.0), "1.2 kHz");
    assert_eq!(text("osc_b_fine", 7.0), "7 ct");
    assert_eq!(text("osc_b_fine", -12.0), "\u{2212}12 ct");
    assert_eq!(text("osc_b_oct", 0.0), "0 Okt.");
    assert_eq!(text("bend", 2.0), "2 st");
    assert_eq!(text("osc_mix", 0.5), "50 %");
    assert_eq!(text("volume", -6.0), "\u{2212}6.0 dB");
    assert_eq!(text("glide", 0.001), "aus");
    assert_eq!(text("lfo1_fade", 0.001), "aus");
    assert_eq!(text("glide", 0.06), "60 ms");
    assert_eq!(text("amp_a", 0.003), "3.0 ms");
    assert_eq!(text("mod_d", 0.6), "600 ms");
    assert_eq!(text("lfo1_rate", 4.5), "4.5 Hz");
    assert_eq!(text("mod1_src", 6.0), "Modrad");
    assert_eq!(text("mod1_dst", 6.0), "Cutoff");
}

fn context(env: &str, lfo: &str) -> ParamContext {
    ParamContext::new()
        .with(BAND, 1)
        .with(SEL, 1)
        .with(ENV_VIEW, env)
        .with(LFO_VIEW, lfo)
}

#[test]
fn the_layout_validates_for_every_envelope_and_lfo_and_shows_every_parameter() {
    let kind = &SPEC;
    let known = |p: &ParamId| kind.wire(p).is_some();
    let mut shown = std::collections::BTreeSet::new();
    for env in ["amp", "filt", "mod"] {
        for lfo in ["1", "2"] {
            let ctx = context(env, lfo);
            let errors = validate(kind.layout(), &ctx, &known);
            assert!(errors.is_empty(), "{env} {lfo}: {errors:?}");
            for cell in kind.layout().pages.iter().flat_map(|p| &p.cells) {
                if let Ok(Some(id)) = cell.resolved(&ctx) {
                    shown.insert(id.as_str().to_owned());
                }
            }
        }
    }
    for info in kind.params() {
        // The rate knob shows the divider while its LFO is synced (the card swaps them).
        let divider = info.id.as_str().ends_with("_div");
        let rate = info.id.as_str().replace("_div", "_rate");
        assert!(
            shown.contains(info.id.as_str()) || (divider && shown.contains(&rate)),
            "{} is not on a page",
            info.id
        );
    }
    let pages: Vec<(&str, u8)> = kind
        .layout()
        .pages
        .iter()
        .map(|p| (p.name.as_str(), p.cols))
        .collect();
    assert_eq!(pages, [("Klang", 10), ("Hüllkurven", 8), ("Matrix", 8)]);
}

#[test]
fn volt_is_an_instrument_in_the_catalog() {
    let catalog = &SPEC.catalog;
    assert_eq!(catalog.name, NAME);
    assert_eq!(catalog.name, "Volt");
    assert_eq!(catalog.role, komponist_model::DeviceRole::Instrument);
    assert!(catalog.features.contains(&"synthesizer"));
    assert_eq!(catalog.plugin_ref().id, "volt");
}

// ------------------------------------------------------------------------------------------------
// Oscillators

/// Power outside the true harmonics, relative to the total, of `wave` at `fundamental_bin` bins of
/// an `n`-point FFT (a whole number of periods in the window, so every harmonic and every alias
/// falls into one bin; the Hann window leaks at most two bins).
fn alias_ratio(f: impl Fn(u32, u32) -> f32, fundamental_bin: usize, n: usize) -> f64 {
    let inc = ((fundamental_bin as f64 / n as f64) * 4_294_967_296.0) as u32;
    let mut phase = 0u32;
    let x: Vec<f32> = (0..n)
        .map(|_| {
            let y = f(phase, inc);
            phase = phase.wrapping_add(inc);
            y
        })
        .collect();
    let s = spectrum(&x, n);
    let harmonics = (n / 2 - 3) / fundamental_bin;
    let mut wanted = 0.0;
    let mut other = 0.0;
    for (bin, p) in s.iter().enumerate().skip(2) {
        let near = (1..=harmonics).any(|h| bin.abs_diff(h * fundamental_bin) <= 2);
        if near {
            wanted += p;
        } else {
            other += p;
        }
    }
    other / wanted
}

#[test]
fn the_band_limited_oscillators_alias_far_less_than_naive_ones() {
    let n = 16_384;
    // Bins 1431, 957, 478 and 239 are 4.2 kHz, 2.8 kHz, 1.4 kHz and 0.7 kHz at 48 kHz (C8, F7, F6
    // and F5): harmonics above 24 kHz fold back into the audible range. A 2-point PolyBLEP cannot
    // make a saw at C8 clean (every step is two samples wide); the thresholds are the measured
    // values with a margin of a few dB, and the gain over the naive wave.
    let limits: [(usize, f64, f64); 4] = [
        (1431, -20.0, -40.0),
        (957, -23.0, -40.0),
        (478, -27.0, -50.0),
        (239, -30.0, -55.0),
    ];
    for (bin, edge_db, triangle_db) in limits {
        for (wave, name, limit) in [
            (va_curve::SAW, "saw", edge_db),
            (va_curve::PULSE, "pulse", edge_db),
            (va_curve::TRIANGLE, "triangle", triangle_db),
        ] {
            let good = alias_ratio(|p, i| osc::sample(wave, p, i, 0.3), bin, n);
            let naive = alias_ratio(
                |p, _| {
                    va_curve::wave_naive(wave, f64::from(klang_dsp::phase::to_unit(p)), 0.3) as f32
                },
                bin,
                n,
            );
            let (good_db, naive_db) = (10.0 * good.log10(), 10.0 * naive.log10());
            assert!(
                good_db < limit,
                "{name} bin {bin}: {good_db:.1} dB (limit {limit})"
            );
            assert!(
                good_db < naive_db - 10.0,
                "{name} bin {bin}: {good_db:.1} dB against naive {naive_db:.1} dB"
            );
        }
    }
}

#[test]
fn the_oscillators_stay_in_range_and_the_sine_is_pure() {
    for wave in 0..4 {
        for inc in [1u32 << 20, 1 << 26, 1 << 29] {
            let mut phase = 12345u32;
            for _ in 0..5000 {
                let y = osc::sample(wave, phase, inc, 0.5);
                assert!((-1.3..=1.3).contains(&y), "{wave} {y}");
                phase = phase.wrapping_add(inc);
            }
        }
    }
    let n = 16_384;
    let r = alias_ratio(|p, i| osc::sample(va_curve::SINE, p, i, 0.5), 300, n);
    assert!(10.0 * r.log10() < -60.0);
}

// ------------------------------------------------------------------------------------------------
// Filter

/// Gain in dB of the filter for a sine at `hz` (small, so the clip is linear).
fn filter_gain_db(typ: usize, cutoff: f32, resonance: f32, hz: f32) -> f64 {
    let c = Coefs::new(typ, cutoff, resonance, 0.0, RATE);
    let mut f = Filter::default();
    let n = 24_000;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let phase = (f64::from(hz) * i as f64 / f64::from(RATE)).fract();
        let x = 0.01 * (std::f64::consts::TAU * phase).sin() as f32;
        out.push(f.tick(x, &c));
    }
    // The amplitude from the RMS: the peak of a sampled sine near Nyquist is off by dB.
    let tail = &out[n / 2..];
    20.0 * (rms(tail) * std::f64::consts::SQRT_2 / 0.01).log10()
}

#[test]
fn the_filter_follows_the_curve_the_graph_draws() {
    for typ in 0..5 {
        for (cutoff, resonance) in [
            (1000.0f32, 0.0f32),
            (1000.0, 0.3),
            (1000.0, 0.7),
            (4000.0, 0.5),
        ] {
            for hz in [60.0f32, 300.0, 700.0, 1000.0, 1500.0, 3000.0, 8000.0] {
                // Where the response is far below the noise floor of the measurement the dB value
                // does not matter.
                let curve = va_curve::filter_response_db(
                    f64::from(hz),
                    f64::from(cutoff),
                    f64::from(resonance),
                    typ,
                    f64::from(RATE),
                );
                if curve < -70.0 {
                    continue;
                }
                let measured = filter_gain_db(typ, cutoff, resonance, hz);
                assert!(
                    (measured - curve).abs() < 1.0,
                    "type {typ}, cutoff {cutoff}, res {resonance}, {hz} Hz: measured {measured:.2} dB, curve {curve:.2} dB"
                );
            }
        }
    }
}

#[test]
fn cutoff_and_resonance_do_what_they_say() {
    // −3 dB (SVF) at the cutoff, then 12 dB per octave; the ladder is 24.
    let at = |typ, hz| filter_gain_db(typ, 1000.0, 0.3, hz);
    assert!((at(1, 1000.0) + 3.0).abs() < 1.0);
    assert!(at(1, 2000.0) < -10.0 && at(1, 2000.0) > -15.0);
    assert!(at(0, 2000.0) < -20.0);
    // Resonance lifts the peak by more than 10 dB at 0.9.
    assert!(filter_gain_db(1, 1000.0, 0.9, 1000.0) > filter_gain_db(1, 1000.0, 0.3, 1000.0) + 10.0);
    assert!(filter_gain_db(0, 1000.0, 0.9, 1000.0) > filter_gain_db(0, 1000.0, 0.3, 1000.0) + 3.0);
}

#[test]
fn the_ladder_rings_by_itself_at_full_resonance_and_stays_bounded() {
    let c = Coefs::new(0, 800.0, 1.0, 0.0, RATE);
    let mut f = Filter::default();
    f.tick(0.5, &c);
    let mut tail = Vec::new();
    for i in 0..48_000 {
        let y = f.tick(0.0, &c);
        assert!(y.is_finite() && y.abs() < 4.0, "{y} at {i}");
        if i > 40_000 {
            tail.push(y);
        }
    }
    // It still rings after a second.
    assert!(peak(&tail) > 0.05, "{}", peak(&tail));
}

#[test]
fn the_filters_survive_wild_input_and_coefficients() {
    for typ in 0..5 {
        let mut f = Filter::default();
        for i in 0..50_000 {
            let cutoff = if i % 5 < 2 { 20.0 } else { 40_000.0 };
            let c = Coefs::new(typ, cutoff, 1.0, 1.0, RATE);
            let x = if i % 3 == 0 { 50.0 } else { -50.0 };
            let y = f.tick(x, &c);
            assert!(y.is_finite() && y.abs() < 500.0, "type {typ} {y} at {i}");
            if i % 32 == 0 {
                f.tidy();
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Envelopes

#[test]
fn envelope_times_are_what_the_knobs_say() {
    use super::env::{Adsr, Coefs, Stage};
    let steps = |seconds: f32| (seconds * RATE / 32.0).round() as usize;
    // Attack: a straight line to 1 in the time.
    let c = Coefs::new(0.1, 0.2, 0.5, 0.3, RATE, [1.0; 3]);
    let mut e = Adsr::default();
    e.gate_on();
    let mut n = 0usize;
    while e.stage == Stage::Attack {
        e.step(&c, None);
        n += 1;
    }
    assert!(n.abs_diff(steps(0.1)) <= 1, "attack {n} steps");
    // Decay: 99 % of the way to the sustain level in the time.
    let mut level = 1.0;
    for _ in 0..steps(0.2) {
        level = e.step(&c, None);
    }
    let expected = 0.5 + 0.5 * 0.01;
    assert!((level - expected).abs() < 0.01, "decay {level}");
    // Sustain holds, release falls 40 dB in its time and ends.
    for _ in 0..2000 {
        e.step(&c, None);
    }
    assert_eq!((e.stage, e.level), (Stage::Sustain, 0.5));
    e.gate_off();
    let mut level = 0.5;
    for _ in 0..steps(0.3) {
        level = e.step(&c, None);
    }
    assert!((level - 0.005).abs() < 0.001, "release {level}");
    for _ in 0..steps(0.3) {
        e.step(&c, None);
    }
    assert!(e.is_idle() && e.level == 0.0);
}

#[test]
fn a_new_gate_starts_from_the_current_level() {
    use super::env::{Adsr, Coefs};
    let c = Coefs::new(0.05, 0.2, 0.5, 0.3, RATE, [1.0; 3]);
    let mut e = Adsr::default();
    e.gate_on();
    for _ in 0..30 {
        e.step(&c, None);
    }
    e.gate_off();
    for _ in 0..30 {
        e.step(&c, None);
    }
    let before = e.level;
    e.gate_on();
    let after = e.step(&c, None);
    assert!(after > before && after - before <= c.attack + 1e-6);
}

#[test]
fn the_amplitude_envelope_shapes_the_note_in_the_render() {
    let mut p = processor(&merged(&[("amp_a", 0.2), ("amp_s", 1.0), ("amp_r", 0.1)]));
    let out = render(
        &mut p,
        48_000,
        &[(0, on(69, 100)), (24_000, off(69))],
        120.0,
    );
    // Rising over the first 200 ms: the quarter windows grow.
    let w = |from: usize, to: usize| rms(&out[0][from..to]);
    assert!(w(0, 2400) < w(4800, 7200) && w(4800, 7200) < w(9600, 12_000));
    assert!(w(12_000, 14_400) > 0.5 * w(20_000, 22_000));
    // Released at 0.5 s: 40 dB down after 0.1 s and gone after 0.3 s.
    assert!(w(24_000 + 4800, 24_000 + 5200) < 0.03 * w(20_000, 22_000));
    assert_eq!(peak(&out[0][24_000 + 14_400..]), 0.0);
}

// ------------------------------------------------------------------------------------------------
// LFO

#[test]
fn an_lfo_runs_at_its_rate_and_follows_the_tempo_when_synced() {
    use super::lfo::Lfo;
    let mut l = Lfo::new(1);
    l.restart();
    // 2 Hz: 750 control steps of 32 samples are one period of 24 000 samples... count zero
    // crossings of the sine over 4 s.
    let inc = Lfo::increment(2.0, RATE);
    let mut crossings = 0;
    let mut last = 0.0f32;
    for _ in 0..(4.0 * RATE / 32.0) as usize {
        let v = l.step(0, inc, 1.0, false);
        if last < 0.0 && v >= 0.0 {
            crossings += 1;
        }
        last = v;
    }
    assert!((7..=8).contains(&crossings), "{crossings}");
    // Sync: 1/4 at 120 bpm is 2 Hz, at 90 bpm 1.5 Hz; 1 bar at 120 is 0.5 Hz.
    let mut k = Volt::new();
    k.set_param(id::LFO1_SYNC, 1.0);
    k.set_param(id::LFO1_DIV, 4.0);
    k.set_tempo(120.0);
    assert!((k.lfo_rate_hz(0) - 2.0).abs() < 1e-6);
    k.set_tempo(90.0);
    assert!((k.lfo_rate_hz(0) - 1.5).abs() < 1e-6);
    k.set_param(id::LFO1_DIV, 2.0);
    k.set_tempo(120.0);
    assert!((k.lfo_rate_hz(0) - 0.5).abs() < 1e-6);
    // Free: the rate parameter.
    k.set_param(id::LFO1_SYNC, 0.0);
    k.set_param(id::LFO1_RATE, 7.5);
    assert!((k.lfo_rate_hz(0) - 7.5).abs() < 1e-6);
}

#[test]
fn lfo_shapes_fade_and_unipolar() {
    use super::lfo::Lfo;
    let inc = Lfo::increment(1.0, RATE);
    let range = |shape: usize, uni: bool| {
        let mut l = Lfo::new(5);
        l.restart();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..(2.5 * RATE / 32.0) as usize {
            let v = l.step(shape, inc, 1.0, uni);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo, hi)
    };
    for shape in 0..6 {
        let (lo, hi) = range(shape, false);
        assert!(
            lo >= -1.001 && hi <= 1.001 && hi - lo > 0.5,
            "shape {shape}: {lo} {hi}"
        );
        let (lo, hi) = range(shape, true);
        assert!(
            lo >= -0.001 && hi <= 1.001 && hi - lo > 0.25,
            "uni {shape}: {lo} {hi}"
        );
    }
    // Fade: starts at 0, full after the fade time.
    let mut l = Lfo::new(5);
    l.restart();
    let step = 32.0 / (0.5 * RATE);
    let first = l.step(3, inc, step, false);
    assert!(first.abs() <= step * 1.01);
    let mut v = 0.0;
    for _ in 0..(0.6 * RATE / 32.0) as usize {
        v = l.step(3, inc, step, false);
    }
    assert!(v.abs() > 0.95);
    // Random shapes hold or glide between values that change once per cycle.
    let mut l = Lfo::new(9);
    l.restart();
    let values: Vec<f32> = (0..3 * 1500).map(|_| l.step(4, inc, 1.0, false)).collect();
    let mut distinct = values.clone();
    distinct.dedup();
    assert!((2..=5).contains(&distinct.len()), "{}", distinct.len());
}

// ------------------------------------------------------------------------------------------------
// Matrix and the wheels

fn slot(n: usize, source: usize, dest: usize, amount: f64) -> Vec<(&'static str, f64)> {
    let name = |what: &str| -> &'static str {
        KEYS.iter()
            .find(|k| **k == format!("mod{n}_{what}"))
            .copied()
            .unwrap()
    };
    vec![
        (name("src"), source as f64),
        (name("dst"), dest as f64),
        (name("amt"), amount),
    ]
}

#[test]
fn the_matrix_routes_a_source_to_a_destination() {
    let n = 16_384;
    let pitch_of = |extra: &[(&str, f64)], events: &[(u32, EventKind)]| {
        let mut p = processor(&merged(extra));
        let mut all = vec![(0, on(69, 100))];
        all.extend_from_slice(events);
        let out = render(&mut p, 24_000, &all, 120.0);
        peak_hz(&out[0][4000..4000 + n], n)
    };
    // Nothing routed: 440 Hz.
    assert!((pitch_of(&[], &[]) - 440.0).abs() < 1.0);
    // The mod wheel at the top → pitch at 50 % = one octave up.
    let routed = slot(1, src::WHEEL, dst::PITCH, 0.5);
    let f = pitch_of(&routed, &[(0, cc(1, 127))]);
    assert!((f / 880.0 - 1.0).abs() < 0.01, "{f}");
    // Half the wheel: half the way (6 semitones).
    let f = pitch_of(&routed, &[(0, cc(1, 64))]);
    assert!(
        (f / (440.0 * (6.0f64 / 12.0).exp2() * 1.0) - 1.0).abs() < 0.02,
        "{f}"
    );
    // Negative amounts go down.
    let f = pitch_of(&slot(2, src::WHEEL, dst::PITCH, -0.5), &[(0, cc(1, 127))]);
    assert!((f / 220.0 - 1.0).abs() < 0.01, "{f}");
    // A slot with the destination "Aus" or the source "Aus" does nothing.
    let f = pitch_of(&slot(1, src::WHEEL, dst::OFF, 0.5), &[(0, cc(1, 127))]);
    assert!((f - 440.0).abs() < 1.0);
    let f = pitch_of(&slot(1, src::OFF, dst::PITCH, 0.5), &[(0, cc(1, 127))]);
    assert!((f - 440.0).abs() < 1.0);
    // The key as a source: A4 (69) is 9/60 above middle C; 100 % on the pitch = 24 st · 0.15.
    let f = pitch_of(&slot(1, src::KEY, dst::PITCH, 1.0), &[]);
    let expect = 440.0 * (24.0 * 9.0 / 60.0 / 12.0f64).exp2();
    assert!((f / expect - 1.0).abs() < 0.01, "{f} vs {expect}");
}

#[test]
fn velocity_the_envelope_and_the_lfo_reach_their_destinations() {
    // Velocity → volume: louder at velocity 127 than at 40.
    let level = |vel: u8| {
        let mut p = processor(&merged(&slot(1, src::VELOCITY, dst::VOLUME, 1.0)));
        let out = render(&mut p, 12_000, &[(0, on(69, vel))], 120.0);
        rms(&out[0][6000..])
    };
    assert!(level(127) > 1.5 * level(40));
    // The third envelope → pitch: starts high, settles. Attack 1 ms, decay 200 ms, sustain 0.
    let extra = [("mod_a", 0.001), ("mod_d", 0.2), ("mod_s", 0.0)];
    let mut all = merged(&extra);
    all.extend(slot(1, src::ENV3, dst::PITCH, 0.5));
    let mut p = processor(&all);
    let out = render(&mut p, 48_000, &[(0, on(69, 100))], 120.0);
    let early = peak_hz(&out[0][0..2048], 2048);
    let late = peak_hz(&out[0][40_000..40_000 + 4096], 4096);
    assert!(early > 550.0, "{early}");
    assert!((late - 440.0).abs() < 8.0, "{late}");
    // LFO 1 → pitch makes the frequency move: a spread of energy around 440 Hz.
    let mut all = merged(&[("lfo1_rate", 6.0), ("lfo1_shape", 0.0)]);
    all.extend(slot(1, src::LFO1, dst::PITCH, 0.2));
    let mut p = processor(&all);
    let out = render(&mut p, 48_000, &[(0, on(69, 100))], 120.0);
    let s = spectrum(&out[0][4000..4000 + 16_384], 16_384);
    let bin = |f: f64| (f * 16_384.0 / f64::from(RATE)) as usize;
    let sideband: f64 = s[bin(480.0)..bin(560.0)].iter().sum();
    let carrier: f64 = s[bin(420.0)..bin(460.0)].iter().sum();
    assert!(
        sideband > 1e-3 * carrier,
        "no vibrato: {sideband} {carrier}"
    );
}

#[test]
fn the_cutoff_destination_darkens_and_opens_the_sound() {
    let bright = |extra: &[(&str, f64)]| {
        let mut base = merged(&[("osc_a_wave", 2.0), ("cutoff", 400.0)]);
        base.extend_from_slice(extra);
        let mut p = processor(&base);
        let out = render(&mut p, 12_000, &[(0, on(36, 100)), (0, cc(1, 127))], 120.0);
        let s = spectrum(&out[0][4000..4000 + 4096], 4096);
        // Above and below 1 kHz (bin 85 of 11.7 Hz).
        let low: f64 = s[1..85].iter().sum();
        let high: f64 = s[85..1024].iter().sum();
        high / low
    };
    let flat = bright(&[]);
    let mut up = slot(1, src::WHEEL, dst::CUTOFF, 0.6);
    let opened = bright(&up);
    up = slot(1, src::WHEEL, dst::CUTOFF, -0.6);
    let closed = bright(&up);
    assert!(opened > 10.0 * flat, "{opened} {flat}");
    assert!(closed < flat / 2.0, "{closed} {flat}");
}

#[test]
fn pitch_bend_the_mod_wheel_and_pressure_reach_the_voices() {
    let pitch = |extra: &[(&str, f64)], events: &[(u32, EventKind)]| {
        let mut p = processor(&merged(extra));
        let mut all = vec![(0u32, on(69, 100))];
        all.extend_from_slice(events);
        let out = render(&mut p, 20_000, &all, 120.0);
        peak_hz(&out[0][3000..3000 + 16_384], 16_384)
    };
    // Pitch bend: range 2 st by default, up and down and half way.
    assert!(
        (pitch(&[], &[(10, bend(8191))]) / (440.0 * (2.0f64 / 12.0).exp2()) - 1.0).abs() < 0.005
    );
    assert!(
        (pitch(&[], &[(10, bend(-8192))]) / (440.0 * (-2.0f64 / 12.0).exp2()) - 1.0).abs() < 0.005
    );
    assert!(
        (pitch(&[], &[(10, bend(4096))]) / (440.0 * (1.0f64 / 12.0).exp2()) - 1.0).abs() < 0.005
    );
    // The range follows the parameter: 12 st.
    assert!((pitch(&[("bend", 12.0)], &[(10, bend(-8192))]) / 220.0 - 1.0).abs() < 0.005);
    // Rest position and a bend of 0.
    assert!((pitch(&[], &[(10, bend(0))]) - 440.0).abs() < 1.0);
    // Mod wheel → vibrato: `wheel_vib` 200 ct with the wheel up spreads the pitch; wheel down: not.
    let vib = [
        ("wheel_vib", 200.0),
        ("lfo1_rate", 5.0),
        ("lfo1_shape", 0.0),
    ];
    let spread = |events: &[(u32, EventKind)]| {
        let mut p = processor(&merged(&vib));
        let mut all = vec![(0u32, on(69, 100))];
        all.extend_from_slice(events);
        let out = render(&mut p, 30_000, &all, 120.0);
        let s = spectrum(&out[0][4000..4000 + 16_384], 16_384);
        let bin = |f: f64| (f * 16_384.0 / f64::from(RATE)) as usize;
        let side: f64 = s[bin(300.0)..bin(430.0)].iter().sum::<f64>()
            + s[bin(450.0)..bin(600.0)].iter().sum::<f64>();
        let main: f64 = s[bin(430.0)..bin(450.0)].iter().sum();
        side / main
    };
    assert!(spread(&[(0, cc(1, 127))]) > 20.0 * spread(&[]));
    // Pressure → cutoff: with the path on, the pressure opens the filter.
    let bright = |events: &[(u32, EventKind)]| {
        let extra = [
            ("osc_a_wave", 2.0),
            ("cutoff", 500.0),
            ("at_cut", 0.8),
            ("key_amount", 0.0),
        ];
        let mut p = processor(&merged(&extra));
        let mut all = vec![(0u32, on(48, 100))];
        all.extend_from_slice(events);
        let out = render(&mut p, 12_000, &all, 120.0);
        let s = spectrum(&out[0][4000..4000 + 4096], 4096);
        s[64..512].iter().sum::<f64>() / s[..64].iter().sum::<f64>()
    };
    assert!(bright(&[(0, pressure(127))]) > 5.0 * bright(&[]));
    // Pressure as a matrix source.
    let mut routed = merged(&[]);
    routed.extend(slot(1, src::PRESSURE, dst::PITCH, 0.5));
    let mut p = processor(&routed);
    let out = render(
        &mut p,
        20_000,
        &[(0, on(69, 100)), (10, pressure(127))],
        120.0,
    );
    let f = peak_hz(&out[0][3000..3000 + 16_384], 16_384);
    assert!((f / 880.0 - 1.0).abs() < 0.01, "{f}");
    // Other controllers and polyphonic aftertouch are ignored.
    let f = pitch(
        &[],
        &[(10, cc(7, 127)), (10, EventKind::Midi([0xA0, 69, 127]))],
    );
    assert!((f - 440.0).abs() < 1.0);
}

// ------------------------------------------------------------------------------------------------
// Unison, glide, voices

#[test]
fn unison_detunes_and_spreads() {
    let n = 16_384;
    let spec = |extra: &[(&str, f64)]| {
        let mut p = processor(&merged(extra));
        render(&mut p, 24_000, &[(0, on(69, 100))], 120.0)
    };
    // Three voices at ±100 ct: three lines, 440 Hz and 440 · 2^(±1/12).
    let out = spec(&[("unison", 3.0), ("detune", 100.0), ("spread", 0.0)]);
    let s = spectrum(&out[0][4000..4000 + n], n);
    let bin = |f: f64| (f * n as f64 / f64::from(RATE)).round() as usize;
    let line = |f: f64| s[bin(f) - 2..=bin(f) + 2].iter().sum::<f64>();
    let total: f64 = s.iter().sum();
    let up = 440.0 * (1.0f64 / 12.0).exp2();
    let down = 440.0 * (-1.0f64 / 12.0).exp2();
    for f in [440.0, up, down] {
        assert!(line(f) > 0.2 * total, "{f}: {}", line(f) / total);
    }
    // One voice: a single line. Mono unison has identical channels, spread makes them differ.
    let one = spec(&[("unison", 1.0), ("detune", 100.0)]);
    let s1 = spectrum(&one[0][4000..4000 + n], n);
    assert!(s1[bin(440.0) - 2..=bin(440.0) + 2].iter().sum::<f64>() > 0.9 * s1.iter().sum::<f64>());
    assert_eq!(out[0], out[1]);
    let wide = spec(&[("unison", 5.0), ("detune", 20.0), ("spread", 1.0)]);
    let diff: f64 = wide[0]
        .iter()
        .zip(&wide[1])
        .map(|(a, b)| f64::from((a - b).abs()))
        .sum();
    assert!(diff > 1.0, "{diff}");
    // The unison is as loud as one voice, give or take 3 dB (the sum is normalised).
    let loud = |u: f64| rms(&spec(&[("unison", u), ("detune", 14.0), ("spread", 0.6)])[0][6000..]);
    let ratio = loud(7.0) / loud(1.0);
    assert!((0.5..2.0).contains(&ratio), "{ratio}");
}

#[test]
fn glide_slides_for_the_time_and_arrives() {
    // Mono, 200 ms glide, C4 then E4 (60 → 64) after 0.3 s.
    let mut p = processor(&merged(&[("mode", 1.0), ("glide", 0.2), ("octave", 0.0)]));
    let out = render(
        &mut p,
        48_000,
        &[(0, on(60, 100)), (14_400, on(64, 100))],
        120.0,
    );
    let freq_at = |from: usize| peak_hz(&out[0][from..from + 4096], 4096);
    let (c4, e4) = (hz(60.0), hz(64.0));
    assert!((freq_at(8000) / c4 - 1.0).abs() < 0.01);
    // 100 ms into the glide it is between the two notes.
    let mid = freq_at(14_400 + 2400 - 2048);
    assert!(mid > c4 * 1.03 && mid < e4 * 0.99, "{mid}");
    // After the 200 ms it has arrived.
    assert!((freq_at(14_400 + 9600 + 3000) / e4 - 1.0).abs() < 0.01);
    // Without glide it jumps at once.
    let mut p = processor(&merged(&[("mode", 1.0)]));
    let out = render(
        &mut p,
        48_000,
        &[(0, on(60, 100)), (14_400, on(64, 100))],
        120.0,
    );
    assert!((peak_hz(&out[0][14_400 + 1500..14_400 + 1500 + 4096], 4096) / e4 - 1.0).abs() < 0.01);
}

#[test]
fn glide_legato_glides_only_while_a_key_is_held() {
    let extra = [("mode", 1.0), ("glide", 0.4), ("glide_legato", 1.0)];
    let mut p = processor(&merged(&extra));
    // Separate notes: no glide; overlapping notes: glide.
    let events = [
        (0, on(60, 100)),
        (9600, off(60)),
        (12_000, on(72, 100)),
        (24_000, off(72)),
        (26_000, on(60, 100)),
        (28_000, on(72, 100)),
    ];
    let out = render(&mut p, 60_000, &events, 120.0);
    let freq_at = |from: usize| peak_hz(&out[0][from..from + 2048], 2048);
    assert!(
        (freq_at(12_000 + 1500) / hz(72.0) - 1.0).abs() < 0.02,
        "separate notes jump"
    );
    let slid = freq_at(28_000 + 1500);
    assert!(slid < hz(72.0) * 0.9, "overlapping notes glide: {slid}");
}

#[test]
fn mono_keeps_a_note_stack_and_legato_keeps_the_envelope() {
    // Mono: 60, 64 held, release 64 → back to 60.
    let mut p = processor(&merged(&[("mode", 1.0)]));
    let events = [(0, on(60, 100)), (6000, on(64, 100)), (12_000, off(64))];
    let out = render(&mut p, 24_000, &events, 120.0);
    let freq_at = |from: usize| peak_hz(&out[0][from..from + 4096], 4096);
    assert!((freq_at(2000) / hz(60.0) - 1.0).abs() < 0.01);
    assert!((freq_at(8000) / hz(64.0) - 1.0).abs() < 0.01);
    assert!((freq_at(16_000) / hz(60.0) - 1.0).abs() < 0.01);
    // After the last key up, the note ends.
    let mut p = processor(&merged(&[("mode", 1.0), ("amp_r", 0.01)]));
    let out = render(
        &mut p,
        24_000,
        &[
            (0, on(60, 100)),
            (4000, on(64, 100)),
            (8000, off(64)),
            (9000, off(60)),
        ],
        120.0,
    );
    assert_eq!(peak(&out[0][12_000..]), 0.0);
    // Legato does not restart the envelope, mono does: with a short decay to a low sustain the
    // second key is as quiet as the first in legato and loud again in mono.
    let env = [("amp_a", 0.001), ("amp_d", 0.05), ("amp_s", 0.2)];
    let after_second = |mode: f64| {
        let mut all = merged(&env);
        all.push(("mode", mode));
        let mut p = processor(&all);
        let out = render(
            &mut p,
            36_000,
            &[(0, on(60, 100)), (24_000, on(64, 100))],
            120.0,
        );
        (rms(&out[0][20_000..24_000]), rms(&out[0][24_000..26_400]))
    };
    let (before, legato) = after_second(2.0);
    assert!(legato < 1.3 * before, "legato {legato} vs {before}");
    let (before, mono) = after_second(1.0);
    assert!(mono > 2.0 * before, "mono {mono} vs {before}");
}

#[test]
fn poly_steals_the_quietest_released_voice_then_the_oldest() {
    let mut k = Volt::new();
    k.activate(RATE);
    k.set_param(id::VOICES, 3.0);
    for key in [60, 62, 64] {
        k.note_on(key, 100);
    }
    assert_eq!(k.sounding_keys(), [60, 62, 64]);
    // A fourth key: the oldest held voice goes.
    k.note_on(66, 100);
    let mut keys = k.sounding_keys();
    keys.sort_unstable();
    assert_eq!(keys, [62, 64, 66]);
    // A released voice goes before a held one: release 62, then a new key takes its place.
    k.note_off(62);
    k.note_on(67, 100);
    let mut keys = k.sounding_keys();
    keys.sort_unstable();
    assert_eq!(keys, [64, 66, 67]);
    // The same key twice does not take a second voice.
    k.note_on(67, 100);
    assert_eq!(k.sounding_keys().len(), 3);
    // Seven unison voices leave six keys; five leave nine; one all sixteen.
    for (unison, voices, expect) in [
        (7.0, 16.0, 6),
        (5.0, 16.0, 9),
        (3.0, 16.0, 16),
        (1.0, 16.0, 16),
        (1.0, 4.0, 4),
    ] {
        k.set_param(id::UNISON, unison);
        k.set_param(id::VOICES, voices);
        assert_eq!(k.max_poly(), expect, "unison {unison}");
    }
    k.set_param(id::UNISON, 7.0);
    k.set_param(id::VOICES, 16.0);
    k.reset();
    for key in 40..60 {
        k.note_on(key, 100);
    }
    assert_eq!(k.sounding_keys().len(), 6);
}

#[test]
fn all_notes_off_fades_quickly_and_reset_silences_at_once() {
    let mut p = processor(&merged(&[("amp_r", 5.0)]));
    let mut events = vec![(0, on(60, 100)), (0, on(64, 100))];
    events.push((12_000, EventKind::AllNotesOff));
    let out = render(&mut p, 24_000, &events, 120.0);
    assert!(rms(&out[0][10_000..12_000]) > 0.05);
    assert_eq!(peak(&out[0][12_000 + 2400..]), 0.0, "gone after 50 ms");
    let mut p = processor(&merged(&[("amp_r", 5.0)]));
    render(&mut p, 4800, &[(0, on(60, 100))], 120.0);
    p.reset();
    let out = render(&mut p, 4800, &[], 120.0);
    assert_eq!(peak(&out[0]), 0.0);
}

// ------------------------------------------------------------------------------------------------
// Realtime, safety, determinism

#[test]
fn nothing_allocates_and_every_value_stays_finite_under_abuse() {
    let mut p = processor(&[]);
    // Every parameter to both ends of its range in turn, notes, wheels, panic, reset, inside
    // `assert_no_alloc` (render() wraps every call).
    let count = SPEC.params().len() as u32;
    let mut rng = klang_dsp::rng::Rng::new(77);
    for round in 0..40 {
        let mut events: Vec<(u32, EventKind)> = Vec::new();
        for i in 0..24 {
            let offset = (i * 37 % 700) as u32;
            let kind = match rng.below(10) {
                0..=2 => on(30 + rng.below(70) as u8, 1 + rng.below(127) as u8),
                3 | 4 => off(30 + rng.below(70) as u8),
                5 => bend(rng.below(16384) as i32 - 8192),
                6 => cc(1, rng.below(128) as u8),
                7 => pressure(rng.below(128) as u8),
                _ => {
                    let wire = rng.below(count as usize) as u32;
                    let info = &SPEC.params()[wire as usize];
                    let (min, max) = info.range();
                    let value = if rng.below(2) == 0 { min } else { max };
                    EventKind::Param {
                        id: wire,
                        value: if rng.below(8) == 0 { f64::NAN } else { value },
                    }
                }
            };
            events.push((offset, kind));
        }
        events.sort_by_key(|e| e.0);
        let out = render(&mut p, 777, &events, 40.0 + 10.0 * f64::from(round));
        assert!(
            out[0].iter().chain(&out[1]).all(|s| s.is_finite()),
            "round {round}"
        );
        assert!(
            peak(&out[0]) < 20.0 && peak(&out[1]) < 20.0,
            "round {round}: {}",
            peak(&out[0])
        );
        if round % 9 == 8 {
            assert_no_alloc(|| p.reset());
        }
    }
    render(&mut p, 512, &[(0, EventKind::AllNotesOff)], 120.0);
}

#[test]
fn the_output_does_not_depend_on_how_the_host_splits_the_blocks() {
    let events = [
        (0, on(48, 100)),
        (100, on(55, 80)),
        (3000, bend(4000)),
        (3500, cc(1, 90)),
        (5000, off(48)),
        (9000, on(60, 120)),
        (12_001, pressure(100)),
        (15_000, off(55)),
    ];
    let setup = [
        ("unison", 3.0),
        ("spread", 0.7),
        ("analog", 0.8),
        ("lfo1_shape", 4.0),
        ("lfo2_shape", 5.0),
    ];
    let mut all = setup.to_vec();
    all.extend(slot(1, src::LFO1, dst::CUTOFF, 0.5));
    all.extend(slot(2, src::RANDOM, dst::PITCH, 0.05));
    all.extend(slot(3, src::LFO2, dst::PW_A, 0.5));
    let reference = {
        let mut p = processor(&all);
        render_split(&mut p, 24_000, 24_000, &events)
    };
    assert!(peak(&reference[0]) > 0.05);
    for block in [1usize, 7, 32, 33, 64, 100, 256, 1000] {
        let mut p = processor(&all);
        let out = render_split(&mut p, 24_000, block, &events);
        assert!(
            out[0]
                .iter()
                .zip(&reference[0])
                .all(|(a, b)| a.to_bits() == b.to_bits())
                && out[1]
                    .iter()
                    .zip(&reference[1])
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            "block {block}"
        );
    }
    // Two instances and a second thread give the same bits (what workers rely on).
    let threaded = std::thread::spawn({
        let all = all.clone();
        move || {
            let mut p = processor(&all);
            render_split(&mut p, 24_000, 256, &events)
        }
    })
    .join()
    .unwrap();
    assert!(
        threaded[0]
            .iter()
            .zip(&reference[0])
            .all(|(a, b)| a.to_bits() == b.to_bits())
    );
}

#[test]
fn analog_randomness_is_reproducible_but_differs_between_notes() {
    let render_two = || {
        let mut p = processor(&[("analog", 1.0), ("unison", 3.0)]);
        render(
            &mut p,
            24_000,
            &[(0, on(60, 100)), (12_000, on(60, 100))],
            120.0,
        )
    };
    let (a, b) = (render_two(), render_two());
    assert_eq!(a[0], b[0], "the same project renders the same");
    // Two notes of the same key get different random phases: the starts differ.
    let first = &a[0][2000..2400];
    let second = &a[0][14_000..14_400];
    assert_ne!(first, second);
}

#[test]
fn the_default_sound_has_a_sensible_level() {
    let mut p = processor(&[]);
    let out = render(&mut p, 24_000, &[(0, on(36, 100))], 120.0);
    let level = peak(&out[0]);
    assert!((0.1..1.0).contains(&level), "{level}");
    let mut loud = processor(&[
        ("unison", 7.0),
        ("detune", 40.0),
        ("spread", 1.0),
        ("resonance", 0.9),
        ("drive", 1.0),
        ("volume", 6.0),
    ]);
    let chord: Vec<_> = (0..8).map(|i| (0, on(48 + 4 * i, 127))).collect();
    let out = render(&mut loud, 24_000, &chord, 120.0);
    assert!(peak(&out[0]) < 8.0 && peak(&out[1]) < 8.0);
}

// ------------------------------------------------------------------------------------------------
// Presets

#[test]
fn the_ten_presets_load_and_sound() {
    let list = presets::presets();
    assert_eq!(list.len(), 10);
    let mut names: Vec<&str> = list.iter().map(|p| p.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 10);
    for (index, preset) in list.iter().enumerate() {
        // Every key exists, every value is inside its range.
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
        assert_ne!(values, SPEC.defaults(), "{}", preset.name);
        // A single note and a chord sound, finite and within reach of full scale.
        let mut p = InternalProcessor::from_dsp(&SPEC, &values, crate::dsp());
        p.activate(RATE, BLOCK).unwrap();
        let chord = [
            (0, on(40, 100)),
            (0, on(47, 100)),
            (0, on(52, 110)),
            (0, on(59, 90)),
        ];
        let out = render(&mut p, 36_000, &chord, 120.0);
        let (level, top) = (rms(&out[0][1_000..12_000]), peak(&out[0]));
        assert!(level > 0.005, "{}: silent ({level})", preset.name);
        assert!(top < 2.0, "{}: peak {top}", preset.name);
        assert!(out[0].iter().chain(&out[1]).all(|s| s.is_finite()));
    }
    assert!(presets::state(10).is_none());
}

#[test]
fn every_supported_sample_rate_plays_in_tune() {
    for rate in [22_050.0f32, 44_100.0, 48_000.0, 96_000.0, 192_000.0] {
        let mut values = SPEC.defaults();
        for (key, value) in merged(&[("lfo1_rate", 0.02)]) {
            values[wire(key) as usize] = value;
        }
        let mut p = InternalProcessor::from_dsp(&SPEC, &values, crate::dsp());
        p.activate(rate, BLOCK).unwrap();
        let frames = 16_384 + 4096;
        let input = [vec![0.0f32; frames], vec![0.0f32; frames]];
        let mut out = [vec![0.0f32; frames], vec![0.0f32; frames]];
        let [ol, or] = &mut out;
        let mut io = StereoIo {
            input: [&input[0], &input[1]],
            output: [ol, or],
        };
        let mut c = ctx(frames, 120.0);
        c.sample_rate = rate;
        let events = [Event {
            offset: 0,
            kind: on(69, 100),
        }];
        assert_no_alloc(|| p.process(&c, &events, &mut io));
        let s = spectrum(&out[0][4096..], 16_384);
        let (i, _) = s
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        let hz = i as f64 * f64::from(rate) / 16_384.0;
        assert!(
            (hz - 440.0).abs() <= f64::from(rate) / 16_384.0,
            "{rate} Hz: {hz}"
        );
    }
}

#[test]
fn stealing_and_retriggering_do_not_click() {
    // One voice, a sine: the second key takes the voice over while it sounds. The waveform stays
    // continuous (phase and level carry over), so no step is larger than the sine's own slope.
    let mut p = processor(&merged(&[
        ("voices", 1.0),
        ("amp_a", 0.001),
        ("amp_r", 0.02),
    ]));
    let events = [(0, on(60, 100)), (12_000, on(72, 100)), (24_000, off(72))];
    let out = render(&mut p, 36_000, &events, 120.0);
    let step = |from: usize, to: usize| {
        out[0][from..to]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };
    // The loudest step of a 523 Hz sine of this level, with some room.
    let level = peak(&out[0][8000..12_000]);
    let slope = level * std::f32::consts::TAU * 523.25 / RATE;
    assert!(
        step(11_900, 12_300) < 2.5 * slope,
        "{} vs {slope}",
        step(11_900, 12_300)
    );
    // Release and end of the note: no step beyond the slope either, and silence after.
    assert!(step(23_900, 28_000) < 2.5 * slope);
    assert_eq!(peak(&out[0][30_000..]), 0.0);
}

// ------------------------------------------------------------------------------------------------
// Golden render (#107): volt's voice is shared with surf; this pins volt's output bit for bit.

/// FNV-1a over the bits of both channels.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn bits_hash(out: &[Vec<f32>; 2]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in out[0].iter().chain(&out[1]) {
        for b in s.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Init and every preset, a chord with bend, wheel and pressure, then the release; with unison and
/// mono variants of Init. One hash each.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn golden_hashes() -> Vec<u64> {
    let events = [
        (0, on(40, 100)),
        (0, on(47, 90)),
        (300, on(52, 110)),
        (700, on(59, 70)),
        (6_000, cc(1, 90)),
        (9_000, bend(3_000)),
        (12_000, pressure(80)),
        (18_000, off(40)),
        (18_000, off(47)),
        (20_000, off(52)),
        (20_000, off(59)),
        (22_000, on(64, 127)),
        (26_000, off(64)),
    ];
    let mut setups: Vec<Vec<f64>> = vec![SPEC.defaults()];
    for (key, value) in [("unison", 5.0), ("mode", 2.0), ("glide", 0.2)] {
        let mut v = SPEC.defaults();
        v[wire(key) as usize] = value;
        if key == "glide" {
            v[wire("mode") as usize] = 1.0;
        }
        setups.push(v);
    }
    setups.extend((0..presets::presets().len()).map(|i| SPEC.preset_values(i).unwrap()));
    setups
        .iter()
        .map(|values| {
            let mut p = InternalProcessor::from_dsp(&SPEC, values, crate::dsp());
            p.activate(RATE, BLOCK).unwrap();
            bits_hash(&render(&mut p, 32_000, &events, 120.0))
        })
        .collect()
}

/// Recorded before the voice became generic over the oscillator (#107). `libm` differs between
/// platforms, so the hashes hold on Linux x86_64 only.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GOLDEN: &[u64] = &[
    0x383f1855d444e4d5,
    0x421301a646ae40fb,
    0x8b663e8295a4a765,
    0xdd1907f812726eb1,
    0x42bd62e5947bdfe9,
    0x612874d0d13f2e45,
    0x2a635e685574e479,
    0x4876f0ff1f301ec9,
    0xa26cf27dbd8725b8,
    0x1c6b776d8166fbbd,
    0x5830a2e5718ba971,
    0xea3a2863456e254d,
    0x09d1e2889d2c2bc9,
    0x81aa22dcebceb25d,
];

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn volt_renders_bit_for_bit_as_before_the_shared_voice() {
    let hashes = golden_hashes();
    assert_eq!(hashes, GOLDEN, "volt's output changed: {hashes:#x?}");
}
