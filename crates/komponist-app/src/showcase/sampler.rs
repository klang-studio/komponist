//! The demo plays one synthesized sample as a tempo-synced instrument and a bank of MIDI slices.

use std::f64::consts::TAU;
use std::io;
use std::path::Path;
use std::sync::Arc;

use komponist_builtin::Internal;
use komponist_general_sampler::{Settings, write_settings};
use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{Hue, Project, Tick};
use komponist_sampler::{SampleLink, links};

use super::{DeviceSource, log_position};

const SAMPLE_FILE: &str = "Glass Pluck.wav";
const RATE: u32 = 48_000;
const SOURCE_BPM: f64 = 120.0;
const ROOT: u8 = 48;
const ROOT_HZ: f64 = 130.812_782_65;
const MELODY_TRACK: &str = "Glass Pluck";
const SLICE_TRACK: &str = "Glass Slices";
const HIT_POSITIONS: [f64; 6] = [0.0, 0.75, 1.5, 2.5, 3.0, 3.5];

pub(super) fn add_tracks(project: &mut Project, source: DeviceSource<'_>) -> Result<(), String> {
    for (name, hue, clip_name, bar, pattern) in [
        (
            MELODY_TRACK,
            Hue::VERMILION,
            "Glass Melody",
            4,
            [
                (0, 7, 72, 82),
                (9, 4, 76, 70),
                (16, 6, 79, 88),
                (26, 5, 76, 74),
            ],
        ),
        (
            SLICE_TRACK,
            Hue::VIOLET,
            "Glass Fill",
            7,
            [
                (2, 2, 48, 84),
                (7, 2, 51, 73),
                (11, 2, 49, 95),
                (14, 2, 53, 81),
            ],
        ),
    ] {
        let melody = name == MELODY_TRACK;
        let values = sampler_values(melody);
        let mut prepared = source.prepare("general-sampler", None, &values, &[])?;
        if !melody {
            let state = prepared
                .state
                .as_deref()
                .ok_or("missing sampler demo state")?;
            let times = HIT_POSITIONS[1..].iter().copied();
            let state = match source {
                DeviceSource::Packaged(_) => {
                    komponist_plugins::bundled_sampler::edit_slice_markers(state, times)
                        .ok_or("could not initialize the sampler's slice map")?
                }
                DeviceSource::LegacyFixture => {
                    write_settings(Some(state), &Settings::from_markers(times))
                }
            };
            prepared.state = Some(state.into());
        }
        let clip_bars = if name == MELODY_TRACK { 2 } else { 1 };
        let (next, track) = edits::add_track(
            project,
            NewTrack {
                name: Some(name.into()),
                hue: Some(hue),
                instrument: Some(prepared.plugin.clone()),
                index: None,
                arm_exclusive: false,
            },
        )
        .expect("showcase sampler track");
        let notes = pattern.map(|(step, duration, pitch, velocity)| NewNote {
            start: Tick::from_steps(step),
            length: Tick::from_steps(duration),
            pitch,
            velocity,
        });
        let (mut next, _) = edits::add_clip(
            &next,
            track,
            NewClip {
                start: Tick::from_steps(bar * 16),
                length: Tick::from_steps(clip_bars * 16),
                name: clip_name.into(),
                content: NewClipContent::Midi {
                    notes: notes.to_vec(),
                    take_name: String::new(),
                },
            },
        )
        .expect("showcase sampler clip");
        let track = next
            .tracks
            .iter_mut()
            .find(|candidate| candidate.id == track)
            .expect("showcase sampler track");
        let track = Arc::make_mut(track);
        track.channel.volume_db = -24.0;
        track.channel.pan = if name == MELODY_TRACK { -0.14 } else { 0.14 };
        let id = track.channel.devices[0].id;
        track.channel.devices[0] = Arc::new(prepared.into_device(id));
        *project = next;
    }
    Ok(())
}

