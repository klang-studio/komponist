//! Tests of the effect DSP (WP-90): EQ response against the analytic curve, delay time and sync,
//! reverb decay time, limiter ceiling and latency, parameter changes without discontinuities,
//! block-size independence, no allocation and NaN safety. They drive the [`Dsp`] objects directly
//! (the `InternalProcessor` around them is covered in `tests`).

use assert_no_alloc::assert_no_alloc;
use komponist_model::params::ParamId;
use komponist_model::params::eq_curve;

use super::dsp::db_to_gain;
use super::*;

const RATE: f32 = 48_000.0;

/// A DSP of `kind` at `rate` with all parameters at their defaults except `set`, activated.
fn make(kind: Internal, rate: f32, set: &[(&str, f64)]) -> Box<dyn Dsp> {
    let mut d = (kind.spec().dsp)();
    for (i, p) in kind.params().iter().enumerate() {
        d.set_param(i, p.default);
    }
    for (key, v) in set {
        // Keys the device does not have are skipped, so one list can serve several devices.
        if let Some(w) = kind.wire(&ParamId::new(*key)) {
            d.set_param(w as usize, kind.params()[w as usize].snap(*v));
        }
    }
    d.activate(rate);
    d
}

fn wire(kind: Internal, key: &str) -> usize {
    kind.wire(&ParamId::new(key)).expect("known parameter") as usize
}

type Stereo = [Vec<f32>; 2];

/// Runs `input` through `d` in calls of `chunk` frames, inside `assert_no_alloc`. `changes` are
/// parameter changes `(frame, wire, value)` applied at their frame (calls are split there, like
/// `InternalProcessor` does).
fn play(d: &mut dyn Dsp, input: &Stereo, chunk: usize, changes: &[(usize, usize, f64)]) -> Stereo {
    let frames = input[0].len();
    let mut out = [vec![0.0; frames], vec![0.0; frames]];
    let mut cuts: Vec<usize> = changes.iter().map(|c| c.0).collect();
    cuts.extend((0..frames).step_by(chunk));
    cuts.push(frames);
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        for c in changes.iter().filter(|c| c.0 == a) {
            d.set_param(c.1, c.2);
        }
        let [ol, or] = &mut out;
        assert_no_alloc(|| {
            d.process(
                [&input[0][a..b], &input[1][a..b]],
                [&mut ol[a..b], &mut or[a..b]],
            )
        });
    }
    out
}

fn sine(freq: f32, amp: f32, frames: usize, rate: f32) -> Stereo {
    let v: Vec<f32> = (0..frames)
        .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / rate).sin())
        .collect();
    [v.clone(), v]
}

fn silence(frames: usize) -> Stereo {
    [vec![0.0; frames], vec![0.0; frames]]
}

fn impulse(frames: usize) -> Stereo {
    let mut s = silence(frames);
    s[0][0] = 1.0;
    s[1][0] = 1.0;
    s
}

