use super::*;
use std::f64::consts::TAU;

const RATIOS: [f64; 6] = [0.5, 0.75, 0.9, 1.1, 1.5, 2.0];

fn tone(rate: u32, frames: usize, channels: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            (0..frames)
                .map(|n| {
                    (0.4 * (TAU * 440.0 * n as f64 / rate as f64 + c as f64 * 0.3).sin()) as f32
                })
                .collect()
        })
        .collect()
}

// Model a decoder whose read quantum differs from the fixed DSP chunk.
fn rich_source(frames: usize) -> Vec<Vec<f32>> {
    let mut state = 14_u32;
    let mut out = tone(48000, frames, 2);
    let (left, right) = out.split_at_mut(1);
    for (n, (l, r)) in left[0].iter_mut().zip(&mut right[0]).enumerate() {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (state as f64 / u32::MAX as f64 - 0.5) as f32 * 0.08;
        *l += noise;
        *r -= noise;
        if n % 7139 == 0 {
            *l += 0.2;
        }
    }
    out
}

fn window(source: &[Vec<f32>], at: usize, frames: usize, quantum: usize) -> Vec<Vec<f32>> {
    // Keep the source cursor independent of each push, including short/EOF reads.
    source
        .iter()
        .map(|channel| {
            let mut out = vec![0.0; frames];
            for start in (0..frames).step_by(quantum) {
                for (j, s) in out[start..frames.min(start + quantum)]
                    .iter_mut()
                    .enumerate()
                {
                    *s = channel.get(at + start + j).copied().unwrap_or(0.0);
                }
            }
            out
        })
        .collect()
}

fn render_with(
    s: &mut Stretcher,
    source: &[Vec<f32>],
    seek: usize,
    frames: usize,
    quantum: usize,
) -> Vec<Vec<f32>> {
    let prime = window(source, seek, s.prime_frames(), quantum);
    s.seek(&prime.iter().map(Vec::as_slice).collect::<Vec<_>>())
        .unwrap();
    let mut at = seek + s.prime_frames();
    let mut rendered = vec![Vec::with_capacity(frames); s.channels()];
    while rendered[0].len() < frames {
        let n = CHUNK_FRAMES.min(frames - rendered[0].len());
        let required = s.input_frames_required(n).unwrap();
        let input = window(source, at, required, quantum);
        let mut output = vec![vec![0.0; n]; s.channels()];
        s.process(
            &input.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &mut output.iter_mut().map(Vec::as_mut_slice).collect::<Vec<_>>(),
        )
        .unwrap();
        for (dest, chunk) in rendered.iter_mut().zip(output) {
            dest.extend(chunk);
        }
        at += required;
    }
    rendered
}
fn render(source: &[Vec<f32>], rate: u32, ratio: f64) -> Vec<Vec<f32>> {
    let mut s = Stretcher::new(rate, source.len(), ratio).unwrap();
    let frames = output_length(source[0].len() as u64, ratio).unwrap() as usize;
    render_with(&mut s, source, 0, frames, 1024)
}
fn silence_prime(s: &mut Stretcher) {
    let input = vec![vec![0.0; s.prime_frames()]; s.channels()];
    s.seek(&input.iter().map(Vec::as_slice).collect::<Vec<_>>())
        .unwrap();
}

#[test]
fn duration_and_pitch_hold_at_all_declared_ratios_and_common_rates() {
    for rate in [8000, 44100, 48000, 96000, 192000] {
        for channels in [1, 2] {
            let source = tone(rate, rate as usize * 2, channels);
            for ratio in RATIOS {
                let out = render(&source, rate, ratio);
                assert_eq!(out.len(), channels);
                for channel in out {
                    assert_eq!(
                        channel.len(),
                        (source[0].len() as f64 * ratio).round() as usize
                    );
                    assert!(channel.iter().all(|s| s.is_finite()));
                    let middle = &channel[channel.len() / 4..channel.len() * 3 / 4];
                    let crossings: Vec<f64> = middle
                        .windows(2)
                        .enumerate()
                        .filter(|(_, p)| p[0] <= 0.0 && p[1] > 0.0)
                        .map(|(j, p)| j as f64 + (-p[0] as f64) / (p[1] as f64 - p[0] as f64))
                        .collect();
                    assert!(crossings.len() > 100);
                    let hz = (crossings.len() - 1) as f64 * rate as f64
                        / (crossings.last().unwrap() - crossings[0]);
                    let cents = 1200.0 * (hz / 440.0).log2();
                    assert!(
                        cents.abs() < 1.0,
                        "rate={rate}, ratio={ratio}, pitch={cents} cents"
                    );
                    let rms = (middle.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                        / middle.len() as f64)
                        .sqrt();
                    assert!(rms > 0.15 && rms < 0.5, "unexpected tone level {rms}");
                }
            }
        }
    }
}

