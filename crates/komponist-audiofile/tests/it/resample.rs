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

/// #230: rates outside what the decoders accept are refused, so no ratio can explode.
#[test]
fn rates_outside_the_decodable_range_are_errors() {
    for (from, to) in [
        (4, 192_000),
        (999, 48_000),
        (48_000, 999),
        (768_001, 48_000),
    ] {
        assert!(
            matches!(
                StreamResampler::new(from, to, 1),
                Err(AudioError::Resample(_))
            ),
            "{from} Hz to {to} Hz"
        );
    }
    assert!(StreamResampler::new(1000, 192_000, 1).is_ok());
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

/// #230: channels of unequal length used to panic in the converter (a slice past the end of the
/// shorter one); they are an error now, and the converter goes on with valid input afterwards.
#[test]
fn channels_of_unequal_length_are_an_error() {
    assert!(matches!(
        resample(&[vec![0.0; 10], vec![0.0; 5]], 44100, 48000),
        Err(AudioError::Resample(_))
    ));
    let mut converter = StreamResampler::new(44100, 48000, 2).unwrap();
    let mut out = vec![Vec::new(); 2];
    let uneven = [vec![0.25f32; 3000], vec![0.25f32; 2999]];
    assert!(converter.push(&uneven, &mut out).is_err());
    assert!(out.iter().all(Vec::is_empty));
    converter
        .push(&[vec![0.0; 100], vec![0.0; 100]], &mut out)
        .unwrap();
    converter.finish(&mut out).unwrap();
    assert_eq!(out[0].len(), out[1].len());
    assert_eq!(out[0].len(), (100 * 48000usize).div_ceil(44100));
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Any rate, channel count and push pattern (the `resampler` fuzz target's harness).
    #[test]
    fn any_push_pattern_keeps_the_channels_aligned(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..24),
    ) {
        komponist_audiofile::fuzzing::resampler(&bytes);
    }
}

/// #230 (found by the proptest): rates with a small common divisor have an FFT block of up to a
/// whole second of input. The flush at the end stopped after 64 silent chunks, before the first
/// block was complete, and a short clip came out empty.
#[test]
fn rate_pairs_with_huge_fft_blocks_still_deliver_every_frame() {
    for (from, to, frames) in [
        (96_001, 48_000, 37),
        (321_021, 96_000, 37),
        (47_999, 44_100, 5000),
    ] {
        let out = resample(&[noise(frames, 3)], from, to).unwrap();
        let expected = (frames as u64 * u64::from(to)).div_ceil(u64::from(from));
        assert_eq!(out[0].len() as u64, expected, "{from} Hz to {to} Hz");
    }
}