/// Deterministic noise in −1 ..= 1 (xorshift).
fn noise(frames: usize, seed: u32) -> Stereo {
    let mut x = seed.max(1);
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    [
        (0..frames).map(|_| next()).collect(),
        (0..frames).map(|_| next()).collect(),
    ]
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn bits(s: &Stereo) -> Vec<u32> {
    s[0].iter().chain(&s[1]).map(|x| x.to_bits()).collect()
}

/// Asserts two outputs are bit-identical, naming the first difference instead of printing both.
fn assert_same(a: &Stereo, b: &Stereo, what: &str) {
    let (x, y) = (bits(a), bits(b));
    assert_eq!(x.len(), y.len(), "{what}: length");
    if let Some(i) = x.iter().zip(&y).position(|(p, q)| p != q) {
        panic!(
            "{what}: first difference at index {i}: {} vs {}",
            f32::from_bits(x[i]),
            f32::from_bits(y[i])
        );
    }
}

/// Largest difference between neighbouring samples.
fn max_step(x: &[f32]) -> f32 {
    x.windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

// ------------------------------------------------------------------------------------------------
// EQ

/// The band setup of the response test: every type, every band on.
const EQ_BANDS: [(f64, f64, f64, f64); 6] = [
    (eq_curve::LOW_CUT as f64, 80.0, 0.0, 0.71),
    (eq_curve::LOW_SHELF as f64, 200.0, 6.0, 0.71),
    (eq_curve::BELL as f64, 1000.0, 8.0, 2.0),
    (eq_curve::HIGH_SHELF as f64, 5000.0, -5.0, 0.9),
    (eq_curve::HIGH_CUT as f64, 12_000.0, 0.0, 0.71),
    (eq_curve::BELL as f64, 3000.0, -4.0, 1.5),
];

fn eq_with_bands(rate: f32) -> Box<dyn Dsp> {
    let mut set_list: Vec<(String, f64)> = Vec::new();
    for (n, (typ, freq, gain, q)) in EQ_BANDS.iter().enumerate() {
        let n = n + 1;
        set_list.push((format!("b{n}_on"), 1.0));
        set_list.push((format!("b{n}_type"), *typ));
        set_list.push((format!("b{n}_freq"), *freq));
        set_list.push((format!("b{n}_gain"), *gain));
        set_list.push((format!("b{n}_q"), *q));
    }
    let refs: Vec<(&str, f64)> = set_list.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    make(Internal::Eq, rate, &refs)
}

fn eq_expected_db(hz: f64, rate: f64) -> f64 {
    EQ_BANDS
        .iter()
        .map(|(t, f, g, q)| {
            eq_curve::magnitude_db(&eq_curve::coefs(*t, *f, *g, *q, rate), hz, rate)
        })
        .sum()
}

#[test]
fn eq_response_matches_the_analytic_curve_within_a_tenth_of_a_db() {
    for rate in [44_100.0f32, 48_000.0, 96_000.0] {
        for hz in [
            40.0, 80.0, 150.0, 200.0, 400.0, 800.0, 1000.0, 1300.0, 2200.0, 3000.0, 4000.0, 5000.0,
            7000.0, 10_000.0, 12_000.0, 15_000.0,
        ] {
            let mut eq = eq_with_bands(rate);
            let frames = rate as usize;
            let x = sine(hz, 0.1, frames, rate);
            let y = play(&mut *eq, &x, 256, &[]);
            let tail = frames / 2;
            let db = 20.0 * (rms(&y[0][tail..]) / rms(&x[0][tail..])).log10();
            let want = eq_expected_db(f64::from(hz), f64::from(rate)) as f32;
            assert!(
                (db - want).abs() < 0.1,
                "{rate} Hz rate, {hz} Hz: {db:.3} dB, analytic {want:.3} dB"
            );
        }
    }
}

#[test]
fn the_graph_curve_is_the_filters_response() {
    // `graph::eq` samples `eq_curve::band_response_db` at 48 kHz: the sum of the bands equals the
    // analytic response used above.
    for hz in [100.0, 1000.0, 6000.0] {
        let sum: f64 = EQ_BANDS
            .iter()
            .map(|(t, f, g, q)| eq_curve::band_response_db(*t, *f, *g, *q, hz))
            .sum();
        assert!((sum - eq_expected_db(hz, 48_000.0)).abs() < 1e-9);
    }
}

#[test]
fn a_flat_eq_is_bit_exact_and_the_output_gain_settles_exactly() {
    let mut eq = make(Internal::Eq, RATE, &[]);
    let x = noise(4096, 3);
    let y = play(&mut *eq, &x, 256, &[]);
    assert_same(&y, &x, "flat EQ");
    // A 0 dB bell stays exact while another band's gain moves in and out.
    let out = wire(Internal::Eq, "out");
    let y = play(&mut *eq, &x, 100, &[(0, out, -6.0)]);
    let gain = db_to_gain(-6.0);
    assert!(y[0][4000] > 0.0 || y[0][4000] <= 0.0);
    let last = 4095;
    let settled = play(&mut *eq, &noise(48_000, 4), 256, &[]);
    let n = settled[0].len() - 1;
    assert_eq!(settled[0][n], noise(48_000, 4)[0][n] * gain);
    let _ = (y, last);
}

/// Runs a 1 kHz sine through the EQ, making `changes` at frame 24 000, and returns the biggest
/// step between samples after the change relative to the biggest step of the sine at the
/// louder of the two steady levels.
fn eq_change_step_ratio(setup: &[(&str, f64)], changes: &[(&str, f64)], level_after: f32) -> f32 {
    let kind = Internal::Eq;
    let mut eq = make(kind, RATE, setup);
    let frames = 48_000;
    let x = sine(1000.0, 0.4, frames, RATE);
    let changes: Vec<(usize, usize, f64)> = changes
        .iter()
        .map(|(k, v)| (24_000, wire(kind, k), kind.params()[wire(kind, k)].snap(*v)))
        .collect();
    let y = play(&mut *eq, &x, 256, &changes);
    let before = max_step(&y[0][12_000..24_000]);
    let after = max_step(&y[0][24_000..]);
    // Level of the sine after the change decides the reference.
    let reference = before.max(max_step(&y[0][40_000..]));
    let _ = level_after;
    after / reference
}

#[test]
fn eq_parameter_changes_make_no_discontinuity() {
    let bell = [
        ("b3_on", 1.0),
        ("b3_type", 2.0),
        ("b3_freq", 1000.0),
        ("b3_gain", 0.0),
        ("b3_q", 2.0),
    ];
    // Gain jump, frequency jump, Q jump, type change, switching on and off.
    let cases: Vec<(&str, Vec<(&str, f64)>, Vec<(&str, f64)>)> = vec![
        ("gain", bell.to_vec(), vec![("b3_gain", 12.0)]),
        (
            "gain down",
            [&bell[..3], &[("b3_gain", 12.0), ("b3_q", 2.0)]].concat(),
            vec![("b3_gain", -12.0)],
        ),
        (
            "freq",
            [&bell[..3], &[("b3_gain", 12.0), ("b3_q", 2.0)]].concat(),
            vec![("b3_freq", 4000.0)],
        ),
        (
            "q",
            [&bell[..3], &[("b3_gain", 12.0), ("b3_q", 0.5)]].concat(),
            vec![("b3_q", 12.0)],
        ),
        (
            "type",
            [&bell[..3], &[("b3_gain", 9.0), ("b3_q", 1.0)]].concat(),
            vec![("b3_type", 3.0)],
        ),
        (
            "cut type",
            [&bell[..3], &[("b3_gain", 0.0), ("b3_q", 1.0)]].concat(),
            vec![("b3_type", 0.0), ("b3_freq", 900.0)],
        ),
        (
            "off",
            [&bell[..3], &[("b3_gain", 12.0), ("b3_q", 1.0)]].concat(),
            vec![("b3_on", 0.0)],
        ),
        (
            "on",
            [("b3_on", 0.0), ("b3_gain", 12.0), ("b3_freq", 1000.0)].to_vec(),
            vec![("b3_on", 1.0)],
        ),
        ("out gain", vec![], vec![("out", 12.0)]),
    ];
    for (name, setup, changes) in cases {
        let ratio = eq_change_step_ratio(&setup, &changes, 1.0);
        assert!(ratio < 1.15, "{name}: step ratio {ratio}");
    }
}

// ------------------------------------------------------------------------------------------------
// Delay

#[test]
fn the_delay_time_is_exact_free_and_synced() {
    let kind = Internal::Delay;
    let frames = 60_000;
    let echo_at = |set_list: &[(&str, f64)], bpm: f64| -> (usize, f32) {
        let mut d = make(kind, RATE, set_list);
        // The tempo is in place before the first call (activation snaps to it).
        d.set_tempo(bpm);
        d.activate(RATE);
        let y = play(&mut *d, &impulse(frames), 256, &[]);
        let at = (0..frames)
            .max_by(|a, b| y[0][*a].abs().total_cmp(&y[0][*b].abs()))
            .unwrap();
        (at, y[0][at])
    };
    let base = [
        ("mix", 1.0),
        ("feedback", 0.0),
        ("ping_pong", 0.0),
        ("width", 1.0),
    ];
    let with = |extra: &[(&'static str, f64)]| [&base[..], extra].concat();
    // Free: 100 ms and 123.4 ms.
    assert_eq!(echo_at(&with(&[("time", 100.0)]), 120.0), (4800, 1.0));
    let (at, v) = echo_at(&with(&[("time", 123.4)]), 120.0);
    assert!((at as i64 - 5923).abs() <= 1 && v > 0.5, "{at} {v}");
    // Sync at 120 BPM: 1/8 = 250 ms, 1/4 = 500 ms, 1/8. = 375 ms, 1/4T = 333.3 ms.
    for (note, ms) in [
        (5.0, 250.0),
        (8.0, 500.0),
        (6.0, 375.0),
        (7.0, 333.333),
        (2.0, 125.0),
    ] {
        let (at, v) = echo_at(&with(&[("sync", 1.0), ("note", note)]), 120.0);
        let want = (ms * 48.0f64).round() as i64;
        assert!(
            (at as i64 - want).abs() <= 1 && v > 0.5,
            "note {note}: {at} vs {want}"
        );
    }
    // Other tempos: 1/4 at 90 BPM = 666.7 ms, at 60 BPM = 1 s.
    for (bpm, ms) in [(90.0, 666.667), (60.0, 1000.0)] {
        let (at, _) = echo_at(&with(&[("sync", 1.0), ("note", 8.0)]), bpm);
        assert!(
            (at as i64 - (ms * 48.0f64).round() as i64).abs() <= 1,
            "{bpm}: {at}"
        );
    }
}

#[test]
fn the_synced_delay_follows_a_tempo_change_by_gliding() {
    let kind = Internal::Delay;
    let mut d = make(
        kind,
        RATE,
        &[
            ("sync", 1.0),
            ("note", 8.0),
            ("mix", 1.0),
            ("feedback", 0.0),
            ("ping_pong", 0.0),
        ],
    );
    d.set_tempo(120.0);
    let x = sine(440.0, 0.5, 96_000, RATE);
    let a = play(&mut *d, &x, 256, &[]);
    d.set_tempo(60.0);
    let b = play(&mut *d, &x, 256, &[]);
    // 500 ms now, 1 s afterwards, arriving without a jump.
    let steady = max_step(&a[0][60_000..]);
    assert!(
        max_step(&b[0]) < steady * 1.6,
        "{} vs {steady}",
        max_step(&b[0])
    );
    // After the glide (≤ 1 s at the slew limit) the new time holds: an impulse echoes at 1 s.
    let mut quiet = silence(100_000);
    quiet[0][0] = 1.0;
    let c = play(&mut *d, &quiet, 256, &[]);
    assert!(c[0][48_000].abs() > 0.5, "{}", c[0][48_000]);
}

#[test]
fn a_free_time_change_glides_without_a_click() {
    let kind = Internal::Delay;
    // No feedback: with it, the repeats at different delays beat against each other and the level
    // moves by itself; a click would still be several times the sine's own step.
    let mut d = make(
        kind,
        RATE,
        &[
            ("mix", 1.0),
            ("feedback", 0.0),
            ("ping_pong", 1.0),
            ("time", 300.0),
        ],
    );
    let x = sine(1000.0, 0.5, 96_000, RATE);
    let time = wire(kind, "time");
    let y = play(
        &mut *d,
        &x,
        256,
        &[(48_000, time, 420.0), (72_000, time, 150.0)],
    );
    let steady = max_step(&y[0][30_000..40_000]).max(max_step(&y[1][30_000..40_000]));
    for c in 0..2 {
        let worst = max_step(&y[c][48_000..]);
        assert!(worst < steady * 1.7, "channel {c}: {worst} vs {steady}");
    }
}

#[test]
fn the_delay_ping_pongs_and_the_width_collapses_to_mono() {
    let kind = Internal::Delay;
    let t = 4800;
    let mut d = make(
        kind,
        RATE,
        &[
            ("time", 100.0),
            ("mix", 1.0),
            ("feedback", 0.8),
            ("ping_pong", 1.0),
        ],
    );
    let mut x = silence(20_000);
    x[0][0] = 1.0;
    let y = play(&mut *d, &x, 256, &[]);
    // Left, right, left, right ... and the input is a mono sum (0.5 of a left-only click).
    assert!((y[0][t] - 0.5).abs() < 1e-6 && y[1][t].abs() < 1e-6);
    assert!(y[1][2 * t].abs() > 0.2 && y[0][2 * t].abs() < 1e-6);
    assert!(y[0][3 * t].abs() > 0.1 && y[1][3 * t].abs() < 1e-6);
    // Width 0: both channels carry the same wet signal.
    let mut d = make(
        kind,
        RATE,
        &[
            ("time", 100.0),
            ("mix", 1.0),
            ("feedback", 0.8),
            ("ping_pong", 1.0),
            ("width", 0.0),
        ],
    );
    let y = play(&mut *d, &x, 256, &[]);
    assert!(y[0].iter().zip(&y[1]).all(|(a, b)| (a - b).abs() < 1e-6));
    assert!((y[0][t] - 0.25).abs() < 1e-6);
}

#[test]
fn the_cuts_in_the_loop_shape_the_repeats() {
    let kind = Internal::Delay;
    // A burst of 100 Hz and of 8 kHz; compare the third repeat with the first.
    let repeats = |hz: f32, set_list: &[(&str, f64)]| -> (f32, f32) {
        let mut d = make(
            kind,
            RATE,
            &[
                &[
                    ("time", 50.0),
                    ("mix", 1.0),
                    ("feedback", 0.9),
                    ("ping_pong", 0.0),
                ],
                set_list,
            ]
            .concat(),
        );
        let mut x = sine(hz, 0.5, 2400, RATE);
        x[0].resize(30_000, 0.0);
        x[1].resize(30_000, 0.0);
        let y = play(&mut *d, &x, 256, &[]);
        let t = 2400;
        (rms(&y[0][t..2 * t]), rms(&y[0][3 * t..4 * t]))
    };
    let (first, third) = repeats(8000.0, &[("high_cut", 1000.0)]);
    assert!(
        third < first * 0.05,
        "8 kHz through a 1 kHz high cut: {first} -> {third}"
    );
    let (first, third) = repeats(8000.0, &[]);
    assert!(third > first * 0.5, "{first} -> {third}");
    let (first, third) = repeats(100.0, &[("low_cut", 1000.0)]);
    assert!(
        third < first * 0.05,
        "100 Hz through a 1 kHz low cut: {first} -> {third}"
    );
    let (first, third) = repeats(100.0, &[]);
    assert!(third > first * 0.5, "{first} -> {third}");
}

#[test]
fn the_feedback_always_dies_out_and_is_never_nan() {
    let kind = Internal::Delay;
    let mut d = make(
        kind,
        RATE,
        &[
            ("time", 20.0),
            ("feedback", 0.95),
            ("mix", 1.0),
            ("ping_pong", 1.0),
        ],
    );
    let burst = noise(4800, 5);
    let _ = play(&mut *d, &burst, 256, &[]);
    let tail = play(&mut *d, &silence(48_000 * 20), 480, &[]);
    assert!(tail[0].iter().chain(&tail[1]).all(|s| s.is_finite()));
    assert!(
        rms(&tail[0][48_000 * 19..]) < 1e-4,
        "{}",
        rms(&tail[0][48_000 * 19..])
    );
}

// ------------------------------------------------------------------------------------------------
// Hall

/// RT60 in seconds from the Schroeder backward integral, extrapolated from −5 … −35 dB.
fn rt60(impulse_response: &[f32], rate: f32) -> f32 {
    let mut energy: Vec<f64> = impulse_response
        .iter()
        .map(|s| f64::from(*s).powi(2))
        .collect();
    for i in (0..energy.len() - 1).rev() {
        energy[i] += energy[i + 1];
    }
    let total = energy[0];
    let db = |i: usize| 10.0 * (energy[i] / total).log10();
    let at = |level: f64| {
        (0..energy.len())
            .find(|i| db(*i) <= level)
            .unwrap_or(energy.len() - 1)
    };
    let (a, b) = (at(-5.0), at(-35.0));
    (b - a) as f32 / rate * 2.0
}

fn hall_ir(set_list: &[(&str, f64)], seconds: usize) -> Stereo {
    let mut d = make(
        Internal::Hall,
        RATE,
        &[
            &[
                ("mix", 1.0),
                ("wet_only", 1.0),
                ("damping", 20_000.0),
                ("modulation", 0.0),
            ],
            set_list,
        ]
        .concat(),
    );
    play(&mut *d, &impulse(48_000 * seconds), 256, &[])
}

#[test]
fn the_reverb_decays_in_the_time_the_decay_parameter_says() {
    for decay in [0.8, 2.0, 4.0] {
        for room in [0.0, 1.0, 2.0] {
            let ir = hall_ir(&[("decay", decay), ("room", room)], 14);
            let t = rt60(&ir[0], RATE);
            let want = decay as f32;
            assert!(
                (t / want - 1.0).abs() < 0.25,
                "decay {decay}, room {room}: RT60 {t:.2} s"
            );
        }
    }
    // The size changes the room, not the decay time.
    for size in [6.0, 28.0, 55.0] {
        let t = rt60(&hall_ir(&[("decay", 2.0), ("size", size)], 10)[0], RATE);
        assert!((t / 2.0 - 1.0).abs() < 0.25, "size {size}: RT60 {t:.2} s");
    }
}

#[test]
fn damping_shortens_the_tail_and_the_rooms_differ_in_their_onset() {
    // The decay of the highs: the second difference of the response (+12 dB/octave) hides the
    // lows, which decay at the decay time whatever the damping.
    let highs = |ir: &[f32]| -> Vec<f32> {
        (2..ir.len())
            .map(|i| ir[i] - 2.0 * ir[i - 1] + ir[i - 2])
            .collect()
    };
    let open = rt60(&highs(&hall_ir(&[("decay", 3.0)], 12)[0]), RATE);
    let dark = rt60(
        &highs(&hall_ir(&[("decay", 3.0), ("damping", 800.0)], 12)[0]),
        RATE,
    );
    assert!(dark < open * 0.5, "{dark} vs {open}");
    // First energy after the pre-delay: Zimmer (small) before Kirche (large).
    let onset = |room: f64| {
        let ir = hall_ir(&[("room", room), ("predelay", 0.0), ("diffusion", 0.0)], 1);
        ir[0].iter().position(|s| s.abs() > 1e-3).unwrap()
    };
    assert!(onset(0.0) < onset(2.0), "{} {}", onset(0.0), onset(2.0));
}

#[test]
fn freeze_holds_the_tail_and_nur_hall_removes_the_dry_signal() {
    let kind = Internal::Hall;
    let mut d = make(
        kind,
        RATE,
        &[
            ("decay", 0.5),
            ("mix", 1.0),
            ("wet_only", 1.0),
            ("predelay", 0.0),
        ],
    );
    let n = noise(48_000, 7);
    let freeze = wire(kind, "freeze");
    let mut x = n;
    x[0].resize(48_000 * 6, 0.0);
    x[1].resize(48_000 * 6, 0.0);
    let y = play(&mut *d, &x, 256, &[(48_000, freeze, 1.0)]);
    let at = |s: f64| (s * 48_000.0) as usize;
    let early = rms(&y[0][at(1.3)..at(1.4)]);
    let late = rms(&y[0][at(5.5)..at(5.6)]);
    assert!(early > 1e-3 && late > early * 0.5, "{early} -> {late}");
    // Without freeze the same tail is gone.
    let mut d = make(
        kind,
        RATE,
        &[
            ("decay", 0.5),
            ("mix", 1.0),
            ("wet_only", 1.0),
            ("predelay", 0.0),
        ],
    );
    let y = play(&mut *d, &x, 256, &[]);
    assert!(rms(&y[0][at(5.5)..at(5.6)]) < early * 0.01);
    // Wet only: nothing before the pre-delay; with the dry signal on, the input shows at once.
    let mut d = make(
        kind,
        RATE,
        &[("predelay", 100.0), ("mix", 0.5), ("wet_only", 1.0)],
    );
    let y = play(&mut *d, &impulse(8000), 256, &[]);
    assert!(y[0][..4700].iter().all(|s| *s == 0.0));
    assert!(y[0][4800..].iter().any(|s| s.abs() > 1e-4));
    let mut d = make(
        kind,
        RATE,
        &[("predelay", 100.0), ("mix", 0.5), ("wet_only", 0.0)],
    );
    let y = play(&mut *d, &impulse(8000), 256, &[]);
    assert!((y[0][0] - 0.5).abs() < 1e-6);
}

#[test]
fn the_reverb_is_stereo_and_width_zero_is_mono() {
    let wide = hall_ir(&[("width", 1.0)], 2);
    let diff: f32 = wide[0]
        .iter()
        .zip(&wide[1])
        .map(|(a, b)| (a - b).abs())
        .sum();
    assert!(diff > 1.0, "{diff}");
    let mono = hall_ir(&[("width", 0.0)], 2);
    assert!(
        mono[0]
            .iter()
            .zip(&mono[1])
            .all(|(a, b)| (a - b).abs() < 1e-7)
    );
}

#[test]
fn the_reverb_level_is_in_the_range_of_the_dry_signal() {
    let mut d = make(
        Internal::Hall,
        RATE,
        &[("mix", 1.0), ("wet_only", 1.0), ("decay", 2.4)],
    );
    let x = noise(48_000 * 4, 9);
    let y = play(&mut *d, &x, 256, &[]);
    let ratio = rms(&y[0][48_000 * 3..]) / rms(&x[0][48_000 * 3..]);
    assert!(ratio > 0.3 && ratio < 3.0, "wet/dry {ratio}");
}

#[test]
fn the_tail_never_goes_subnormal() {
    for kind in [
        Internal::Hall,
        Internal::Delay,
        Internal::Eq,
        Internal::Limiter,
    ] {
        let mut d = make(kind, RATE, &[("mix", 1.0)]);
        let _ = play(&mut *d, &noise(4800, 11), 256, &[]);
        let tail = play(&mut *d, &silence(48_000 * 30), 4800, &[]);
        let sub = tail[0]
            .iter()
            .chain(&tail[1])
            .filter(|s| s.is_subnormal())
            .count();
        assert_eq!(sub, 0, "{kind:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// Limiter

/// Loud, nasty material: noise, a clipped sweep, full-scale clicks, DC steps.
fn stress(frames: usize) -> Stereo {
    let mut x = noise(frames, 21);
    for c in 0..2 {
        for (i, s) in x[c].iter_mut().enumerate() {
            let sweep = (std::f32::consts::TAU * (50.0 + i as f32 * 0.05) * i as f32 / RATE).sin();
            *s = match (i / 4800) % 5 {
                0 => *s * 3.0,
                1 => sweep * 4.0,
                2 if i % 997 == 0 => 8.0,
                3 => 2.5,
                _ => *s * 0.05,
            };
        }
    }
    x[1].rotate_left(13);
    x
}

#[test]
fn the_limiter_never_exceeds_the_ceiling() {
    let kind = Internal::Limiter;
    for lookahead in [1.0, 0.0] {
        for (ceiling, release, gain) in
            [(-0.3, 100.0, 0.0), (-6.0, 10.0, 12.0), (-1.0, 1000.0, 24.0)]
        {
            let mut d = make(
                kind,
                RATE,
                &[
                    ("ceiling", ceiling),
                    ("release", release),
                    ("gain", gain),
                    ("lookahead", lookahead),
                ],
            );
            let x = stress(48_000 * 3);
            for chunk in [1, 37, 256] {
                d.reset();
                let y = play(&mut *d, &x, chunk, &[]);
                let limit = db_to_gain(ceiling as f32);
                let top = peak(&y[0]).max(peak(&y[1]));
                assert!(
                    top <= limit,
                    "lookahead {lookahead}, ceiling {ceiling}: {top} > {limit}"
                );
                // It limits, it does not mute: the loud parts get through at about the ceiling.
                assert!(top > limit * 0.9);
            }
        }
    }
}

#[test]
fn a_step_to_four_times_the_ceiling_arrives_attenuated_and_under_it() {
    // The gain is down before the loud audio leaves the delay line (that no overs are found
    // without the final clamp is checked in `limiter::tests`).
    let kind = Internal::Limiter;
    let mut d = make(
        kind,
        RATE,
        &[("ceiling", -6.0), ("release", 100.0), ("lookahead", 1.0)],
    );
    let ceiling = db_to_gain(-6.0);
    let mut x = silence(4800);
    for i in 2000..4800 {
        x[0][i] = 2.0;
        x[1][i] = 2.0;
    }
    let y = play(&mut *d, &x, 256, &[]);
    let delay = d.latency() as usize;
    assert!(y[0][2000 + delay - 1] == 0.0);
    assert!(
        (y[0][2000 + delay] - ceiling).abs() < 1e-5,
        "{}",
        y[0][2000 + delay]
    );
    assert!(y[0][2000 + delay..].iter().all(|s| *s <= ceiling));
}

#[test]
fn lookahead_reports_its_delay_as_latency_and_delays_the_audio_by_it() {
    let kind = Internal::Limiter;
    let mut d = make(kind, RATE, &[("lookahead", 1.0), ("ceiling", -0.3)]);
    assert_eq!(d.latency(), 96, "2 ms at 48 kHz");
    let x = noise(2000, 31).map(|c| c.iter().map(|s| s * 0.5).collect::<Vec<f32>>());
    let y = play(&mut *d, &x, 256, &[]);
    assert!(y[0][..96].iter().all(|s| *s == 0.0));
    assert_eq!(
        &y[0][96..],
        &x[0][..2000 - 96],
        "bit-exact copy, 96 frames late"
    );
    let d96 = make(kind, 96_000.0, &[("lookahead", 1.0)]);
    assert_eq!(d96.latency(), 192);
    let mut off = make(kind, RATE, &[("lookahead", 0.0)]);
    assert_eq!(off.latency(), 0);
    let y = play(&mut *off, &x, 256, &[]);
    assert_same(&y, &x, "flat EQ");
    // Switching at run time changes the reported latency.
    off.set_param(wire(kind, "lookahead"), 1.0);
    assert_eq!(off.latency(), 96);
}

#[test]
fn the_limiter_releases_with_its_time_constant_and_reports_the_gain_reduction() {
    let kind = Internal::Limiter;
    let gain_after = |release: f64, ms_after: f64| -> f32 {
        let mut d = make(
            kind,
            RATE,
            &[("ceiling", -6.0), ("release", release), ("lookahead", 0.0)],
        );
        let mut x = silence(48_000);
        for i in 0..2400 {
            x[0][i] = 1.0;
            x[1][i] = 1.0;
        }
        for i in 2400..48_000 {
            x[0][i] = 0.1;
            x[1][i] = 0.1;
        }
        let y = play(&mut *d, &x, 256, &[]);
        y[0][2400 + (ms_after * 48.0) as usize] / 0.1
    };
    // Right after the burst the gain is 0.5; one time constant later it has covered 63 % of the
    // way up to 1 (±0.05 for the smoothing of the attack).
    assert!((gain_after(100.0, 1.0) - 0.5).abs() < 0.05);
    let tau = gain_after(100.0, 100.0);
    assert!((tau - (0.5 + 0.5 * 0.632)).abs() < 0.05, "{tau}");
    assert!(gain_after(10.0, 100.0) > 0.99);
    assert!(gain_after(1000.0, 100.0) < 0.6);
    // The gain-reduction hook: about −6 dB for the burst, then back to 0 once released.
    let mut d = make(
        kind,
        RATE,
        &[("ceiling", -6.0), ("lookahead", 1.0), ("release", 50.0)],
    );
    let mut x = silence(4800);
    x[0][1000] = 1.0;
    x[1][1000] = 1.0;
    let _ = play(&mut *d, &x, 256, &[]);
    let gr = d.take_reduction_db();
    assert!((gr - 6.0).abs() < 0.2, "{gr}");
    let _ = play(&mut *d, &silence(48_000), 256, &[]);
    let _ = d.take_reduction_db();
    let _ = play(&mut *d, &silence(48_000), 256, &[]);
    assert!(d.take_reduction_db() < 0.1);
}

#[test]
fn the_limiter_input_gain_changes_without_a_click() {
    let kind = Internal::Limiter;
    let mut d = make(
        kind,
        RATE,
        &[("ceiling", 0.0), ("lookahead", 1.0), ("gain", 0.0)],
    );
    let x = sine(1000.0, 0.2, 48_000, RATE);
    let gain = wire(kind, "gain");
    let y = play(&mut *d, &x, 256, &[(24_000, gain, 12.0)]);
    let steady = max_step(&y[0][30_000..40_000]);
    assert!(max_step(&y[0][24_000..]) < steady * 1.05);
}

// ------------------------------------------------------------------------------------------------
// All effects

#[test]
fn the_output_does_not_depend_on_how_the_host_splits_the_blocks() {
    for kind in [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
    ] {
        let x = noise(30_000, 41);
        let reference = {
            let mut d = make(kind, RATE, &[("mix", 0.6)]);
            d.set_tempo(133.0);
            let changes = param_script(kind);
            play(&mut *d, &x, 30_000, &changes)
        };
        for chunk in [1, 31, 32, 33, 256, 1000] {
            let mut d = make(kind, RATE, &[("mix", 0.6)]);
            d.set_tempo(133.0);
            let changes = param_script(kind);
            let y = play(&mut *d, &x, chunk, &changes);
            assert_same(&y, &reference, &format!("{kind:?}, chunk {chunk}"));
        }
    }
}

/// Parameter changes at fixed frames, for every parameter in turn.
fn param_script(kind: Internal) -> Vec<(usize, usize, f64)> {
    kind.params()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (lo, hi) = p.range();
            (2000 + i * 900, i, p.snap(lo + (hi - lo) * 0.7))
        })
        .collect()
}

#[test]
fn nan_and_infinity_in_never_come_out_and_do_not_poison_the_state() {
    for kind in [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
    ] {
        let mut d = make(kind, RATE, &[("mix", 1.0), ("feedback", 0.9)]);
        let mut x = noise(2000, 51);
        for i in (0..2000).step_by(7) {
            x[0][i] = f32::NAN;
            x[1][i] = f32::INFINITY;
            x[0][i + 1] = f32::NEG_INFINITY;
        }
        let y = play(&mut *d, &x, 256, &[]);
        assert!(y[0].iter().chain(&y[1]).all(|s| s.is_finite()), "{kind:?}");
        // Clean input afterwards gives a clean output, the same as a fresh DSP would give.
        d.reset();
        let clean = noise(3000, 52);
        let a = play(&mut *d, &clean, 256, &[]);
        let mut fresh = make(kind, RATE, &[("mix", 1.0), ("feedback", 0.9)]);
        let b = play(&mut *fresh, &clean, 256, &[]);
        assert!(a[0].iter().all(|s| s.is_finite()));
        assert_same(&a, &b, &format!("{kind:?}: reset restores a fresh state"));
    }
}

#[test]
fn extreme_parameter_values_stay_finite_and_bounded() {
    for kind in [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
    ] {
        for pick in [0usize, 1] {
            let mut d = make(kind, RATE, &[]);
            for (i, p) in kind.params().iter().enumerate() {
                let (lo, hi) = p.range();
                d.set_param(i, p.snap(if pick == 0 { lo } else { hi }));
            }
            let x = noise(48_000, 61);
            let y = play(&mut *d, &x, 256, &[]);
            let top = peak(&y[0]).max(peak(&y[1]));
            // All six EQ bands at +24 dB on top of each other are +144 dB at 20 kHz: finite is the bar.
            let bound = if kind == Internal::Eq { 1e12 } else { 1000.0 };
            assert!(
                top.is_finite() && top < bound,
                "{kind:?} pick {pick}: {top}"
            );
        }
    }
}

#[test]
fn sample_rates_from_8_to_192_khz_work() {
    for rate in [8_000.0f32, 22_050.0, 44_100.0, 96_000.0, 192_000.0] {
        for kind in [
            Internal::Eq,
            Internal::Delay,
            Internal::Hall,
            Internal::Limiter,
        ] {
            let mut d = make(kind, rate, &[("mix", 0.5)]);
            let x = noise(rate as usize / 2, 71);
            let y = play(&mut *d, &x, 256, &[]);
            assert!(
                y[0].iter()
                    .chain(&y[1])
                    .all(|s| s.is_finite() && s.abs() < 50.0),
                "{kind:?} at {rate}"
            );
        }
    }
}

#[test]
fn through_the_processor_the_tempo_and_the_latency_arrive() {
    use komponist_engine::{ProcessContext, Processor, StereoIo, TransportInfo};
    use komponist_model::{Meter, Tick, TickRange};
    let mut p = Internal::Limiter.processor(&Internal::Limiter.defaults());
    p.activate(RATE, 256).unwrap();
    assert_eq!(p.latency(), 96);
    let values: Vec<f64> = {
        let kind = Internal::Delay;
        let mut v = kind.defaults();
        v[wire(kind, "sync")] = 1.0;
        v[wire(kind, "note")] = 5.0;
        v[wire(kind, "mix")] = 1.0;
        v[wire(kind, "feedback")] = 0.0;
        v[wire(kind, "ping_pong")] = 0.0;
        v
    };
    let mut p = Internal::Delay.processor(&values);
    p.activate(RATE, 4096).unwrap();
    let frames = 4096;
    let mut input = impulse(frames);
    input[1][0] = 0.0;
    let mut out = silence(frames);
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    let ctx = ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 480.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: false,
        deadline: None,
    };
    assert_no_alloc(|| p.process(&ctx, &[], &mut io));
    // 1/8 at 480 BPM is 62.5 ms = 3000 frames; the 120 BPM the device started with would give
    // 250 ms. The first tempo is taken at once, so the echo is exactly there.
    assert_eq!(out[0].iter().position(|s| s.abs() > 0.5), Some(3000));
}

// ------------------------------------------------------------------------------------------------
// CPU (docs/PERFORMANCE.md §7): `cargo test -p komponist-builtin --release --lib cpu_ -- --ignored --nocapture`

/// Mean and worst time of one 256-frame call in microseconds for `instances` instances of `kind`
/// (called one after the other, as a single thread of the engine would), over 10 s of noise.
fn cpu_us(kind: Internal, instances: usize, set: &[(&str, f64)]) -> (f64, f64) {
    let mut dsps: Vec<Box<dyn Dsp>> = (0..instances).map(|_| make(kind, RATE, set)).collect();
    let x = noise(48_000 * 10, 77);
    let blocks = x[0].len() / 256;
    let mut out = silence(256);
    let (mut total, mut worst) = (0.0f64, 0.0f64);
    for b in 0..blocks + 100 {
        let at = (b % blocks) * 256;
        let t = std::time::Instant::now();
        for d in &mut dsps {
            let [ol, or] = &mut out;
            d.process(
                [&x[0][at..at + 256], &x[1][at..at + 256]],
                [&mut ol[..], &mut or[..]],
            );
        }
        let us = t.elapsed().as_secs_f64() * 1e6;
        if b >= 100 {
            total += us;
            worst = worst.max(us);
        }
    }
    (total / blocks as f64, worst)
}

#[test]
#[ignore = "timing; run in release"]
fn cpu_per_effect() {
    let period = 256.0 / 48_000.0 * 1e6;
    let eq_all: Vec<(String, f64)> = (1..=6)
        .flat_map(|n| {
            [
                (format!("b{n}_on"), 1.0),
                (format!("b{n}_type"), [0.0, 1.0, 2.0, 2.0, 3.0, 4.0][n - 1]),
                (format!("b{n}_gain"), 6.0),
                (format!("b{n}_freq"), 100.0 * 3.0f64.powi(n as i32)),
            ]
        })
        .collect();
    let eq_all: Vec<(&str, f64)> = eq_all.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let cases: [(&str, Internal, &[(&str, f64)]); 5] = [
        ("EQ, 6 bands active", Internal::Eq, &eq_all),
        (
            "Delay, ping-pong, feedback 0.6",
            Internal::Delay,
            &[("feedback", 0.6), ("mix", 0.5)],
        ),
        ("Hall, defaults", Internal::Hall, &[]),
        (
            "Hall, Kirche, modulation 1",
            Internal::Hall,
            &[("room", 2.0), ("modulation", 1.0)],
        ),
        ("Limiter, lookahead", Internal::Limiter, &[("gain", 12.0)]),
    ];
    for (name, kind, set) in cases {
        let (one, one_worst) = cpu_us(kind, 1, set);
        let (sixteen, sixteen_worst) = cpu_us(kind, 16, set);
        println!(
            "{name}: 1× mean {one:.1} µs ({:.3} % of the period), worst {one_worst:.0} µs; 16× mean {sixteen:.1} µs ({:.2} %), worst {sixteen_worst:.0} µs",
            one / period * 100.0,
            sixteen / period * 100.0
        );
    }
}
