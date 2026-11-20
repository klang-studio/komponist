use komponist_model::params::ParamId;

use super::{SPEC, dsp};

fn stereo_impulse(width: f64) -> [Vec<f32>; 2] {
    let mut reverb = dsp();
    for (i, param) in SPEC.params().iter().enumerate() {
        reverb.set_param(i, param.default);
    }
    for (key, value) in [
        ("mix", 1.0),
        ("wet_only", 1.0),
        ("damping", 20_000.0),
        ("modulation", 0.0),
        ("width", width),
    ] {
        let wire = SPEC.wire(&ParamId::new(key)).unwrap() as usize;
        reverb.set_param(wire, SPEC.params()[wire].snap(value));
    }
    reverb.activate(48_000.0);
    let mut input = vec![0.0; 96_000];
    input[0] = 1.0;
    let mut left = vec![0.0; input.len()];
    let mut right = vec![0.0; input.len()];
    for ((input, left), right) in input
        .chunks(256)
        .zip(left.chunks_mut(256))
        .zip(right.chunks_mut(256))
    {
        reverb.process([input, input], [left, right]);
    }
    [left, right]
}

#[test]
fn the_reverb_is_stereo_and_width_zero_is_mono() {
    let wide = stereo_impulse(1.0);
    let diff: f32 = wide[0]
        .iter()
        .zip(&wide[1])
        .map(|(a, b)| (a - b).abs())
        .sum();
    assert!(diff > 1.0, "{diff}");
    let mono = stereo_impulse(0.0);
    assert!(
        mono[0]
            .iter()
            .zip(&mono[1])
            .all(|(a, b)| (a - b).abs() < 1e-7)
    );
}
