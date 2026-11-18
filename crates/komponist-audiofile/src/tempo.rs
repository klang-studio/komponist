//! Estimate a regular beat period from a bounded prefix of the audio source.
use std::path::Path;
use std::{fs, io::Read};

use realfft::RealFftPlanner;

use crate::{AudioError, AudioFile, FileId, StreamResampler};

const RATE: u32 = 12000;
const HOP: usize = 120;
const WINDOW: usize = 512;
const MAX_SECONDS: u64 = 60;
const MIN_BPM: f64 = 40.0;
const MAX_BPM: f64 = 240.0;
const CACHE_MAGIC: &[u8; 8] = b"KPTEMPO1";

/// Candidate original tempo for a source. The confidence is a heuristic measure of regularity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoEstimate {
    pub bpm: f64,
    pub confidence: f32,
}

pub(crate) fn estimate(id: &FileId) -> Result<Option<TempoEstimate>, AudioError> {
    let mut file = AudioFile::open(&id.path)?;
    let info = *file.info();
    let mut src = StreamResampler::new(info.sample_rate, RATE, info.channels)?;
    let mut interleaved = vec![0.0; 4096 * info.channels];
    let mut input = vec![Vec::with_capacity(4096); info.channels];
    let mut output = vec![Vec::new(); info.channels];
    let mut remaining = u64::from(info.sample_rate) * MAX_SECONDS;
    while remaining > 0 {
        let want = 4096.min(remaining as usize);
        let frames = file.read(&mut interleaved[..want * info.channels])?;
        if frames == 0 {
            break;
        }
        for channel in &mut input {
            channel.clear();
        }
        for frame in interleaved[..frames * info.channels].chunks_exact(info.channels) {
            for (&sample, channel) in frame.iter().zip(&mut input) {
                if !sample.is_finite() {
                    return Err(AudioError::Malformed(
                        "non-finite tempo analysis input".into(),
                    ));
                }
                channel.push(sample);
            }
        }
        src.push(&input, &mut output)?;
        remaining -= frames as u64;
    }
    src.finish(&mut output)?;
    let full_length = remaining > 0 || file.position() == info.frames;
    let estimate = track_period(&onset_strength(&output), full_length);
    if FileId::of(&id.path)? != *id {
        return Err(AudioError::Io(
            "audio source changed during tempo analysis".into(),
        ));
    }
    Ok(estimate)
}

fn onset_strength(channels: &[Vec<f32>]) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let window: Vec<_> = (0..WINDOW)
        .map(|n| {
            let angle = std::f32::consts::TAU * n as f32 / (WINDOW - 1) as f32;
            0.5 - 0.5 * angle.cos()
        })
        .collect();
    let mut previous = vec![vec![0.0f32; spectrum.len()]; channels.len()];
    let mut envelope = Vec::new();
    for center in (0..channels[0].len()).step_by(HOP) {
        let mut flux = 0.0;
        // Compare magnitudes per channel so an opposite-polarity stereo source retains onsets.
        for (audio, last) in channels.iter().zip(&mut previous) {
            for (i, (sample, &w)) in input.iter_mut().zip(&window).enumerate() {
                let pos = (center + i).checked_sub(WINDOW / 2);
                *sample = pos.and_then(|pos| audio.get(pos)).copied().unwrap_or(0.0) * w;
            }
            fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
                .expect("fixed-size analysis FFT");
            for (bin, old) in spectrum[1..].iter().zip(&mut last[1..]) {
                let level = (bin.norm() * (2.0 / WINDOW as f32)).ln_1p();
                flux += (level - *old).max(0.0);
                *old = level;
            }
        }
        envelope.push(flux);
    }
    envelope
}

fn correlation(onsets: &[f32], lag: usize) -> f32 {
    if lag >= onsets.len() {
        return 0.0;
    }
    let (dot, left, right) = onsets
        .iter()
        .zip(&onsets[lag..])
        .fold((0.0, 0.0, 0.0), |(dot, left, right), (&a, &b)| {
            (dot + a * b, left + a * a, right + b * b)
        });
    dot / (left * right).sqrt().max(f32::EPSILON)
}

