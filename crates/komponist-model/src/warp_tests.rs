use super::*;
use crate::{AudioClip, AudioFile, ClipTimeMode};
use proptest::prelude::*;
use std::path::Path;

fn clip(rate: u32, offset: u64, bpm: f64) -> AudioClip {
    let mut a = AudioClip::new(
        AudioFile::new(Path::new("/synthetic.wav"), None, u64::MAX, rate, 2),
        offset,
        Tempo::new(bpm),
    );
    a.warp = Some(AudioWarp::new(true));
    a
}

#[test]
fn native_offsets_remain_exact_above_f64_integer_precision() {
    let offset = (1_u64 << 60) + 17;
    let a = clip(44100, offset, 123.45);
    let m = a.warp_map(0).unwrap();
    assert_eq!(m.source_frame(Tick::ZERO), Ok(offset));
    for t in [1, 13, 97, 3840, 1_000_000] {
        let delta = (t as f64 * 44100.0 * 60.0 / (123.45 * 960.0)).round() as u64;
        assert_eq!(m.source_frame(Tick(t)), Ok(offset + delta));
        assert_eq!(m.tick_at_source_frame(offset + delta), Ok(Tick(t)));
    }
    assert_eq!(m.duration_ratio(Tempo::new(82.3)), Ok(1.5));
    // Current playback remains unwarped until engine integration.
    assert_eq!(a.speed(Tempo::new(82.3)), 1.0);
}

#[test]
fn mixed_native_rates_and_repeated_shifts_do_not_accumulate_rounding() {
    let mut a = clip(48000, 1_000_000, 123.45);
    for rate in [1, 44100, 192000, u32::MAX] {
        a.takes.push(AudioTake {
            name: String::new(),
            file: AudioFile::new(Path::new("/take.wav"), None, u64::MAX, rate, 1),
            offset: 1_000_000,
        });
    }
    let maps: Vec<_> = (0..a.takes.len()).map(|i| a.warp_map(i).unwrap()).collect();
    for _ in 0..257 {
        a.shift_sources(Tick(1), Tempo::DEFAULT).unwrap();
    }
    assert_eq!(a.warp.unwrap().phase(), Tick(257));
    for (take, map) in a.takes.iter().zip(&maps) {
        assert_eq!(take.offset, map.source_frame(Tick(257)).unwrap());
    }
    a.shift_sources(Tick(-257), Tempo::DEFAULT).unwrap();
    assert!(a.takes.iter().all(|take| take.offset == 1_000_000));
    assert_eq!(a.warp.unwrap().phase(), Tick::ZERO);
}

#[test]
fn mapping_inverse_and_backwards_limits_observe_rounded_native_boundaries() {
    let a = clip(48000, 100, 120.0);
    let m = a.warp_map(0).unwrap();
    assert_eq!(m.source_frame(Tick(-4)), Ok(0));
    assert_eq!(m.source_frame(Tick(-5)), Err(WarpError::SourcePosition));
    assert_eq!(m.available_before(), Tick(4));
    assert_eq!(m.tick_at_source_frame(124), Ok(Tick(0)));
    assert_eq!(m.tick_at_source_frame(125), Ok(Tick(1)));
    assert_eq!(m.ticks_for_frames(99), Tick(3));
    assert_eq!(m.ticks_for_frames(100), Tick(4));
}

#[test]
fn phase_continuity_checks_half_frame_ties_when_the_translated_origins_cross_zero() {
    let mut early = clip(2880, 800, 120.0);
    early.warp = Some(early.warp.unwrap().shifted(Tick(-2)).unwrap());
    let early = early.warp_map(0).unwrap();
    let later = clip(2880, 800, 120.0).warp_map(0).unwrap();
    assert_eq!(early.source_frame(Tick(2)), later.source_frame(Tick(2)));
    assert_ne!(early.source_frame(Tick(1)), later.source_frame(Tick(1)));
    assert!(!early.continues_with(Tick::ZERO, later, Tick(2)));

    let later = clip(2880, 803, 120.0).warp_map(0).unwrap();
    assert!(early.continues_with(Tick(2), later, Tick(8)));
}

#[test]
fn model_ratios_are_not_limited_to_the_current_dsp_wrapper_domain() {
    let a = clip(48000, 0, 40.0);
    assert_eq!(
        a.warp_map(0).unwrap().duration_ratio(Tempo::new(240.0)),
        Ok(1.0 / 6.0)
    );
    assert_eq!(
        a.warp_map(0).unwrap().duration_ratio(Tempo::new(f64::NAN)),
        Err(WarpError::Tempo)
    );
}

#[test]
fn metadata_and_mapping_domains_reject_invalid_values_without_panics() {
    for wire in [
        serde_json::json!({"version":2,"enabled":true}),
        serde_json::json!({"version":1}),
        serde_json::json!({"version":1,"enabled":true,"phase":MAX_WARP_TICK+1}),
    ] {
        assert!(serde_json::from_value::<AudioWarp>(wire).is_err());
    }
    let mut a = clip(48000, 0, 120.0);
    let m = a.warp_map(0).unwrap();
    assert_eq!(m.source_frame(Tick(i64::MAX)), Err(WarpError::Phase));
    assert_eq!(m.source_frames(Tick(-1)), Err(WarpError::Phase));
    a.time_mode = ClipTimeMode::RePitch;
    assert!(matches!(a.warp_map(0), Err(WarpError::Mode)));
    assert!(serde_json::to_value(&a).is_err());
    a.time_mode = ClipTimeMode::Fixed;
    a.original_tempo = Tempo::new(f64::INFINITY);
    assert!(matches!(a.warp_map(0), Err(WarpError::Tempo)));
    a.original_tempo = Tempo::DEFAULT;
    a.takes[0].file.sample_rate = 0;
    assert!(matches!(a.warp_map(0), Err(WarpError::SampleRate)));
}

proptest! {
    #[test]
    fn shifted_mapping_telescopes_for_arbitrary_valid_bpm_and_native_rates(
        bpm in 40.0_f64..240.0,
        rate in prop::sample::select(vec![1_u32,44100,48000,192000,u32::MAX]),
        phase in (-MAX_WARP_TICK+2000)..(MAX_WARP_TICK-2000),
        first in -999_i64..999,
        second in -999_i64..999,
    ) {
        let mut a=clip(rate,1_u64<<62,bpm);
        a.warp=Some(AudioWarp {enabled:true,phase:Tick(phase)});
        let original=a.warp_map(0).unwrap();
        a.shift_sources(Tick(first),Tempo::DEFAULT).unwrap();
        a.shift_sources(Tick(second),Tempo::DEFAULT).unwrap();
        prop_assert_eq!(a.offset,original.source_frame(Tick(first+second)).unwrap());
        a.shift_sources(Tick(-first-second),Tempo::DEFAULT).unwrap();
        prop_assert_eq!(a.offset,1_u64<<62);
        prop_assert_eq!(a.warp.unwrap().phase(),Tick(phase));
    }
}