/// Values in the bundle's published CLAP parameter ranges. The time knobs are normalized.
fn sampler_values(melody: bool) -> Vec<(&'static str, f64)> {
    let mut values = vec![
        ("root", f64::from(ROOT)),
        ("source_bpm", (SOURCE_BPM - 40.0) / 200.0),
        ("filter", 1.0),
        ("cutoff", log_position(3100.0, 20.0, 20000.0)),
        ("resonance", 0.18),
        ("filter_env", (0.28 + 8.0) / 16.0),
        ("filter_decay", 190.0 / 10000.0),
        ("lfo_rate", log_position(0.70, 0.05, 30.0)),
        ("lfo_target", 2.0),
        ("lfo_depth", (0.08 + 1.0) / 2.0),
        ("velocity_target", 2.0),
        ("velocity_depth", (0.12 + 1.0) / 2.0),
        ("amp_attack", 6.0 / 5000.0),
        ("amp_decay", 250.0 / 10000.0),
        ("amp_sustain", 0.72),
        ("amp_release", 130.0 / 10000.0),
    ];
    values.extend(if melody {
        [
            ("mode", 1.0),
            ("sync", 1.0),
            ("end", 0.5 / 4.0),
            ("fade_out", 12.0 / 10000.0),
        ]
    } else {
        [
            ("mode", 2.0),
            ("direction", 1.0),
            ("fade_in", 8.0 / 10000.0),
            ("fade_out", 10.0 / 10000.0),
        ]
    });
    values
}

/// Writes the source of the two showcase tracks and links it without replacing their instruments.
pub(crate) fn write_sampler_file(project: &mut Project, dir: &Path) -> io::Result<()> {
    let uses_sample = |name: &str, device: &komponist_model::Device| {
        matches!(name, MELODY_TRACK | SLICE_TRACK)
            && Internal::from_plugin(&device.plugin) == Some(Internal::GeneralSampler)
    };
    if !project.tracks.iter().any(|track| {
        track
            .channel
            .devices
            .iter()
            .any(|device| uses_sample(&track.name, device))
    }) {
        return Ok(());
    }

    let path = write_sample(dir)?;
    let link = SampleLink::new(&path, None);
    for track in &mut project.tracks {
        let track = Arc::make_mut(track);
        for device in &mut track.channel.devices {
            if uses_sample(&track.name, device) {
                let state = links::write(device.state.as_deref(), 0, Some(&link));
                Arc::make_mut(device).state = Some(state.into());
            }
        }
    }
    Ok(())
}

pub(super) fn write_packaged_sample(project: &mut Project, dir: &Path) -> io::Result<()> {
    let path = write_sample(dir)?;
    let link = AssetLink::new(&path, Some(dir));
    for track in &mut project.tracks {
        if matches!(track.name.as_str(), MELODY_TRACK | SLICE_TRACK) {
            let device = &mut Arc::make_mut(track).channel.devices[0];
            Arc::make_mut(device).asset_links.insert(0, link.clone());
        }
    }
    Ok(())
}

fn write_sample(dir: &Path) -> io::Result<std::path::PathBuf> {
    let path = dir.join(SAMPLE_FILE);
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(io::Error::other)?;
    for frame in 0..4 * RATE {
        let t = f64::from(frame) / f64::from(RATE);
        let value = HIT_POSITIONS
            .iter()
            .zip([1.0, 0.66, 0.88, 0.58, 0.81, 0.63])
            .filter(|(hit, _)| t >= **hit)
            .map(|(hit, velocity)| {
                let age = t - hit;
                let phase = TAU * ROOT_HZ * age;
                let attack = (age / 0.003).min(1.0);
                let tone = 0.42 * (-age * 5.7).exp() * phase.sin()
                    + 0.17 * (-age * 9.0).exp() * (phase * 2.01).sin()
                    + 0.10 * (-age * 22.0).exp() * (phase * 5.43).sin();
                velocity * attack * tone
            })
            .sum::<f64>();
        writer
            .write_sample((value.clamp(-1.0, 1.0) * 8_388_607.0) as i32)
            .map_err(io::Error::other)?;
    }
    writer.finalize().map_err(io::Error::other)?;

    Ok(path)
}
