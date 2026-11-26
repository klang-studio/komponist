//! Synthesized audio in the demo (audio proposal §9): Re-Pitch, gain, fades and mono/stereo.
//! Building the project only creates links; `showcase::demo_with_kit` writes the WAV files.
//!
//! `showcase::render_levels`, macOS arm64, 2026-11-17: Shaker peak −18.4 dBFS, Sweep −18.8;
//! full mix before the Limiter −2.1, after it −0.3 dBFS (RMS −14.6). With the Limiter's 3 dB
//! input gain, its peak reduction is about 1.2 dB; the render test bounds it at 2 dB.

use std::io;
use std::path::Path;
use std::sync::Arc;

use komponist_model::edits::{self, AudioClipProp, NewAudioTrack, NewClip, NewClipContent};
use komponist_model::{
    AudioFile, ClipContent, ClipTimeMode, Hue, Project, Tempo, Tick, TrackOutput,
};

const RATE: u32 = 48_000;
const BAR: i64 = 3840;
const SHAKER: &str = "Shaker Loop.wav";
const RISER: &str = "Riser.wav";
const PULSE: &str = "Pulse 90.wav";
/// Eight beats at 90 BPM, stereo 48 kHz. Warp follows the existing showcase's 124 BPM.
const PULSE_FRAMES: u64 = 256000;
const ORIGINAL_BPM: f64 = 120.0;
const SHAKER_FRAMES: u64 = 16 * RATE as u64;

fn riser_frames(project: &Project) -> u64 {
    project
        .tempo_map()
        .tick_to_samples(Tick(2 * BAR), f64::from(RATE))
        .ceil() as u64
}

/// Adds the approved arrangement without accessing the filesystem.
pub(super) fn add_tracks(project: &mut Project) {
    let rhythm = project
        .tracks
        .iter()
        .find(|t| t.name == super::GROUP_NAME)
        .unwrap()
        .id;
    let spec = NewAudioTrack {
        name: Some("Shaker".into()),
        hue: Some(Hue::TEAL),
        ..Default::default()
    };
    let (p, shaker) = edits::add_audio_track(project, spec).unwrap();
    let p = edits::set_track_prop(&p, &[shaker], edits::TrackProp::InputGain(60)).unwrap();
    *project = edits::set_output(&p, &[shaker], TrackOutput::Bus(rhythm)).unwrap();
    let file = AudioFile::new(Path::new(SHAKER), None, SHAKER_FRAMES, RATE, 1);
    for (bar, bars, gain, fade_in, fade_out) in [
        (0, 8, 0.0, 0, 0),
        (8, 4, -3.0, 0, BAR),
        (12, 4, -3.0, BAR / 2, 0),
    ] {
        let (p, clip) = edits::add_clip(
            project,
            shaker,
            NewClip {
                start: Tick(bar * BAR),
                length: Tick(bars * BAR),
                name: "Shaker Loop".into(),
                content: NewClipContent::Audio {
                    file: file.clone(),
                    offset: 0,
                },
            },
        )
        .unwrap();
        *project = p;
        for prop in [
            AudioClipProp::OriginalTempo(Tempo { bpm: ORIGINAL_BPM }),
            AudioClipProp::TimeMode(ClipTimeMode::RePitch),
            AudioClipProp::GainDb(gain),
            AudioClipProp::FadeIn(Tick(fade_in)),
            AudioClipProp::FadeOut(Tick(fade_out)),
        ] {
            *project = edits::set_audio_clip_prop(project, clip, prop).unwrap();
        }
    }
    let spec = NewAudioTrack {
        name: Some("Sweep".into()),
        hue: Some(Hue::ORANGE),
        ..Default::default()
    };
    let (p, sweep) = edits::add_audio_track(project, spec).unwrap();
    *project = p;
    let (p, clip) = edits::add_clip(
        project,
        sweep,
        NewClip {
            start: Tick(6 * BAR),
            length: Tick(2 * BAR),
            name: "Riser".into(),
            content: NewClipContent::Audio {
                file: AudioFile::new(Path::new(RISER), None, riser_frames(project), RATE, 2),
                offset: 0,
            },
        },
    )
    .unwrap();
    *project = p;
    for prop in [
        AudioClipProp::GainDb(2.0),
        AudioClipProp::FadeIn(Tick(BAR / 2)),
    ] {
        *project = edits::set_audio_clip_prop(project, clip, prop).unwrap();
    }
    let (p, pulse) = edits::add_audio_track(
        project,
        NewAudioTrack {
            name: Some("Pulse 90".into()),
            hue: Some(Hue::GREEN),
            ..Default::default()
        },
    )
    .unwrap();
    let (p, clip) = edits::add_clip(
        &p,
        pulse,
        NewClip {
            start: Tick(4 * BAR),
            length: Tick(2 * BAR),
            name: "Pulse 90".into(),
            content: NewClipContent::Audio {
                file: AudioFile::new(Path::new(PULSE), None, PULSE_FRAMES, RATE, 2),
                offset: 0,
            },
        },
    )
    .unwrap();
    let p = edits::set_audio_clip_prop(&p, clip, AudioClipProp::OriginalTempo(Tempo::new(90.0)))
        .unwrap();
    *project = edits::set_audio_clip_warp(&p, clip, true).unwrap();
    for track in &mut project.tracks {
        if track.id == pulse {
            Arc::make_mut(track).channel.volume_db = -24.0;
        }
        if track.id == shaker || track.id == sweep {
            Arc::make_mut(track).channel.volume_db = -12.0;
        }
    }
}

