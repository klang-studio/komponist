//! Sample-rate conversion accuracy.

use komponist_audiofile::{AudioError, StreamResampler, resample};

use crate::common::*;

/// RMS error against the ideal sine at the output rate, ignoring the first and last `edge`
/// frames (the filter's start-up and tail).
fn sine_error(out: &[f32], freq: f64, rate: u32, amp: f64, edge: usize) -> f64 {
    let want = sine(freq, rate, out.len(), amp);
    rms((edge..out.len() - edge).map(|i| f64::from(out[i] - want[i])))
}

#[test]
fn a_sine_survives_the_conversion_between_common_rates() {
    for (from, to) in [
        (44100, 48000),
        (48000, 44100),
        (22050, 48000),
        (96000, 48000),
        (44100, 96000),
    ] {
        let input = vec![sine(1000.0, from, from as usize, 0.5)];
        let out = resample(&input, from, to).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].len(),
            to as usize,
            "{from} -> {to}: one second stays one second"
        );
        // Error below −90 dB re the signal, over everything but 20 ms at each end. Measured:
        // ≈ −130 dB, the precision of f32; the bound leaves room for platform float differences.
        let edge = to as usize / 50;
        let err = sine_error(&out[0], 1000.0, to, 0.5, edge);
        assert!(err < 1.1e-5, "{from} -> {to}: rms error {err}");
        // And the tone's level is untouched.
        let level = tone_amplitude(&out[0][edge..out[0].len() - edge], 1000.0, to);
        assert!((level - 0.5).abs() < 1e-4, "{from} -> {to}: level {level}");
    }
}

#[test]
fn the_passband_is_flat_up_to_the_top_of_the_audio_band() {
    for freq in [10000.0, 18000.0, 20000.0] {
        let out = resample(&[sine(freq, 44100, 44100, 0.5)], 44100, 48000).unwrap();
        let level = tone_amplitude(&out[0][2400..out[0].len() - 2400], freq, 48000);
        assert!(
            (level - 0.5).abs() < 0.005,
            "{freq} Hz: {level} (0.09 dB is 0.005)"
        );
    }
}

#[test]
fn the_edges_line_up_with_the_input() {
    // No delay: the output is time-aligned, so the first frames already follow the sine.
    let input = vec![sine(500.0, 44100, 44100, 0.5)];
    let out = resample(&input, 44100, 48000).unwrap();
    let want = sine(500.0, 48000, out[0].len(), 0.5);
    let head = rms((0..48).map(|i| f64::from(out[0][i] - want[i])));
    assert!(head < 0.02, "head error {head}");
}

#[test]
fn tones_above_the_new_nyquist_are_rejected() {
    // 15 kHz at 48 kHz, converted to 24 kHz (Nyquist 12 kHz): must not alias to 9 kHz.
    let input = vec![sine(15000.0, 48000, 48000, 0.5)];
    let out = resample(&input, 48000, 24000).unwrap();
    let mid = &out[0][2400..out[0].len() - 2400];
    let alias = tone_amplitude(mid, 9000.0, 24000);
    // Measured 2e-8 (−150 dB); the bound is −100 dB re the input tone.
    assert!(alias < 5e-6, "alias at 9 kHz: {alias}");
}

#[test]
fn channels_are_converted_independently() {
    let l = sine(440.0, 44100, 22050, 0.5);
    let r = sine(880.0, 44100, 22050, 0.25);
    let out = resample(&[l, r], 44100, 48000).unwrap();
    assert_eq!(out[0].len(), out[1].len());
    let edge = 1000;
    assert!(sine_error(&out[0], 440.0, 48000, 0.5, edge) < 1e-3);
    assert!(sine_error(&out[1], 880.0, 48000, 0.25, edge) < 1e-3);
}

#[test]
fn equal_rates_copy() {
    let input = vec![noise(1000, 1)];
    assert_eq!(resample(&input, 48000, 48000).unwrap(), input);
}

#[test]
fn short_input_still_gets_the_right_length() {
    for frames in [1usize, 2, 100, 1023, 1024, 1025] {
        let input = vec![noise(frames, 7)];
        let out = resample(&input, 44100, 48000).unwrap();
        assert_eq!(out[0].len(), (frames * 48000).div_ceil(44100), "{frames}");
    }
}

#[test]
fn zero_rates_are_errors() {
    assert!(matches!(
        resample(&[vec![0.0; 10]], 0, 48000),
        Err(AudioError::Resample(_))
    ));
    assert!(matches!(
        resample(&[vec![0.0; 10]], 44100, 0),
        Err(AudioError::Resample(_))
    ));
}

#[test]
fn streaming_in_odd_chunks_equals_converting_the_whole() {
    let input = vec![
        sine(1234.0, 44100, 30000, 0.5),
        sine(321.0, 44100, 30000, 0.3),
    ];
    let whole = resample(&input, 44100, 48000).unwrap();

    let mut stream = StreamResampler::new(44100, 48000, 2).unwrap();
    let mut out = vec![Vec::new(), Vec::new()];
    let mut at = 0;
    for len in [1usize, 17, 1000, 3, 4096, 700, 9999].iter().cycle() {
        let end = (at + len).min(30000);
        stream
            .push(&[&input[0][at..end], &input[1][at..end]], &mut out)
            .unwrap();
        at = end;
        if at == 30000 {
            break;
        }
    }
    stream.finish(&mut out).unwrap();
    assert_eq!(out[0].len(), whole[0].len());
    for c in 0..2 {
        let worst = out[c]
            .iter()
            .zip(&whole[c])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-5, "channel {c}: {worst}");
    }
}

#[test]
fn a_stream_can_be_reset_for_a_seek() {
    let input = sine(1000.0, 44100, 44100, 0.5);
    let mut stream = StreamResampler::new(44100, 48000, 1).unwrap();
    let mut out = vec![Vec::new()];
    stream.push(&[&input[..10000]], &mut out).unwrap();
    // Seek to frame 20000: reset, push from there. After the start-up transient the output is
    // the sine at the new position (the phase continues from input frame 20000).
    stream.reset();
    let mut out = vec![Vec::new()];
    stream.push(&[&input[20000..30000]], &mut out).unwrap();
    stream.finish(&mut out).unwrap();
    let t0 = 20000.0 / 44100.0;
    let want = |i: usize| 0.5 * (std::f64::consts::TAU * 1000.0 * (t0 + i as f64 / 48000.0)).sin();
    let err = rms((600..out[0].len() - 600).map(|i| f64::from(out[0][i]) - want(i)));
    assert!(err < 2e-3, "rms error after the seek {err}");
}

#[test]
fn same_rate_stream_passes_through() {
    let mut stream = StreamResampler::new(48000, 48000, 1).unwrap();
    let mut out = vec![Vec::new()];
    let x = noise(500, 3);
    stream.push(&[&x[..200], &x[..0]][..1], &mut out).unwrap();
    stream.push(&[&x[200..]], &mut out).unwrap();
    stream.finish(&mut out).unwrap();
    assert_eq!(out[0], x);
}
