use komponist_model::params::ParamId;

use super::{SPEC, dsp};

const RATE: f32 = 48_000.0;

#[test]
fn the_delay_time_is_exact_free_and_synced() {
    let frames = 60_000;
    let echo_at = |set_list: &[(&str, f64)], bpm: f64| -> (usize, f32) {
        let mut delay = dsp();
        for (i, param) in SPEC.params().iter().enumerate() {
            delay.set_param(i, param.default);
        }
        for (key, value) in set_list {
            let wire = SPEC.wire(&ParamId::new(*key)).unwrap() as usize;
            delay.set_param(wire, SPEC.params()[wire].snap(*value));
        }
        // Activation snaps the time to the current tempo.
        delay.set_tempo(bpm);
        delay.activate(RATE);
        let mut input = vec![0.0; frames];
        input[0] = 1.0;
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        for ((input, left), right) in input
            .chunks(256)
            .zip(left.chunks_mut(256))
            .zip(right.chunks_mut(256))
        {
            delay.process([input, input], [left, right]);
        }
        let at = (0..frames)
            .max_by(|a, b| left[*a].abs().total_cmp(&left[*b].abs()))
            .unwrap();
        (at, left[at])
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