#[test]
fn silence_stays_silent_after_eof_and_loud_history() {
    for ratio in RATIOS {
        let source = vec![vec![0.0; 48000]; 2];
        let out = render(&source, 48000, ratio);
        assert!(out.iter().flatten().all(|s| s.abs() <= 1e-8));
        let mut s = Stretcher::new(48000, 2, ratio).unwrap();
        let loud = vec![vec![MAX_SAMPLE_MAGNITUDE; 48000]; 2];
        let out = render_with(&mut s, &loud, 0, 48000, 64);
        assert!(out.iter().flatten().all(|s| s.is_finite()));
        let quiet = render_with(&mut s, &source, 0, 48000, 64);
        assert!(quiet.iter().flatten().all(|s| s.abs() <= 1e-8));
    }
}

#[test]
fn seek_and_ratio_changes_match_fresh_instances_bit_for_bit() {
    let source = rich_source(96000);
    let mut s = Stretcher::new(48000, 2, 0.75).unwrap();
    let first = render_with(&mut s, &source, 0, 32001, 64);
    s.reset().unwrap();
    assert_eq!(s.input_frames_required(512), Err(Error::NotPrimed));
    assert_eq!(first, render_with(&mut s, &source, 0, 32001, 1024));
    for ratio in RATIOS {
        s.set_ratio(ratio).unwrap();
        assert_eq!(s.input_frames_required(512), Err(Error::NotPrimed));
        let actual = render_with(&mut s, &source, 12345, 32001, 64);
        let mut fresh = Stretcher::new(48000, 2, ratio).unwrap();
        let expected = render_with(&mut fresh, &source, 12345, 32001, 1024);
        assert_eq!(actual, expected, "ratio={ratio}");
    }
}

#[test]
fn fixed_worker_chunks_are_independent_of_decoder_read_size_and_final_tail() {
    let source = rich_source(48000);
    for ratio in RATIOS {
        let mut s = Stretcher::new(48000, 2, ratio).unwrap();
        let full = render_with(&mut s, &source, 0, 16384, 64);
        for length in [1, 511, 512, 513, 1024, 1537, 16001] {
            let actual = render_with(&mut s, &source, 0, length, 1024);
            assert_eq!(actual[0].len(), length);
            let complete = length / 512 * 512;
            assert_eq!(&actual[0][..complete], &full[0][..complete]);
            if length % CHUNK_FRAMES != 0 {
                assert_eq!(s.input_frames_required(512), Err(Error::Finished));
            }
        }
        assert_eq!(full, render_with(&mut s, &source, 0, 16384, 1024));
    }
}

#[test]
fn compensated_click_positions_meet_the_three_ms_target() {
    let mut source = vec![vec![0.0; 48000 * 3]; 2];
    for ch in &mut source {
        for at in [24000, 48000, 72000, 96000, 120000] {
            ch[at] = 0.7;
        }
    }
    for ratio in RATIOS {
        let out = render(&source, 48000, ratio);
        for at in [24000, 48000, 72000, 96000, 120000] {
            let expected = (at as f64 * ratio).round() as usize;
            let lo = expected.saturating_sub(4800);
            let hi = (expected + 4800).min(out[0].len());
            let peak = lo
                + out[0][lo..hi]
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
                    .unwrap()
                    .0;
            assert!(
                peak.abs_diff(expected) <= 144,
                "ratio={ratio}, error={} frames",
                peak.abs_diff(expected)
            );
        }
    }
}

