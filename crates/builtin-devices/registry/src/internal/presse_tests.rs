//! Tests of the compressor „presse“ (WP-93): the static curve against the formula, attack and
//! release time constants, program-dependent release, lookahead and latency, stereo link, the
//! sidechain high-pass, mix 0 bit-exact, telemetry, presets and layout. The generic effect tests
//! (block size, NaN, extremes, sample rates, no allocation, subnormals, CPU) include the device
//! in `fx_tests`.

#![allow(clippy::needless_range_loop)]

use komponist_model::params::comp_curve;

use super::fx_tests::{
    RATE, Stereo, assert_same, impulse, make, noise, peak, play, silence, sine, wire,
};
use super::*;

const KIND: Internal = Internal::Presse;

fn db(x: f32) -> f64 {
    20.0 * f64::from(x.abs().max(1e-9)).log10()
}

fn amp(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// A constant (DC) signal of level `level_db` on both channels: Peak and RMS detectors agree.
fn dc(level_db: f64, frames: usize) -> Stereo {
    [vec![amp(level_db); frames], vec![amp(level_db); frames]]
}

/// A compressor with plain settings: Peak, no auto-release, no lookahead, no filter.
fn plain(set: &[(&str, f64)]) -> Box<dyn Dsp> {
    let mut all = vec![
        ("detector", 0.0),
        ("auto_release", 0.0),
        ("attack", 0.1),
        ("mix", 1.0),
    ];
    all.extend_from_slice(set);
    make(KIND, RATE, &all)
}

#[test]
fn the_static_curve_matches_the_formula_within_a_tenth_of_a_db() {
    let mut worst = 0.0f64;
    for detector in [0.0, 1.0] {
        for threshold in [-30.0, -18.0, -9.0] {
            for ratio in [1.5, 2.0, 4.0, 20.0] {
                for knee in [0.0, 6.0, 12.0, 24.0] {
                    let mut d = plain(&[
                        ("detector", detector),
                        ("threshold", threshold),
                        ("ratio", ratio),
                        ("knee", knee),
                    ]);
                    for level in [-50.0, -30.0, -20.0, -12.0, -6.0, 0.0] {
                        d.reset();
                        let x = dc(level, 24_000);
                        let y = play(&mut *d, &x, 256, &[]);
                        let got = db(y[0][23_999]) - level;
                        let want = comp_curve::reduction_db(level, threshold, ratio, knee);
                        worst = worst.max((got - want).abs());
                        assert!(
                            (got - want).abs() < 0.1,
                            "detector {detector}, T {threshold}, R {ratio}, W {knee}, level {level}: {got:.3} dB, formula {want:.3} dB"
                        );
                    }
                }
            }
        }
    }
    println!("worst deviation from the formula: {worst:.4} dB");
}

/// The gain in dB at every frame from `from` of a DC input (output over input).
fn gain_curve(y: &[f32], level_db: f64, from: usize) -> Vec<f64> {
    y[from..].iter().map(|s| db(*s) - level_db).collect()
}

#[test]
fn attack_and_release_are_time_constants_in_the_db_domain() {
    for ms in [10.0f64, 50.0] {
        // −6 dB over a −30 dB threshold at 4 : 1 (hard knee) is −18 dB of reduction.
        let (t, loud, quiet) = (-30.0, -6.0, -40.0);
        let want = comp_curve::reduction_db(loud, t, 4.0, 0.0);
        assert!((want + 18.0).abs() < 1e-9);
        let set = [
            ("threshold", t),
            ("ratio", 4.0),
            ("knee", 0.0),
            ("attack", ms),
            ("release", ms * 4.0),
        ];
        let mut d = plain(&set);
        // Silence first so the reduction starts at 0, then the loud step, then the quiet one.
        let frames = (RATE as f64 * ms / 1000.0 * 12.0) as usize;
        let mut x = silence(0);
        for c in 0..2 {
            x[c].extend(vec![amp(quiet); 480]);
            x[c].extend(vec![amp(loud); frames]);
            x[c].extend(vec![amp(quiet); frames]);
        }
        let y = play(&mut *d, &x, 256, &[]);
        // Attack: 63.2 % of the final reduction after one time constant.
        let tau = (RATE as f64 * ms / 1000.0) as usize;
        let attack = gain_curve(&y[0][480..480 + frames], loud, 0);
        let at_tau = attack[tau - 1] / want;
        assert!(
            (at_tau - 0.632).abs() < 0.02,
            "attack {ms} ms: {at_tau:.3} of the final reduction after one τ"
        );
        // The release has the time constant of its own knob (4 × the attack here).
        let tau_r = 4 * tau;
        let release = gain_curve(&y[0][480 + frames..], quiet, 0);
        let left = release[tau_r - 1] / want;
        assert!(
            (left - 0.368).abs() < 0.02,
            "release {} ms: {left:.3} of the reduction left after one τ",
            ms * 4.0
        );
    }
}

/// Frames until the reduction has recovered to half after the loud part of `loud_ms` ms.
fn half_recovery(loud_ms: usize, auto: f64) -> usize {
    let mut d = plain(&[
        ("threshold", -30.0),
        ("ratio", 8.0),
        ("knee", 0.0),
        ("attack", 5.0),
        ("release", 100.0),
        ("auto_release", auto),
    ]);
    let loud = (RATE as usize) * loud_ms / 1000;
    let quiet = 48_000;
    let mut x = silence(0);
    for c in 0..2 {
        x[c].extend(vec![amp(-6.0); loud]);
        x[c].extend(vec![amp(-60.0); quiet]);
    }
    let y = play(&mut *d, &x, 256, &[]);
    let start = loud;
    let depth = db(y[0][start - 1]) + 6.0;
    (0..quiet)
        .find(|i| (db(y[0][start + i]) + 60.0) > depth / 2.0)
        .expect("recovers")
}

#[test]
fn auto_release_recovers_fast_after_a_peak_and_slowly_after_a_long_loud_passage() {
    let (short, long) = (half_recovery(15, 1.0), half_recovery(2000, 1.0));
    assert!(
        long > 3 * short,
        "a long passage must recover much slower: short {short} frames, long {long} frames"
    );
    // Without it the recovery time does not depend on the length of the passage (the release is
    // the one knob), so the long one is not slower than the same release alone.
    let (short_off, long_off) = (half_recovery(15, 0.0), half_recovery(2000, 0.0));
    assert!(
        (long_off as f64 / short_off as f64) < 1.3,
        "without auto-release: {short_off} vs {long_off}"
    );
    // With auto-release a short peak recovers faster than the plain release of the same knob.
    assert!(short < short_off, "{short} vs {short_off}");
}

/// A burst of a 1 kHz sine of full scale after silence.
fn burst(frames: usize) -> Stereo {
    let mut x = silence(4800);
    let s = sine(1000.0, 1.0, frames, RATE);
    for c in 0..2 {
        x[c].extend(&s[c]);
    }
    x
}

#[test]
fn lookahead_does_not_overshoot_the_curve_and_reports_its_delay() {
    let set = [
        ("threshold", -30.0),
        ("ratio", 20.0),
        ("knee", 0.0),
        ("attack", 0.1),
        ("release", 300.0),
    ];
    let x = burst(24_000);
    let mut with = set.to_vec();
    with.push(("lookahead", 1.0));
    let mut d_on = plain(&with);
    let mut d_off = plain(&set);
    assert_eq!(d_on.latency(), 144, "3 ms at 48 kHz");
    assert_eq!(d_off.latency(), 0);
    let on = play(&mut *d_on, &x, 256, &[]);
    let off = play(&mut *d_off, &x, 256, &[]);
    // The curve's output for a full-scale peak.
    let expected = comp_curve::output_db(0.0, -30.0, 20.0, 0.0);
    let (on_peak, off_peak) = (db(peak(&on[0])), db(peak(&off[0])));
    assert!(
        on_peak < expected + 0.5,
        "with lookahead the peak is {on_peak:.2} dB, the curve gives {expected:.2} dB"
    );
    assert!(
        off_peak > expected + 6.0,
        "without it the first samples get through: {off_peak:.2} dB"
    );
    // The audio is late by exactly the latency: the silence ahead of the burst is 144 frames longer.
    let first = |y: &Stereo| y[0].iter().position(|s| s.abs() > 1e-4).unwrap();
    assert!(
        first(&on) >= first(&off) + 143,
        "{} {}",
        first(&on),
        first(&off)
    );
}

#[test]
fn lookahead_latency_reaches_the_engine_and_changes_with_the_switch() {
    use komponist_engine::Processor;
    let mut v = KIND.defaults();
    v[wire(KIND, "lookahead")] = 1.0;
    let mut p = KIND.processor(&v);
    p.activate(RATE, 256).unwrap();
    assert_eq!(p.latency(), 144);
    assert_eq!(p.meter().latency(), 144);
    p.activate(44_100.0, 256).unwrap();
    assert_eq!(p.latency(), 132, "3 ms at 44.1 kHz");
    let mut q = KIND.processor(&KIND.defaults());
    q.activate(RATE, 256).unwrap();
    assert_eq!(q.latency(), 0, "off by default");
}

#[test]
fn mix_zero_is_bit_exact_and_one_is_the_wet_signal() {
    let x = noise(20_000, 5);
    for lookahead in [0.0, 1.0] {
        let busy = [
            ("threshold", -40.0),
            ("ratio", 20.0),
            ("sc_hpf", 100.0),
            ("link", 0.3),
            ("makeup", 6.0),
            ("lookahead", lookahead),
        ];
        let mut set = busy.to_vec();
        set.push(("mix", 0.0));
        let mut d = make(KIND, RATE, &set);
        let y = play(&mut *d, &x, 256, &[]);
        let delay = d.latency() as usize;
        let mut want = silence(20_000);
        for c in 0..2 {
            for n in delay..20_000 {
                want[c][n] = x[c][n - delay];
            }
        }
        assert_same(&y, &want, &format!("mix 0, lookahead {lookahead}"));
        // The same settings at mix 1 compress.
        let mut set = busy.to_vec();
        set.push(("mix", 1.0));
        let mut wet = make(KIND, RATE, &set);
        let w = play(&mut *wet, &x, 256, &[]);
        assert!(peak(&w[0]) != peak(&y[0]));
    }
}

#[test]
fn parallel_mix_adds_dry_and_wet_in_phase() {
    // Half mix of a reduced and an unreduced signal is their mean.
    let x = dc(-6.0, 24_000);
    let set = [("threshold", -30.0), ("ratio", 20.0), ("knee", 0.0)];
    let mut full = set.to_vec();
    full.push(("mix", 1.0));
    let mut half = set.to_vec();
    half.push(("mix", 0.5));
    let wet = play(&mut *plain(&full), &x, 256, &[]);
    let mixed = play(&mut *plain(&half), &x, 256, &[]);
    let (w, m, dry) = (wet[0][23_999], mixed[0][23_999], x[0][23_999]);
    assert!(
        (m - 0.5 * (w + dry)).abs() < 1e-5,
        "{m} vs {}",
        0.5 * (w + dry)
    );
}

/// A loud left channel and a quiet right one (below the threshold on its own).
fn lopsided(frames: usize) -> Stereo {
    [vec![amp(-6.0); frames], vec![amp(-40.0); frames]]
}

#[test]
fn link_ties_the_channels_together_and_zero_leaves_them_alone() {
    let x = lopsided(24_000);
    let set = |link: f64| {
        [
            ("threshold", -30.0),
            ("ratio", 4.0),
            ("knee", 0.0),
            ("link", link),
        ]
    };
    let gain = |link: f64| {
        let y = play(&mut *plain(&set(link)), &x, 256, &[]);
        [db(y[0][23_999]) + 6.0, db(y[1][23_999]) + 40.0]
    };
    let full = gain(1.0);
    assert!((full[0] - full[1]).abs() < 0.01, "linked: {full:?}");
    assert!(full[0] < -17.0);
    let none = gain(0.0);
    assert!(
        none[1].abs() < 1e-3,
        "unlinked right is untouched: {none:?}"
    );
    assert!((none[0] - full[0]).abs() < 0.01);
    let half = gain(0.5);
    // Half: the right channel's level is raised by half the distance to the left one.
    let lifted = -40.0 + 0.5 * (-6.0 + 40.0);
    let want = comp_curve::reduction_db(lifted, -30.0, 4.0, 0.0);
    assert!((half[1] - want).abs() < 0.1, "{half:?}, want {want}");
    assert!(half[1] < 0.0 && half[1] > full[1]);
    // Unlinked and symmetric material: both channels do the same.
    let sym = dc(-6.0, 24_000);
    let y = play(&mut *plain(&set(0.0)), &sym, 256, &[]);
    assert_eq!(y[0][23_999].to_bits(), y[1][23_999].to_bits());
}

/// The reduction history the tap collected over the last `pairs` pairs.
fn trace(d: &dyn Dsp, pairs: usize) -> Vec<[f32; 2]> {
    let tap = d.tap().unwrap();
    let mut flat = vec![0.0; pairs * 2];
    let end = tap.read_latest(&mut flat).expect("enough samples");
    // An odd end would swap the roles of the two values.
    assert_eq!(end % 2, 0);
    flat.chunks(2).map(|c| [c[0], c[1]]).collect()
}

#[test]
fn the_sidechain_high_pass_keeps_bass_from_pumping_the_mids() {
    // A loud 30 Hz bass under a quiet 3 kHz tone that is below the threshold by itself.
    let frames = 48_000;
    let bass = sine(30.0, 0.8, frames, RATE);
    let mid = sine(3000.0, 0.05, frames, RATE);
    let x: Stereo = std::array::from_fn(|c| (0..frames).map(|i| bass[c][i] + mid[c][i]).collect());
    let ripple = |hpf: f64| {
        let mut d = plain(&[
            ("threshold", -18.0),
            ("ratio", 4.0),
            ("knee", 0.0),
            ("attack", 0.5),
            ("release", 15.0),
            ("sc_hpf", hpf),
        ]);
        let tap = d.tap().unwrap();
        tap.want();
        // Keep the tap's lease up across the blocks.
        for a in (0..frames).step_by(4800) {
            tap.want();
            let [ol, or] = &mut silence(4800);
            d.process(
                [&x[0][a..a + 4800], &x[1][a..a + 4800]],
                [&mut ol[..], &mut or[..]],
            );
        }
        // The last half second.
        let t = trace(&*d, 750);
        let reductions: Vec<f32> = t.iter().map(|p| p[1]).collect();
        let hi = reductions.iter().copied().fold(f32::MIN, f32::max);
        let lo = reductions.iter().copied().fold(f32::MAX, f32::min);
        (
            hi - lo,
            reductions.iter().sum::<f32>() / reductions.len() as f32,
        )
    };
    let (off, off_mean) = ripple(20.0);
    let (on, on_mean) = ripple(300.0);
    assert!(off > 3.0, "without the filter the bass pumps: {off:.2} dB");
    assert!(
        on < 0.4 * off,
        "with 300 Hz the pumping drops: {on:.2} dB instead of {off:.2} dB"
    );
    assert!(on_mean < off_mean, "{on_mean} vs {off_mean}");
}

#[test]
fn sc_listen_outputs_the_filtered_detector_signal() {
    let frames = 24_000;
    let low = sine(40.0, 0.5, frames, RATE);
    let high = sine(4000.0, 0.5, frames, RATE);
    let x: Stereo = std::array::from_fn(|c| (0..frames).map(|i| low[c][i] + high[c][i]).collect());
    let mut d = plain(&[
        ("sc_hpf", 500.0),
        ("sc_listen", 1.0),
        ("makeup", 12.0),
        ("mix", 0.0),
    ]);
    let y = play(&mut *d, &x, 256, &[]);
    let tail = &y[0][12_000..];
    // Mix and makeup do not apply: the bass is gone, the 4 kHz tone is there at its own level.
    let level = super::fx_tests::rms(tail);
    assert!(
        (level - 0.3536).abs() < 0.012,
        "rms {level}, the 4 kHz tone alone is 0.3536"
    );
    // Switching it off brings the audio back (mix 0: the input itself).
    let mut d2 = plain(&[("sc_hpf", 500.0), ("mix", 0.0)]);
    let back = play(&mut *d2, &x, 256, &[]);
    assert_same(&back, &x, "listen off");
}

#[test]
fn auto_makeup_replaces_the_makeup_knob_with_the_reference_reduction() {
    // T −18, R 4:1, hard knee: +6.75 dB, so a signal at the reference level T/2 = −9 dB comes out
    // where it went in, and the knob has no effect while auto is on.
    let x = dc(-9.0, 48_000);
    let set = [("threshold", -18.0), ("ratio", 4.0), ("knee", 0.0)];
    let level = |extra: &[(&str, f64)]| {
        let mut s = set.to_vec();
        s.extend_from_slice(extra);
        let y = play(&mut *plain(&s), &x, 256, &[]);
        db(y[0][47_999])
    };
    assert!((level(&[("auto_makeup", 1.0)]) + 9.0).abs() < 0.1);
    assert!((level(&[("auto_makeup", 1.0), ("makeup", -6.0)]) + 9.0).abs() < 0.1);
    assert!((level(&[("makeup", 3.0)]) - (-9.0 - 6.75 + 3.0)).abs() < 0.1);
}

#[test]
fn the_gr_meter_reports_the_deepest_reduction_and_resets() {
    let mut d = plain(&[("threshold", -30.0), ("ratio", 4.0), ("knee", 0.0)]);
    let y = dc(-6.0, 24_000);
    let _ = play(&mut *d, &y, 256, &[]);
    let gr = d.take_reduction_db();
    assert!((gr - 18.0).abs() < 0.2, "{gr}");
    assert_eq!(d.take_reduction_db(), 0.0, "taking resets");
    assert_eq!(d.gr_range_db(), 24.0);
}

#[test]
fn the_tap_carries_level_and_reduction_pairs_while_somebody_looks() {
    let mut d = plain(&[("threshold", -30.0), ("ratio", 4.0), ("knee", 0.0)]);
    let tap = d.tap().unwrap();
    assert_eq!(tap.rate(), RATE);
    // Nobody looks: nothing is written.
    let _ = play(&mut *d, &dc(-6.0, 4800), 256, &[]);
    assert_eq!(tap.written(), 0);
    tap.want();
    let _ = play(&mut *d, &dc(-6.0, 12_000), 256, &[]);
    let pairs = trace(&*d, 100);
    let last = pairs.last().unwrap();
    assert!((last[0] + 6.0).abs() < 0.1, "level {}", last[0]);
    assert!((last[1] - 18.0).abs() < 0.3, "reduction {}", last[1]);
    // One pair per 32 frames at 48 kHz, 64 per 2 048 frames.
    assert_eq!(super::presse::pair_frames(48_000.0), 32);
    assert_eq!(super::presse::pair_frames(96_000.0), 64);
    assert_eq!(super::presse::pair_frames(44_100.0), 32);
    assert!(tap.written() >= 2 * (12_000 / 32 - 4) as u64);
    // The tap holds the 4 s of history at every supported rate.
    for rate in [44_100.0f32, 48_000.0, 96_000.0, 192_000.0] {
        let pairs = (super::presse::HISTORY_SECONDS * rate
            / super::presse::pair_frames(rate) as f32)
            .ceil() as usize;
        assert!(pairs * 2 <= tap.capacity() / 2, "{rate}: {pairs} pairs");
    }
}

#[test]
fn an_impulse_through_silence_stays_finite_and_the_curve_below_the_threshold_is_unity() {
    let mut d = plain(&[("threshold", -10.0), ("knee", 0.0)]);
    let y = play(&mut *d, &impulse(2000), 256, &[]);
    assert!(y[0].iter().all(|s| s.is_finite()));
    // Far below the threshold nothing is changed: bit-exact.
    let quiet = dc(-50.0, 4800);
    let z = play(
        &mut *plain(&[("threshold", -10.0), ("knee", 0.0)]),
        &quiet,
        256,
        &[],
    );
    assert_same(&z, &quiet, "below the threshold");
}

#[test]
fn the_presets_are_valid_and_play() {
    let list = KIND.params();
    assert_eq!(KIND.presets().len(), 11);
    for (i, preset) in KIND.presets().iter().enumerate() {
        let name = preset.name;
        for (key, _) in preset.changes {
            assert!(
                list.iter().any(|p| p.id.as_str() == *key),
                "{name}: unknown parameter {key}"
            );
        }
        let values = KIND.preset_values(i).expect("a preset");
        assert_eq!(values.len(), list.len());
        for (v, p) in values.iter().zip(list) {
            let (lo, hi) = p.range();
            assert!((lo..=hi).contains(v), "{name}: {} = {v}", p.id.as_str());
        }
        let mut d = komponist_presse::dsp();
        for (w, v) in values.iter().enumerate() {
            d.set_param(w, *v);
        }
        d.activate(RATE);
        let out = play(&mut *d, &noise(4800, 3 + i as u32), 256, &[]);
        assert!(
            out[0].iter().chain(&out[1]).all(|s| s.is_finite()),
            "{name}"
        );
    }
    assert!(KIND.preset_values(11).is_none());
}

// ------------------------------------------------------------------------------------------------
// External sidechain (WP-104)
// ------------------------------------------------------------------------------------------------

/// `play` with a sidechain signal, in calls of `chunk` frames.
fn play_sc(d: &mut dyn Dsp, input: &Stereo, sc: &Stereo, chunk: usize) -> Stereo {
    let frames = input[0].len();
    let mut out = [vec![0.0; frames], vec![0.0; frames]];
    for a in (0..frames).step_by(chunk) {
        let b = (a + chunk).min(frames);
        let [ol, or] = &mut out;
        assert_no_alloc::assert_no_alloc(|| {
            d.process_sidechain(
                [&input[0][a..b], &input[1][a..b]],
                [&sc[0][a..b], &sc[1][a..b]],
                [&mut ol[a..b], &mut or[a..b]],
            )
        });
    }
    out
}

#[test]
fn the_spec_flags_the_sidechain_input_of_presse_only() {
    for kind in Internal::ALL {
        assert_eq!(kind.has_sidechain(), kind == Internal::Presse, "{kind:?}");
    }
}

#[test]
fn the_sidechain_feeds_the_detector_instead_of_the_input() {
    let pad = dc(-30.0, 24_000);
    let silent = silence(24_000);
    let loud = dc(-6.0, 24_000);
    let set = [("threshold", -20.0), ("ratio", 8.0), ("knee", 0.0)];
    // A quiet input with a loud sidechain is reduced; a loud input with a silent sidechain is not.
    let ducked = play_sc(&mut *plain(&set), &pad, &loud, 256);
    assert!(db(ducked[0][23_999]) - (-30.0) < -10.0, "ducked");
    let free = play_sc(&mut *plain(&set), &loud, &silent, 256);
    assert_same(&free, &loud, "silent sidechain");
}

#[test]
fn a_sidechain_equal_to_the_input_is_the_same_as_none() {
    let x = noise(20_000, 7);
    let sets: [&[(&str, f64)]; 3] = [
        &[("threshold", -30.0)],
        &[("sc_hpf", 120.0), ("detector", 1.0), ("link", 0.5)],
        &[("lookahead", 1.0), ("mix", 0.5), ("auto_release", 1.0)],
    ];
    for set in sets {
        let alone = play(&mut *plain(set), &x, 200, &[]);
        let with = play_sc(&mut *plain(set), &x, &x, 200);
        assert_same(&alone, &with, "sidechain = input");
    }
}

#[test]
fn the_filter_and_listen_work_on_the_sidechain() {
    // 50 Hz in the sidechain, 1 kHz as the audio; the high-pass at 500 Hz removes the sidechain.
    let audio = sine(1000.0, 0.01, 24_000, RATE);
    let sc = sine(50.0, 0.5, 24_000, RATE);
    let set = [("threshold", -30.0), ("ratio", 10.0)];
    let hard = play_sc(&mut *plain(&set), &audio, &sc, 256);
    let mut filtered = set.to_vec();
    filtered.push(("sc_hpf", 500.0));
    let soft = play_sc(&mut *plain(&filtered), &audio, &sc, 256);
    let tail = |y: &Stereo| peak(&y[0][12_000..]);
    assert!(
        tail(&hard) < 0.5 * tail(&soft),
        "high-pass lets the audio through"
    );
    // Listen: the output is the filtered sidechain, not the audio.
    filtered.push(("sc_listen", 1.0));
    let heard = play_sc(&mut *plain(&filtered), &audio, &sc, 256);
    let alone = play(&mut *plain(&filtered), &sc, 256, &[]);
    assert_same(&heard, &alone, "listen = filtered sidechain");
}

#[test]
fn the_ducking_preset_pushes_a_pad_down_by_a_kick() {
    let index = KIND
        .presets()
        .iter()
        .position(|p| p.name == "Ducking")
        .expect("a Ducking preset");
    let values = KIND.preset_values(index).unwrap();
    let mut d = komponist_presse::dsp();
    for (i, v) in values.iter().enumerate() {
        d.set_param(i, *v);
    }
    d.activate(RATE);
    let pad = dc(-20.0, 24_000);
    let kick = dc(-3.0, 24_000);
    let y = play_sc(&mut *d, &pad, &kick, 256);
    assert!(db(y[0][23_999]) < -20.0 - 12.0, "ducks by more than 12 dB");
}
