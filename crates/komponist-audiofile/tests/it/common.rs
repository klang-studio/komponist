//! Shared helpers: scratch folders, WAV writers, test signals.

use std::path::{Path, PathBuf};

/// A fresh scratch folder in the system's temporary folder, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("komponist-audiofile-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A checked-in fixture: 0.75 s, 44.1 kHz, stereo, left 440 Hz and right 880 Hz at amplitude 0.5.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub const FIXTURES: [&str; 3] = [
    "stereo-440-880.flac",
    "stereo-440-880.ogg",
    "stereo-440-880.mp3",
];

/// `0.5 × sin` of `freq` at `rate`, `frames` long.
pub fn sine(freq: f64, rate: u32, frames: usize, amp: f64) -> Vec<f32> {
    (0..frames)
        .map(|n| (amp * (std::f64::consts::TAU * freq * n as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}

/// Writes a 16-bit WAV with `channels` interleaved channels.
pub fn write_wav_i16(path: &Path, rate: u32, channels: u16, interleaved: &[i16]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in interleaved {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
}

/// Writes a float WAV from planar channels.
pub fn write_wav_f32(path: &Path, rate: u32, planar: &[Vec<f32>]) {
    let spec = hound::WavSpec {
        channels: planar.len() as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..planar[0].len() {
        for c in planar {
            w.write_sample(c[i]).unwrap();
        }
    }
    w.finalize().unwrap();
}

/// Amplitude of the component of `x` at `freq` (a single-bin DFT).
pub fn tone_amplitude(x: &[f32], freq: f64, rate: u32) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, v) in x.iter().enumerate() {
        let phase = std::f64::consts::TAU * freq * n as f64 / f64::from(rate);
        re += f64::from(*v) * phase.cos();
        im += f64::from(*v) * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

pub fn rms(x: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = x.fold((0.0, 0usize), |(s, n), v| (s + v * v, n + 1));
    (sum / n.max(1) as f64).sqrt()
}

/// A deterministic noise signal in -1..1.
pub fn noise(frames: usize, seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 40) as f32 / (1u64 << 23) as f32) - 1.0
        })
        .collect()
}