fn track_period(flux: &[f32], full_length: bool) -> Option<TempoEstimate> {
    let frames_per_beat = 60.0 * f64::from(RATE) / HOP as f64;
    let min_lag = (frames_per_beat / MAX_BPM) as usize;
    let max_lag = (frames_per_beat / MIN_BPM) as usize;
    if flux.len() < 2 * min_lag {
        return None;
    }
    let peak = flux.iter().copied().fold(0.0, f32::max);
    if peak < 1e-4 {
        return None;
    }
    let onsets: Vec<_> = flux.iter().map(|&f| (f / peak - 0.10).max(0.0)).collect();
    let mut attacks = Vec::new();
    for (i, w) in onsets.windows(3).enumerate() {
        if w[1] > 0.14 && w[1] > w[0] && w[1] >= w[2] {
            // Group nearby maxima from one transient.
            if attacks.last().is_none_or(|&last| i - last >= 11) {
                attacks.push(i);
            }
        }
    }
    if attacks.len() < 3 {
        return None;
    }
    let mut gaps: Vec<_> = attacks.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable();
    let median = gaps[gaps.len() / 2] as f32;
    let ac: Vec<_> = (0..=2 * max_lag + 1)
        .map(|lag| correlation(&onsets, lag))
        .collect();
    let mut candidates = Vec::new();
    for lag in min_lag..=max_lag.min(onsets.len() / 2) {
        if ac[lag] < ac[lag - 1] || ac[lag] < ac[lag + 1] {
            continue;
        }
        let a = ac[lag - 1];
        let b = ac[lag];
        let c = ac[lag + 1];
        let bend = a - 2.0 * b + c;
        let delta = if bend < -1e-6 {
            (0.5 * (a - c) / bend).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let period = lag as f32 + delta;
        let regularity = (0.8 * b + 0.2 * ac[2 * lag]).clamp(0.0, 1.0);
        // Correlation can mistake an accent pattern for the beat, or reward one coincidental
        // pair in sparse audio. Require most consecutive attacks to support the candidate.
        let supported = gaps
            .iter()
            .filter(|&&gap| {
                let beats = gap as f32 / period;
                beats >= 0.85 && (beats - beats.round()).abs() <= 0.15
            })
            .count();
        let coverage = supported as f32 / gaps.len() as f32;
        if coverage < 0.60 {
            continue;
        }
        let interval_prior = (-(period / median).ln().abs() * 3.0).exp();
        let beats = onsets.len() as f32 / period;
        let bars = beats / 4.0;
        let loop_prior = if full_length && beats >= 3.9 {
            (1.0 - (bars - bars.round()).abs() * 5.0).max(0.0)
        } else {
            0.0
        };
        let score = regularity + 0.20 * interval_prior + 0.06 * loop_prior;
        candidates.push((score, regularity, period));
    }
    candidates.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let (score, regularity, period) = *candidates.first()?;
    if regularity < 0.20 {
        return None;
    }
    let second = candidates.get(1).map_or(0.0, |c| c.0);
    let confidence = (0.75 * regularity + (score - second).clamp(0.0, 0.25)).clamp(0.0, 1.0);
    let bpm = (frames_per_beat / f64::from(period)).clamp(MIN_BPM, MAX_BPM);
    Some(TempoEstimate {
        bpm: (bpm * 100.0).round() / 100.0,
        confidence,
    })
}

fn cache_path(dir: &Path, id: &FileId) -> std::path::PathBuf {
    dir.join(format!("{:016x}.tempo", id.stable_hash()))
}

pub(crate) fn cached(dir: &Path, id: &FileId) -> Option<Option<TempoEstimate>> {
    let mut data = Vec::with_capacity(21);
    fs::File::open(cache_path(dir, id))
        .ok()?
        .take(21)
        .read_to_end(&mut data)
        .ok()?;
    if data.get(..8)? != CACHE_MAGIC {
        return None;
    }
    if data.len() == 8 {
        return Some(None);
    }
    let bpm = f64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
    let confidence = f32::from_le_bytes(data.get(16..20)?.try_into().ok()?);
    (data.len() == 20 && (MIN_BPM..=MAX_BPM).contains(&bpm) && (0.0..=1.0).contains(&confidence))
        .then_some(Some(TempoEstimate { bpm, confidence }))
}

pub(crate) fn cache(dir: &Path, id: &FileId, result: Option<TempoEstimate>) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = cache_path(dir, id);
    let temp = path.with_extension(format!("tempo.{}.tmp", std::process::id()));
    let mut data = CACHE_MAGIC.to_vec();
    if let Some(result) = result {
        data.extend_from_slice(&result.bpm.to_le_bytes());
        data.extend_from_slice(&result.confidence.to_le_bytes());
    }
    let saved = fs::write(&temp, data).and_then(|_| fs::rename(&temp, path));
    if saved.is_err() {
        let _ = fs::remove_file(&temp);
    }
    saved
}