#[test]
fn invalid_dimensions_and_samples_are_rejected_without_advancing_state() {
    let mut s = Stretcher::new(48000, 2, 0.9).unwrap();
    silence_prime(&mut s);
    let n = s.input_frames_required(512).unwrap();
    let input = vec![0.0; n];
    let mut a = vec![123.0; 512];
    let mut b = vec![123.0; 512];
    assert_eq!(
        s.process(&[&input], &mut [&mut a, &mut b]),
        Err(Error::Input)
    );
    assert_eq!(
        s.process(&[&input, &input[..n - 1]], &mut [&mut a, &mut b]),
        Err(Error::Input)
    );
    assert_eq!(
        s.process(&[&input, &input], &mut [&mut a]),
        Err(Error::Output)
    );
    assert_eq!(
        s.process(&[&input, &input], &mut [&mut a, &mut b[..511]]),
        Err(Error::Output)
    );
    assert_eq!(s.process(&[&input, &input], &mut []), Err(Error::Output));
    assert_eq!(s.input_frames_required(0), Err(Error::OutputFrames));
    assert_eq!(s.input_frames_required(513), Err(Error::OutputFrames));
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        MAX_SAMPLE_MAGNITUDE * 2.0,
    ] {
        let mut bad = input.clone();
        bad[0] = value;
        assert_eq!(
            s.process(&[&bad, &input], &mut [&mut a, &mut b]),
            Err(Error::Input)
        );
    }
    assert!(a.iter().chain(&b).all(|&x| x == 123.0));
    assert_eq!(s.input_frames_required(512).unwrap(), n);
    assert_eq!(s.seek(&[&[]]), Err(Error::Input));
    assert_eq!(s.input_frames_required(512).unwrap(), n);
    s.process(&[&input, &input], &mut [&mut a, &mut b]).unwrap();
    assert!(a.iter().chain(&b).all(|x| x.abs() < 1e-8));
}

#[test]
fn configuration_and_counter_bounds_are_checked_before_native_calls() {
    for rate in [0, 7999, 384001, u32::MAX] {
        assert!(matches!(
            Stretcher::new(rate, 2, 1.0),
            Err(Error::SampleRate)
        ));
    }
    for channels in [0, 3, usize::MAX] {
        assert!(matches!(
            Stretcher::new(48000, channels, 1.0),
            Err(Error::Channels)
        ));
    }
    let mut s = Stretcher::new(48000, 1, 1.0).unwrap();
    for ratio in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        MIN_RATIO.next_down(),
        MAX_RATIO.next_up(),
    ] {
        assert!(matches!(Stretcher::new(48000, 2, ratio), Err(Error::Ratio)));
        assert_eq!(s.set_ratio(ratio), Err(Error::Ratio));
        assert_eq!(s.ratio(), 1.0);
        assert_eq!(output_length(48000, ratio), Err(Error::Ratio));
    }
    assert_eq!(output_length(0, 1.0), Ok(0));
    assert_eq!(output_length(1, 0.5), Ok(1));
    assert_eq!(output_length(3, 1.5), Ok(5));
    assert_eq!(
        output_length(MAX_STREAM_FRAMES, 2.0),
        Err(Error::FrameLimit)
    );
    assert_eq!(output_length(u64::MAX, 0.5), Err(Error::FrameLimit));
    assert_eq!(s.input_frames_required(512), Err(Error::NotPrimed));
    silence_prime(&mut s);
    s.produced = MAX_STREAM_FRAMES + s.latency.output_frames as u64;
    assert_eq!(s.input_frames_required(512), Err(Error::FrameLimit));
    assert_eq!(
        s.latency(),
        Latency {
            input_frames: 2880,
            output_frames: 2880
        }
    );
    assert_eq!(s.sample_rate(), 48000);
    assert_eq!(s.channels(), 1);
}

#[test]
fn a_single_output_frame_can_legitimately_require_no_new_input() {
    let mut s = Stretcher::new(48000, 1, 1.9).unwrap();
    silence_prime(&mut s);
    assert_eq!(s.input_frames_required(1), Ok(0));
    let mut output = [123.0];
    s.process(&[&[]], &mut [&mut output]).unwrap();
    assert!(output[0].abs() < 1e-8);
    assert_eq!(s.input_frames_required(1), Err(Error::Finished));
}

#[test]
fn exclusive_state_can_move_to_a_worker_thread() {
    let s = Stretcher::new(48000, 1, 1.5).unwrap();
    let actual = std::thread::spawn(move || {
        let mut s = s;
        render_with(&mut s, &tone(48000, 48000, 1), 0, 4096, 64)
    })
    .join()
    .unwrap();
    let mut fresh = Stretcher::new(48000, 1, 1.5).unwrap();
    assert_eq!(
        actual,
        render_with(&mut fresh, &tone(48000, 48000, 1), 0, 4096, 1024)
    );
}