/// Writes only the demo links present in this project; other sampler fixtures stay unchanged.
pub(crate) fn write_files(project: &mut Project, dir: &Path) -> io::Result<()> {
    for (name, frames, channels) in [
        (SHAKER, SHAKER_FRAMES, 1),
        (RISER, riser_frames(project), 2),
        (PULSE, PULSE_FRAMES, 2),
    ] {
        let uses_file = |audio: &komponist_model::AudioClip| {
            audio.file.abs.file_name().is_some_and(|f| f == name)
        };
        if !project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .any(|c| matches!(&c.content, ClipContent::Audio(a) if uses_file(a)))
        {
            continue;
        }
        let path = dir.join(name);
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels,
                sample_rate: RATE,
                bits_per_sample: 24,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .map_err(io::Error::other)?;
        let mut noise = Noise(0x7831_abc9);
        let mut low = [0.0; 2];
        let mut high = [0.0; 2];
        for frame in 0..frames {
            let progress = frame as f32 / frames as f32;
            for channel in 0..usize::from(channels) {
                let raw = noise.next();
                let sample = if name == PULSE {
                    let seconds = frame as f64 / f64::from(RATE);
                    let age = (frame % 32000) as f64 / f64::from(RATE);
                    let attack = (age / 0.002).min(1.0);
                    let transient = 0.42
                        * (-age / 0.035).exp()
                        * attack
                        * (std::f64::consts::TAU * 95.0 * age).sin();
                    let carrier = 0.2 * (std::f64::consts::TAU * 440.0 * seconds).sin();
                    let detail = 0.015
                        * (std::f64::consts::TAU
                            * (if channel == 0 { 660.0 } else { 880.0 })
                            * seconds)
                            .sin();
                    (carrier + transient + detail) as f32
                } else if name == SHAKER {
                    // Eight eighth-note bursts per bar, accented on beats 1 and 3.
                    let eighth = RATE / 4;
                    let hit = frame / u64::from(eighth);
                    let age = (frame % u64::from(eighth)) as f32 / RATE as f32;
                    low[channel] += 0.35 * (raw - low[channel]);
                    let accent = if hit.is_multiple_of(4) { 1.0 } else { 0.65 };
                    let attack = (age / 0.002).min(1.0);
                    (raw - low[channel]) * (-age / 0.025).exp() * attack * accent * 0.55
                } else {
                    // Difference of two one-pole low-passes: a stable rising band-pass.
                    let cutoff = 250.0 * 16.0_f32.powf(progress);
                    let coefficient =
                        |hz: f32| 1.0 - (-std::f32::consts::TAU * hz / RATE as f32).exp();
                    low[channel] += coefficient(cutoff * 0.5) * (raw - low[channel]);
                    high[channel] += coefficient(cutoff * 2.0) * (raw - high[channel]);
                    (high[channel] - low[channel]) * (0.1 + 0.6 * progress) * 0.8
                };
                writer
                    .write_sample((sample.clamp(-1.0, 1.0) * 8_388_607.0) as i32)
                    .map_err(io::Error::other)?;
            }
        }
        writer.finalize().map_err(io::Error::other)?;
        for track in &mut project.tracks {
            for clip in &mut Arc::make_mut(track).clips {
                if let ClipContent::Audio(audio) = &mut Arc::make_mut(clip).content
                    && uses_file(audio)
                {
                    audio.file = AudioFile::new(&path, None, frames, RATE, channels);
                }
            }
        }
    }
    Ok(())
}

struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}
