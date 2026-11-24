//! A small synthesized drum kit: eight short samples made from sine sweeps and noise, so tests,
//! the demo project and the acceptance flows have something to play without any recorded (and
//! possibly copyrighted) sample. Deterministic: the same bytes every time.
//!
//! The names are the demo's step rows (`Kick`, `Snare`, `Clap`, `Closed Hat`, `Open Hat`, `Tom`,
//! `Rim`, `Perc`), in pad order, so a sampler loaded with the kit shows exactly those rows.

use std::f32::consts::TAU;
use std::io;
use std::path::{Path, PathBuf};

/// The rate of the kit's files.
pub const RATE: u32 = 44_100;

/// The names of the kit, pad 1 first; also the file stems.
pub const NAMES: [&str; 8] = [
    "Kick",
    "Snare",
    "Clap",
    "Closed Hat",
    "Open Hat",
    "Tom",
    "Rim",
    "Perc",
];

/// A tiny deterministic noise source (xorshift), −1 … 1.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn frames(seconds: f32) -> usize {
    (seconds * RATE as f32) as usize
}

/// A sine that falls from `from` to `to` Hz over the sample, with an exponential decay.
fn sweep(seconds: f32, from: f32, to: f32, decay: f32) -> Vec<f32> {
    let n = frames(seconds);
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let hz = to + (from - to) * (-t * 9.0).exp();
            phase = (phase + hz / RATE as f32).fract();
            (phase * TAU).sin() * (-t * decay).exp()
        })
        .collect()
}

/// Noise with a decay, high-passed by differencing `passes` times (more passes, brighter).
fn noise(seconds: f32, decay: f32, passes: u32, seed: u32) -> Vec<f32> {
    let n = frames(seconds);
    let mut rng = Noise(seed);
    let mut v: Vec<f32> = (0..n)
        .map(|i| rng.next() * (-(i as f32 / n as f32) * decay).exp())
        .collect();
    for _ in 0..passes {
        let mut last = 0.0;
        for s in &mut v {
            let x = *s;
            *s = (x - last) * 0.5;
            last = x;
        }
    }
    let peak = v.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    v.iter().map(|s| s / peak).collect()
}

fn mix(a: &[f32], b: &[f32], gain_b: f32) -> Vec<f32> {
    (0..a.len().max(b.len()))
        .map(|i| a.get(i).copied().unwrap_or(0.0) + gain_b * b.get(i).copied().unwrap_or(0.0))
        .collect()
}

/// The kit as mono samples at [`RATE`], in pad order ([`NAMES`]), peak 0.9.
pub fn kit() -> Vec<Vec<f32>> {
    let kick = sweep(0.32, 160.0, 48.0, 5.0);
    let snare = mix(
        &sweep(0.18, 220.0, 170.0, 9.0),
        &noise(0.22, 7.0, 1, 7),
        0.8,
    );
    // A clap is a few short noise bursts in a row.
    let mut clap = noise(0.24, 14.0, 1, 11);
    for (i, s) in clap.iter_mut().enumerate() {
        let burst = ((i as f32 / RATE as f32) * 90.0).fract();
        *s *= if i < frames(0.03) { 0.5 + burst } else { 1.0 };
    }
    let hat_closed = noise(0.06, 22.0, 2, 3);
    let hat_open = noise(0.4, 6.0, 2, 5);
    let tom = sweep(0.4, 140.0, 85.0, 5.0);
    let rim = mix(
        &sweep(0.05, 1700.0, 1500.0, 26.0),
        &noise(0.03, 30.0, 1, 13),
        0.4,
    );
    let perc = sweep(0.16, 900.0, 600.0, 14.0);
    [kick, snare, clap, hat_closed, hat_open, tom, rim, perc]
        .into_iter()
        .map(|v| {
            let peak = v.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
            v.iter().map(|s| s / peak * 0.9).collect()
        })
        .collect()
}

/// Writes the kit as 16-bit mono WAV files `<name>.wav` into `dir` (which must exist) and returns
/// the paths in pad order.
pub fn write_kit(dir: &Path) -> io::Result<Vec<PathBuf>> {
    NAMES
        .iter()
        .zip(kit())
        .map(|(name, samples)| {
            let path = dir.join(format!("{name}.wav"));
            write_wav(&path, &samples, RATE)?;
            Ok(path)
        })
        .collect()
}

/// Writes mono `samples` as a 16-bit WAV at `rate`.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let to_io = |e: hound::Error| io::Error::other(e.to_string());
    let mut writer = hound::WavWriter::create(path, spec).map_err(to_io)?;
    for s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .map_err(to_io)?;
    }
    writer.finalize().map_err(to_io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthesized_kit_is_eight_short_audible_samples() {
        let samples = kit();
        assert_eq!(samples.len(), NAMES.len());
        for (name, s) in NAMES.iter().zip(&samples) {
            let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!((0.85..=0.91).contains(&peak), "{name}: {peak}");
            assert!(s.len() > 2000 && s.len() < 20_000, "{name}: {}", s.len());
        }
        assert_eq!(samples, kit(), "deterministic");
    }
}