#[test]
fn bounded_high_headroom_noise_stays_finite() {
    let mut source = rich_source(48000);
    let peak = source
        .iter()
        .flatten()
        .map(|s| s.abs())
        .fold(0.0_f32, f32::max);
    for sample in source.iter_mut().flatten() {
        *sample = (*sample / peak * MAX_SAMPLE_MAGNITUDE)
            .clamp(-MAX_SAMPLE_MAGNITUDE, MAX_SAMPLE_MAGNITUDE);
    }
    for ratio in RATIOS {
        for channel in render(&source, 192000, ratio) {
            assert!(channel.iter().all(|s| s.is_finite()));
        }
    }
}

// Oversampled narrow-band DFT peak. Phase-randomized extreme expansion can add tiny zero
// crossings, so count-based frequency is not the dominant sinusoidal pitch metric here.
fn dominant_tone_hz(x: &[f32], rate: u32) -> f64 {
    let stride = (rate / 4000).max(1) as usize;
    let samples: Vec<f64> = x
        .iter()
        .step_by(stride)
        .enumerate()
        .map(|(i, &v)| {
            let n = x.len().div_ceil(stride);
            f64::from(v) * (0.5 - 0.5 * (TAU * i as f64 / (n - 1) as f64).cos())
        })
        .collect();
    let effective = f64::from(rate) / stride as f64;
    let powers: Vec<f64> = (0..=80)
        .map(|k| {
            let angle = TAU * (430.0 + k as f64 * 0.25) / effective;
            let (sin, cos) = angle.sin_cos();
            let (mut re, mut im, mut zr, mut zi) = (0.0, 0.0, 1.0, 0.0);
            for &v in &samples {
                re += v * zr;
                im += v * zi;
                let next = zr * cos - zi * sin;
                zi = zi * cos + zr * sin;
                zr = next;
            }
            re * re + im * im
        })
        .collect();
    let k = (1..80)
        .max_by(|&a, &b| powers[a].total_cmp(&powers[b]))
        .unwrap();
    let (a, b, c) = (powers[k - 1].ln(), powers[k].ln(), powers[k + 1].ln());
    430.0 + (k as f64 + 0.5 * (a - c) / (a - 2.0 * b + c)) * 0.25
}

#[test]
fn model_tempo_extrema_preserve_duration_pitch_silence_and_repeatability() {
    for rate in [8000, 48000, 192000, 384000] {
        let source = tone(rate, rate as usize * 2, 2);
        for (numerator, denominator) in [(1, 6), (1, 4), (1, 3), (3, 1), (4, 1), (6, 1)] {
            let ratio = numerator as f64 / denominator as f64;
            let state = Stretcher::new(rate, 2, ratio);
            assert!(
                state.is_ok(),
                "rate={rate} ratio={ratio}: {:?}",
                state.err()
            );
            let out = render(&source, rate, ratio);
            // Rational reference duration, rounded to the nearest output frame.
            let expected = (source[0].len() * numerator + denominator / 2) / denominator;
            assert!(out.iter().all(|channel| channel.len() == expected));
            assert!(out.iter().flatten().all(|x| x.is_finite()));
            let x = &out[0][out[0].len() / 4..out[0].len() * 3 / 4];
            let crossings: Vec<_> = x
                .windows(2)
                .enumerate()
                .filter(|(_, p)| p[0] <= 0.0 && p[1] > 0.0)
                .map(|(j, p)| j as f64 + (-p[0] as f64) / (p[1] as f64 - p[0] as f64))
                .collect();
            let hz = (crossings.len() - 1) as f64 * rate as f64
                / (crossings.last().unwrap() - crossings[0]);
            let cents = 1200.0 * (hz / 440.0).log2();
            let spectral = 1200.0 * (dominant_tone_hz(x, rate) / 440.0).log2();
            println!(
                "rate={rate} ratio={ratio}: dominant {spectral:.3} cents, crossing {cents:.3} cents"
            );
            assert!(
                spectral.abs() < 10.0,
                "rate={rate} ratio={ratio} dominant pitch={spectral} cents"
            );
            let mut s = Stretcher::new(rate, 2, ratio).unwrap();
            let first = render_with(&mut s, &source, 0, 12001, 64);
            assert_eq!(first, render_with(&mut s, &source, 0, 12001, 1024));
            let quiet = vec![vec![0.0; rate as usize]; 2];
            assert!(
                render_with(&mut s, &quiet, 0, 4096, 64)
                    .iter()
                    .flatten()
                    .all(|x| x.abs() < 1e-8)
            );
        }
    }
}
