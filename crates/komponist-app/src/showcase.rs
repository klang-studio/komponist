//! The app's demo project (`komponist --demo`): "Nachtfahrt" with built-in devices only.
//!
//! The song is [`komponist_model::fixtures::demo_song`], the same tempo, loop, tracks, clips and
//! notes as the design fixture `demo_project()` (which keeps its third-party plugins for the
//! missing-plugin tests). Only the device chains differ, and the Arp track plays held chords for
//! the Arpeggiator, so the demo opens without "Plugin fehlt" and shows the built-in devices:
//!
//! | Track | Volume | Chain (preset) |
//! |---|---|---|
//! | Drums | −6 dB | Sampler (test kit), presse („Drums fest“) |
//! | Bass | −4 dB | knurr („Fette Säge“), EQ („Bass Wärme“) |
//! | Lead | −3 dB | pisynth („Saw-Lead“), EQ („Low Cut 80 Hz“), Delay („Punktierte Achtel“), Hall („Studio-Platte“) |
//! | Pad | −9 dB | knurr („Breiter Pad“), EQ („Low Cut 80 Hz“), Hall („Große Halle“) |
//! | Keys | −8 dB | pisynth („E-Piano“), Delay („Weiches Band“) |
//! | Arp | −10 dB | Tonleiter („A-Moll-Pentatonik“), Arpeggiator („Sechzehntel Auf/Ab“), pisynth („Synth-Pluck“), Delay („Achtel Ping-Pong“) |
//! | Master | 0 dB | Limiter (ceiling −0.3 dB) |
//!
//! Device states are written like the presets of the UI do: `{"params": …, "preset": "<name>"}`
//! ([`Internal::preset_state`]); pisynth's state is the factory patch.

use std::sync::Arc;

use komponist_builtin::{Internal, encode_state, preset_patch, presets as pisynth_presets};
use komponist_model::fixtures::demo_song;
use komponist_model::{
    ClipContent, Device, DeviceId, DeviceRole, MidiClip, Note, NoteId, Project, Take, Tick,
};

/// Volumes of the tracks in dB, in track order (Drums, Bass, Lead, Pad, Keys, Arp), set by
/// render analysis: the built-in instruments are louder than the old plugin chain's.
const VOLUMES_DB: [f32; 6] = [-16.0, -9.0, 0.0, -10.0, 0.0, 0.0];

/// The chords of the Arp clip, one per bar: C, C, Am, F, C, C, Am, G (MIDI pitches).
const CHORDS: [[u8; 3]; 8] = [
    [60, 64, 67],
    [60, 64, 67],
    [57, 60, 64],
    [57, 60, 65],
    [60, 64, 67],
    [60, 64, 67],
    [57, 60, 64],
    [55, 59, 62],
];

/// Velocity of the Arp chords.
const CHORD_VELOCITY: u8 = 84;

/// Allocates ids and builds the devices.
struct Devices {
    next: u64,
}

impl Devices {
    fn take(&mut self) -> DeviceId {
        let id = DeviceId(self.next);
        self.next += 1;
        id
    }

    /// An internal device with factory preset `preset`.
    fn internal(&mut self, kind: Internal, preset: &str) -> Arc<Device> {
        let index = kind
            .presets()
            .iter()
            .position(|p| p.name == preset)
            .unwrap_or_else(|| panic!("{} has no preset {preset}", kind.name()));
        let state = kind.preset_state(None, index).expect("the preset exists");
        self.with_state(kind, state)
    }

    /// An internal device with its default parameters (no preset name).
    fn plain(&mut self, kind: Internal, changes: &[(&str, f64)]) -> Arc<Device> {
        let mut state = kind.encode_state(&kind.defaults());
        for (key, value) in changes {
            let wire = kind
                .wire(&komponist_model::params::ParamId::new(*key))
                .unwrap_or_else(|| panic!("{} has no parameter {key}", kind.name()));
            state = kind
                .with_value(Some(&state), wire, *value)
                .expect("a known wire id");
        }
        self.with_state(kind, state)
    }

