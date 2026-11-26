//! The demo plays one synthesized sample as a tempo-synced instrument and a bank of MIDI slices.

use std::f64::consts::TAU;
use std::io;
use std::path::Path;
use std::sync::Arc;

use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{Hue, PluginRef, Project, Tempo, Tick, TrackId};
use komponist_plugins::bundled_initial_state::InitialMarkers;

use super::{log_position, prepare_device};

const SAMPLE_FILE: &str = "Glass Pluck.wav";
const RATE: u32 = 48_000;
const SOURCE_BPM: f64 = 120.0;
const ROOT: u8 = 48;
const ROOT_HZ: f64 = 130.812_782_65;
const MELODY_TRACK: &str = "Glass Pluck";
const SLICE_TRACK: &str = "Glass Slices";
const HIT_POSITIONS: [f64; 6] = [0.0, 0.75, 1.5, 2.5, 3.0, 3.5];

struct GlassTrack {
    name: &'static str,
    hue: Hue,
    clip: &'static str,
    bar: i64,
    clip_bars: i64,
    notes: [(i64, i64, u8, u8); 4],
}

const GLASS_TRACKS: [GlassTrack; 2] = [
    GlassTrack {
        name: MELODY_TRACK,
        hue: Hue::VERMILION,
        clip: "Glass Melody",
        bar: 4,
        clip_bars: 2,
        notes: [
            (0, 7, 72, 82),
            (9, 4, 76, 70),
            (16, 6, 79, 88),
            (26, 5, 76, 74),
        ],
    },
    GlassTrack {
        name: SLICE_TRACK,
        hue: Hue::VIOLET,
        clip: "Glass Fill",
        bar: 7,
        clip_bars: 1,
        notes: [
            (2, 2, 48, 84),
            (7, 2, 51, 73),
            (11, 2, 49, 95),
            (14, 2, 53, 81),
        ],
    },
];

impl GlassTrack {
    fn melody(&self) -> bool {
        self.name == MELODY_TRACK
    }

    fn markers(&self) -> &'static [f64] {
        if self.melody() {
            &[]
        } else {
            &HIT_POSITIONS[1..]
        }
    }

    fn add(&self, project: &Project, instrument: PluginRef) -> (Project, TrackId) {
        let (next, id) = edits::add_track(
            project,
            NewTrack {
                name: Some(self.name.into()),
                hue: Some(self.hue),
                instrument: Some(instrument),
                index: None,
                arm_exclusive: false,
            },
        )
        .expect("showcase sampler track");
        let notes = self.notes.map(|(step, duration, pitch, velocity)| NewNote {
            start: Tick::from_steps(step),
            length: Tick::from_steps(duration),
            pitch,
            velocity,
        });
        let (mut next, _) = edits::add_clip(
            &next,
            id,
            NewClip {
                start: Tick::from_steps(self.bar * 16),
                length: Tick::from_steps(self.clip_bars * 16),
                name: self.clip.into(),
                content: NewClipContent::Midi {
                    notes: notes.to_vec(),
                    take_name: String::new(),
                },
            },
        )
        .expect("showcase sampler clip");
        let index = next.track_index(id).expect("showcase sampler track");
        let track = Arc::make_mut(&mut next.tracks[index]);
        track.channel.volume_db = -24.0;
        track.channel.pan = if self.melody() { -0.14 } else { 0.14 };
        (next, id)
    }
}

/// Adds both Glass tracks. The slice track's markers are placed on the generated sample through
/// the bundle's waveform-marker extension; the project links the sample as a host-managed file.
pub(super) fn add_tracks(
    project: &mut Project,
    host: &crate::device_host::PluginHost,
    dir: &Path,
) -> Result<(), String> {
    let path = write_sample(dir).map_err(|error| error.to_string())?;
    let link = AssetLink::new(&path, Some(dir));
    for glass in GLASS_TRACKS {
        let values = sampler_values(glass.melody());
        let prepared = if glass.markers().is_empty() {
            prepare_device(host, "general-sampler", None, &values, &[])?
        } else {
            host.manager().prepare_bundled_device_with_markers(
                "org.klang-studio.komponist.general-sampler",
                &values,
                InitialMarkers {
                    slot: 0,
                    file: &path,
                    positions: glass.markers(),
                },
            )?
        };
        let (mut next, id) = glass.add(project, prepared.plugin.clone());
        let index = next.track_index(id).expect("showcase sampler track");
        let track = Arc::make_mut(&mut next.tracks[index]);
        let device = &mut track.channel.devices[0];
        let mut prepared = prepared.into_device(device.id);
        prepared.asset_links.insert(0, link.clone());
        *device = Arc::new(prepared);
        *project = next;
    }
    Ok(())
}

/// Glass melody or slices, with a host-managed sample and authored CLAP setup values.
#[doc(hidden)]
pub struct GeneralSamplerFixture {
    pub project: Project,
    pub params: Vec<(&'static str, f64)>,
    pub markers: &'static [f64],
}

/// Builds only the requested Glass track, using the supplied scanned sampler.
#[doc(hidden)]
pub fn general_sampler_fixture(
    name: &str,
    plugin: PluginRef,
    dir: &Path,
) -> io::Result<GeneralSamplerFixture> {
    let glass = GLASS_TRACKS
        .iter()
        .find(|glass| glass.name == name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown Glass track"))?;
    let mut project = Project::new(komponist_model::fixtures::DEMO_PROJECT_NAME);
    project.tempo = Tempo::new(124.0);
    let (mut project, _) = glass.add(&project, plugin);
    super::add_master_fade(&mut project);
    let track = Arc::make_mut(&mut project.tracks[0]);
    let device = Arc::make_mut(&mut track.channel.devices[0]);
    device
        .asset_links
        .insert(0, AssetLink::new(&write_sample(dir)?, None));
    Ok(GeneralSamplerFixture {
        project,
        params: sampler_values(glass.melody()),
        markers: glass.markers(),
    })
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