    fn with_state(&mut self, kind: Internal, state: Vec<u8>) -> Arc<Device> {
        Arc::new(Device {
            id: self.take(),
            role: kind.role(),
            plugin: kind.plugin_ref(),
            bypass: false,
            state: Some(state.into()),
            sidechain: None,
        })
    }

    /// The sampler without state: `samples::demo_with_kit` puts the test kit on its pads.
    fn sampler(&mut self) -> Arc<Device> {
        Arc::new(Device {
            id: self.take(),
            role: DeviceRole::Instrument,
            plugin: Internal::Sampler.plugin_ref(),
            bypass: false,
            state: None,
            sidechain: None,
        })
    }

    /// pisynth with factory preset `preset`.
    fn pisynth(&mut self, preset: &str) -> Arc<Device> {
        let info = pisynth_presets()
            .find(|p| p.name == preset)
            .unwrap_or_else(|| panic!("pisynth has no preset {preset}"));
        let patch = preset_patch(info.index).expect("a listed preset exists");
        Arc::new(Device {
            id: self.take(),
            role: DeviceRole::Instrument,
            plugin: crate::app::pisynth_ref(),
            bypass: false,
            state: Some(encode_state(&patch).into()),
            sidechain: None,
        })
    }
}

/// The demo project: see the module docs.
pub fn showcase_project() -> Project {
    let mut project = demo_song();
    let mut d = Devices {
        next: project.next_id,
    };
    let chains = [
        vec![d.sampler(), d.internal(Internal::Presse, "Drums fest")],
        vec![
            d.internal(Internal::Knurr, "Fette Säge"),
            d.internal(Internal::Eq, "Bass Wärme"),
        ],
        vec![
            d.pisynth("Saw-Lead"),
            d.internal(Internal::Eq, "Low Cut 80 Hz"),
            d.internal(Internal::Delay, "Punktierte Achtel"),
            d.internal(Internal::Hall, "Studio-Platte"),
        ],
        vec![
            d.internal(Internal::Knurr, "Breiter Pad"),
            d.internal(Internal::Eq, "Low Cut 80 Hz"),
            d.internal(Internal::Hall, "Große Halle"),
        ],
        vec![
            d.pisynth("E-Piano"),
            d.internal(Internal::Delay, "Weiches Band"),
        ],
        vec![
            d.internal(Internal::Tonleiter, "A-Moll-Pentatonik"),
            d.internal(Internal::Arpeggiator, "Sechzehntel Auf/Ab"),
            d.pisynth("Synth-Pluck"),
            d.internal(Internal::Delay, "Achtel Ping-Pong"),
        ],
    ];
    let master = vec![d.plain(Internal::Limiter, &[("gain", 3.0), ("ceiling", -0.3)])];
    for ((track, chain), volume) in project.tracks.iter_mut().zip(chains).zip(VOLUMES_DB) {
        let track = Arc::make_mut(track);
        track.channel.devices = chain;
        track.channel.volume_db = volume;
    }
    Arc::make_mut(&mut project.master).devices = master;

    // The Arp track plays held chords: the Arpeggiator turns them into the pattern.
    let mut notes = Vec::new();
    for (bar, chord) in CHORDS.iter().enumerate() {
        for &pitch in chord {
            notes.push(Note {
                id: NoteId(d.take().0),
                start: Tick::from_steps(bar as i64 * 16),
                length: Tick::from_steps(16),
                pitch,
                velocity: CHORD_VELOCITY,
            });
        }
    }
    let arp = Arc::make_mut(&mut project.tracks[5]);
    let clip = Arc::make_mut(&mut arp.clips[0]);
    clip.content = ClipContent::Midi(MidiClip {
        takes: vec![Take {
            name: String::new(),
            notes: Arc::new(notes),
        }],
        active_take: 0,
    });
    project.next_id = d.next;
    project
}
