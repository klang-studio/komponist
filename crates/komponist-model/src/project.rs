//! The project document tree (ARCHITECTURE §5.2).
//!
//! The document is a tree of `Arc`s. Edits ([`crate::edits`]) clone the path to the changed node
//! with `Arc::make_mut` and return a new root, so unchanged subtrees stay shared between undo
//! snapshots and the engine compiler can detect changes with `Arc::ptr_eq` per track, clip and
//! device.
//!
//! Only what must survive a save lives here. Runtime state (device status "lädt / abgestürzt /
//! fehlt", playhead, selection, zoom) belongs to `komponist-plugins`, the engine and the app.
//!
//! Units used throughout: positions and lengths in [`Tick`]s (960 PPQ); MIDI pitch and velocity
//! `0 ..= 127`; volume in dB; pan `-1.0 ..= 1.0` (left … right).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{ClipId, DeviceId, NoteId, TrackId};
use crate::time::{Meter, StepResolution, Tempo, TempoMap, Tick, TickRange};

/// Name of a new, empty project (S10, prototype `newProject`).
pub const DEFAULT_PROJECT_NAME: &str = "Unbenannt";

/// Lowest track height in design px (`size.track.min` in `design/tokens.json`).
pub const TRACK_HEIGHT_MIN: u16 = 32;
/// Highest track height in design px (`size.track.max`).
pub const TRACK_HEIGHT_MAX: u16 = 160;
/// Height of a new track in design px (`size.track.default`).
pub const TRACK_HEIGHT_DEFAULT: u16 = 56;

/// Lowest stored channel volume in dB. This value means −∞ (silence): the fader bottom, shown as
/// "−∞" by [`fmt_db`](crate::format::fmt_db). JSON cannot store infinities, so −∞ is never stored.
pub const VOLUME_DB_MIN: f32 = -70.0;
/// Highest channel volume in dB (fader top, `DBT` in `ks-engine.js`).
pub const VOLUME_DB_MAX: f32 = 6.0;

/// Number of steps a step row stores. Patterns use the first [`StepClip::length_steps`] of them;
/// the rest keep their values while hidden.
pub const MAX_STEPS: usize = 32;

/// Default rows of a new step clip for instruments without CLAP note names: the prototype's GM drum
/// rows (`PADS` / `DRUM_P` in `ks-engine.js`), as `(name, MIDI note)`.
pub const DEFAULT_STEP_ROWS: [(&str, u8); 8] = [
    ("Kick", 36),
    ("Snare", 38),
    ("Clap", 39),
    ("Hat zu", 42),
    ("Hat offen", 46),
    ("Tom", 45),
    ("Rim", 37),
    ("Perc", 47),
];

/// Catalog id of the built-in pisynth instrument (in process, `komponist-builtin`).
pub const BUILTIN_PISYNTH: &str = "pisynth";
/// Catalog id of the built-in klopfer drum sampler (the bundled `Klopfer.clap`, hosted through the
/// normal bridge, shown as "INTERN"; decision D5).
pub const BUILTIN_KLOPFER: &str = "klopfer";

/// A whole project: the root of the document tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Project name, shown in the transport bar; also the default export file name.
    pub name: String,
    /// Constant tempo (v0.1).
    pub tempo: Tempo,
    /// Constant meter (v0.1).
    pub meter: Meter,
    /// Loop range ("Loop 9–13"); bar-aligned when set from the ruler, any ticks otherwise.
    pub loop_range: TickRange,
    /// Whether playback loops over [`Project::loop_range`] (key `L`).
    pub loop_on: bool,
    /// Metronome click during playback (key `C`). The click always sounds during the count-in.
    pub metronome: bool,
    /// Bars of count-in before recording ("Vorzähler 1"); 0 = off.
    pub count_in_bars: u8,
    /// Tracks in display order (top to bottom).
    pub tracks: Vec<Arc<Track>>,
    /// The master strip: volume, pan and master inserts (`devices` holds effects only).
    pub master: Arc<Channel>,
    /// Next free id for [`TrackId`], [`ClipId`], [`NoteId`] and [`DeviceId`] (one counter for all
    /// kinds). Only grows.
    pub next_id: u64,
}

impl Project {
    /// An empty project named `name`: 120 bpm, 4/4, loop off over bars 1–5 (ticks 0 .. 15360,
    /// prototype `newProject`), metronome off, count-in 1 bar, no tracks, master at 0 dB, centred.
    pub fn new(name: impl Into<String>) -> Project {
        Project {
            name: name.into(),
            tempo: Tempo::DEFAULT,
            meter: Meter::FOUR_FOUR,
            loop_range: TickRange::new(Tick::ZERO, Tick::from_steps(64)),
            loop_on: false,
            metronome: false,
            count_in_bars: 1,
            tracks: Vec::new(),
            master: Arc::new(Channel::default()),
            next_id: 1,
        }
    }

    /// The tempo map for tick ↔ time conversions.
    pub fn tempo_map(&self) -> TempoMap {
        TempoMap::constant(self.tempo, self.meter)
    }

    /// Takes the next id from [`Project::next_id`]. Edits use it for every new object.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The track with `id`.
    pub fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Display index of the track with `id`.
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The clip with `id` and the track that holds it.
    pub fn find_clip(&self, id: ClipId) -> Option<(&Arc<Track>, &Arc<Clip>)> {
        self.tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t, c)))
    }

    /// The device with `id`, the channel that holds it and its index in that channel's chain.
    pub fn find_device(&self, id: DeviceId) -> Option<(ChannelRef, usize, &Arc<Device>)> {
        todo!("WP-12")
    }

    /// Whether any track is soloed. Audible = `!mute && (!any_solo || solo)`; "stumm durch Solo" is
    /// derived from this in the UI.
    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.channel.solo)
    }

    /// End of the last clip on any track, or tick 0 for a project without clips. The export range
    /// "Ganzer Song" ends here (S08).
    pub fn song_end(&self) -> Tick {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .max()
            .unwrap_or(Tick::ZERO)
    }
}

/// Addresses a channel strip: a track's channel or the master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelRef {
    /// The channel of the track with this id.
    Track(TrackId),
    /// The master strip.
    Master,
}

/// One track (row of the arrangement, strip of the mixer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Stable id.
    pub id: TrackId,
    /// Display name, unique within the project by convention (`edits::unique_track_name`), not
    /// enforced.
    pub name: String,
    /// Track colour, one of the 14 token hues ([`Hue::ALL`]).
    pub hue: Hue,
    /// What the track carries. Only MIDI in v0.1.
    pub kind: TrackKind,
    /// Mixer channel: volume, pan, mute, solo, devices. `devices[0]` is the instrument on MIDI
    /// tracks (role [`DeviceRole::Instrument`]); a track may have no instrument (silent).
    pub channel: Channel,
    /// Record arm ("R"). Arming also switches [`Track::monitor`] on (S02).
    pub arm: bool,
    /// Input monitoring ("I"): live MIDI input reaches the instrument.
    pub monitor: bool,
    /// Which MIDI input feeds the track ("Eingang").
    pub input: MidiInputSel,
    /// Which MIDI channel of that input feeds the track ("Kanal").
    pub midi_channel: MidiChannelSel,
    /// Header height in design px, [`TRACK_HEIGHT_MIN`] ..= [`TRACK_HEIGHT_MAX`]. Part of the project.
    pub height: u16,
    /// Clips sorted by `start`, then by id. Clips may overlap; overlapping clips all play.
    pub clips: Vec<Arc<Clip>>,
}

/// Kind of a track. An enum so audio tracks can come later (ARCHITECTURE §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    /// A MIDI track with an instrument.
    #[default]
    Midi,
}

/// A track colour: one of the 14 hues of the design tokens (`track.colors.*.hue`), in degrees of
/// the oklch hue circle. The UI turns it into colours with `komponist-tokens`; the model only stores
/// the hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hue(pub u16);

impl Hue {
    /// red, 25°.
    pub const RED: Hue = Hue(25);
    /// vermilion, 45°. Not in [`Hue::AUTO_ORDER`]: manual only (ARCHITECTURE §13.4).
    pub const VERMILION: Hue = Hue(45);
    /// orange, 70°.
    pub const ORANGE: Hue = Hue(70);
    /// yellow, 95°.
    pub const YELLOW: Hue = Hue(95);
    /// lime, 125°.
    pub const LIME: Hue = Hue(125);
    /// green, 150°.
    pub const GREEN: Hue = Hue(150);
    /// emerald, 175°.
    pub const EMERALD: Hue = Hue(175);
    /// teal, 200°.
    pub const TEAL: Hue = Hue(200);
    /// cyan, 225°.
    pub const CYAN: Hue = Hue(225);
    /// blue, 250°.
    pub const BLUE: Hue = Hue(250);
    /// indigo, 275°.
    pub const INDIGO: Hue = Hue(275);
    /// violet, 300°.
    pub const VIOLET: Hue = Hue(300);
    /// magenta, 325°.
    pub const MAGENTA: Hue = Hue(325);
    /// pink, 350°.
    pub const PINK: Hue = Hue(350);

    /// All 14 hues in token order (= the colour swatches of the inspector).
    pub const ALL: [Hue; 14] = [
        Hue::RED,
        Hue::VERMILION,
        Hue::ORANGE,
        Hue::YELLOW,
        Hue::LIME,
        Hue::GREEN,
        Hue::EMERALD,
        Hue::TEAL,
        Hue::CYAN,
        Hue::BLUE,
        Hue::INDIGO,
        Hue::VIOLET,
        Hue::MAGENTA,
        Hue::PINK,
    ];

    /// `track.auto-assign-hue-order` from the tokens: new tracks take the first hue not used by
    /// another track; when all are used, `ALL[track_count % 14]` (prototype `addTrack`).
    pub const AUTO_ORDER: [Hue; 13] = [
        Hue(150),
        Hue(250),
        Hue(300),
        Hue(95),
        Hue(200),
        Hue(25),
        Hue(325),
        Hue(175),
        Hue(70),
        Hue(225),
        Hue(125),
        Hue(275),
        Hue(350),
    ];
}

/// Which MIDI input feeds a track.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MidiInputSel {
    /// All enabled MIDI inputs ("Alle MIDI-Eingänge"), the default.
    #[default]
    All,
    /// The input device with this name (as listed by `komponist-audio`). Kept when the device is
    /// absent; the track then receives nothing from MIDI.
    Device(String),
}

/// Which MIDI channel of the input feeds a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MidiChannelSel {
    /// Every channel ("Omni"), the default.
    #[default]
    Omni,
    /// One channel, `0 ..= 15` (shown as 1–16).
    Channel(u8),
}

/// A mixer channel: the track or master strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    /// Fader level in dB, [`VOLUME_DB_MIN`] (= −∞) ..= [`VOLUME_DB_MAX`]. Default 0.0.
    pub volume_db: f32,
    /// Balance, `-1.0` (left) ..= `1.0` (right); 0.0 = centre ("C"). Applied with
    /// `klang_dsp::pan::balance`.
    pub pan: f32,
    /// Mute ("M").
    pub mute: bool,
    /// Solo ("S"). Ignored on the master.
    pub solo: bool,
    /// Device chain, processed in order. On MIDI tracks `devices[0]` is the instrument when its role
    /// is [`DeviceRole::Instrument`]; all others are effects.
    pub devices: Vec<Arc<Device>>,
}

impl Default for Channel {
    /// 0 dB, centred, not muted, not soloed, no devices.
    fn default() -> Channel {
        Channel {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            devices: Vec::new(),
        }
    }
}

/// An instrument or effect in a channel's chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// Stable id; one engine processor slot per device id.
    pub id: DeviceId,
    /// Instrument (first in a track chain) or effect.
    pub role: DeviceRole,
    /// Which plugin this is.
    pub plugin: PluginRef,
    /// Bypass: effects pass audio through, instruments are silent; the processor is reset when
    /// bypass turns on.
    pub bypass: bool,
    /// Opaque plugin state (pisynth: patch JSON; CLAP/VST3: the bridge's state blob). `None` = the
    /// plugin's defaults. Kept unchanged when the plugin is missing (S06). Stored as base64 in the
    /// project file.
    #[serde(default, with = "crate::persist::base64_state")]
    pub state: Option<Arc<[u8]>>,
}

/// Role of a device in its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    /// Makes sound from notes; only at index 0 of a track chain.
    Instrument,
    /// Processes audio.
    Effect,
}

/// Identifies a plugin independent of where it is installed.
///
/// Resolved against the catalog of `komponist-plugins` when a project opens. An unresolved
/// reference makes the device "fehlt" (S06), with its state kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    /// Plugin format.
    pub format: PluginFormat,
    /// Format-specific id: [`BUILTIN_PISYNTH`] / [`BUILTIN_KLOPFER`] for built-ins, the CLAP plugin
    /// id (`com.example.synth`), or the VST3 class id as 32 upper-case hex digits.
    pub id: String,
    /// Display name at the time the device was added ("Vital"); shown when the plugin is missing.
    pub name: String,
    /// Vendor at the time the device was added.
    pub vendor: String,
    /// Version string at the time the device was added (informational).
    pub version: String,
}

/// Plugin format of a [`PluginRef`]. Shown as the format tag "INTERN" / "CLAP" / "VST3".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    /// Shipped with komponist (pisynth in process, klopfer through the bridge).
    Builtin,
    /// A CLAP plugin.
    Clap,
    /// A VST3 plugin.
    Vst3,
}

/// A clip on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Stable id.
    pub id: ClipId,
    /// Display name ("Beat A", "Take 1").
    pub name: String,
    /// Timeline position of the clip start, ≥ 0.
    pub start: Tick,
    /// Length on the timeline, ≥ 1 tick. Content beyond it is cut.
    pub length: Tick,
    /// Clip loop ("loopen", corner drag): `Some(l)` repeats the content `0 .. l` every `l` ticks
    /// until [`Clip::length`]; notes reaching past `l` are cut at each repetition boundary. `None` =
    /// the content plays once. Step clips ignore it: their pattern always repeats, and edits keep it
    /// `None` for them.
    pub loop_length: Option<Tick>,
    /// Muted clips stay visible (dim colours) but do not play (key `0`).
    pub mute: bool,
    /// Non-destructive transposition in semitones, `-48 ..= 48` ("Transponieren" in the inspector).
    /// Notes shifted outside `0 ..= 127` are dropped at compile time.
    #[serde(default)]
    pub transpose: i8,
    /// Non-destructive velocity change in percent, `-100 ..= 100` ("Velocity +0 %"): every velocity
    /// is scaled by `1 + v / 100` and clamped to `1 ..= 127` at compile time.
    #[serde(default)]
    pub velocity_pct: i8,
    /// MIDI notes or a step pattern.
    pub content: ClipContent,
}

impl Clip {
    /// First tick after the clip on the timeline: `start + length`.
    pub fn end(&self) -> Tick {
        self.start + self.length
    }

    /// Whether this is a MIDI or a step clip.
    pub fn kind(&self) -> ClipKind {
        match self.content {
            ClipContent::Midi(_) => ClipKind::Midi,
            ClipContent::Step(_) => ClipKind::Step,
        }
    }
}

/// Content of a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ClipContent {
    /// Notes, edited in the piano roll.
    Midi(MidiClip),
    /// A step pattern, edited in the step sequencer.
    Step(StepClip),
}

/// Kind of a clip's content, without the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipKind {
    /// [`ClipContent::Midi`].
    Midi,
    /// [`ClipContent::Step`].
    Step,
}

/// MIDI content: one or more takes, one of them active.
///
/// Invariants: `takes` is never empty; `active_take < takes.len()`. A clip drawn by hand has one take
/// with an empty name. Loop recording adds a take per pass (S02), selectable in the inspector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiClip {
    /// All takes, oldest first.
    pub takes: Vec<Take>,
    /// Index of the take that plays and is edited.
    pub active_take: usize,
}

impl MidiClip {
    /// The active take.
    pub fn active(&self) -> &Take {
        &self.takes[self.active_take]
    }
}

/// One take of a MIDI clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Take {
    /// Take name ("Take 2"); empty for a hand-drawn clip.
    pub name: String,
    /// Notes sorted by `start`, then `pitch`, then id.
    pub notes: Arc<Vec<Note>>,
}

/// A MIDI note inside a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Note {
    /// Stable id.
    pub id: NoteId,
    /// Start relative to the clip start (clip-local), ≥ 0.
    pub start: Tick,
    /// Length, ≥ 1 tick.
    pub length: Tick,
    /// MIDI pitch `0 ..= 127`; 60 = C4.
    pub pitch: u8,
    /// MIDI velocity `1 ..= 127`; drives the note brightness.
    pub velocity: u8,
}

/// Step content: rows of steps, played as a repeating pattern.
///
/// The pattern is `length_steps × resolution` ticks long and repeats from the clip start until the
/// clip end. Compiled notes: one per `on` step at `row.note`, one step long; a ratchet of `n` splits
/// the step into `n` evenly spaced hits of `step / n` ticks, hits after the first at 75 % velocity
/// (prototype `schedule`). Swing delays every second step (odd index) by
/// `swing × step length / 2`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepClip {
    /// Rows top to bottom; default [`DEFAULT_STEP_ROWS`], or the instrument's note names.
    pub rows: Vec<StepRow>,
    /// Pattern length in steps, `1 ..= 32` ("Länge 16 Steps").
    pub length_steps: u8,
    /// Step length ("Auflösung").
    pub resolution: StepResolution,
    /// Swing amount `0.0 ..= 0.75` (shown as 0–75 %).
    pub swing: f32,
}

/// One row (pad) of a step clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRow {
    /// Row name ("Kick").
    pub name: String,
    /// MIDI note the row plays.
    pub note: u8,
    /// Row mute ("M" in the row header).
    pub mute: bool,
    /// Row solo ("S"): when any row is soloed only soloed rows play.
    pub solo: bool,
    /// All [`MAX_STEPS`] steps; only the first `length_steps` play.
    pub steps: [Step; MAX_STEPS],
}

/// One step of a step row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Step {
    /// Whether the step plays.
    pub on: bool,
    /// Velocity `1 ..= 127`; kept while the step is off.
    pub velocity: u8,
    /// Chance in percent `0 ..= 100` that the step plays ("Wahrsch."), rolled once per step and pass
    /// by the engine.
    pub probability: u8,
    /// Hits per step `1 ..= 4` ("Ratch.").
    pub ratchet: u8,
}

impl Default for Step {
    /// Off, velocity 100, probability 100 %, ratchet 1 (a clicked step turns on with these values).
    fn default() -> Step {
        Step {
            on: false,
            velocity: 100,
            probability: 100,
            ratchet: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model mirrors a few design tokens it cannot import (no studio dependencies): the hue
    /// list, the auto-assign order and the track height range.
    #[test]
    fn mirrored_tokens_match_tokens_json() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/tokens.json");
        let tokens: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("tokens.json")).unwrap();

        let colors = tokens["track"]["colors"].as_object().expect("track.colors");
        // serde_json maps are sorted by key, so compare the hues sorted (token order is ascending).
        let mut hues: Vec<u16> = colors
            .values()
            .map(|c| c["hue"].as_u64().expect("hue") as u16)
            .collect();
        hues.sort_unstable();
        assert_eq!(hues, Hue::ALL.map(|h| h.0).to_vec());

        let order: Vec<u16> = tokens["track"]["auto-assign-hue-order"]
            .as_array()
            .expect("auto-assign-hue-order")
            .iter()
            .map(|h| h.as_u64().unwrap() as u16)
            .collect();
        assert_eq!(order, Hue::AUTO_ORDER.map(|h| h.0).to_vec());

        let track = &tokens["size"]["track"];
        assert_eq!(track["min"].as_u64(), Some(TRACK_HEIGHT_MIN as u64));
        assert_eq!(track["max"].as_u64(), Some(TRACK_HEIGHT_MAX as u64));
        assert_eq!(track["default"].as_u64(), Some(TRACK_HEIGHT_DEFAULT as u64));
    }

    #[test]
    fn new_project_defaults() {
        let p = Project::new(DEFAULT_PROJECT_NAME);
        assert_eq!(p.tempo, Tempo::DEFAULT);
        assert_eq!(p.loop_range, TickRange::new(Tick(0), Tick(4 * 3840)));
        assert!(!p.loop_on && !p.metronome);
        assert_eq!(p.count_in_bars, 1);
        assert_eq!(p.song_end(), Tick::ZERO);
        assert!(p.tracks.is_empty());
    }

    #[test]
    fn step_defaults() {
        let s = Step::default();
        assert_eq!(
            (s.on, s.velocity, s.probability, s.ratchet),
            (false, 100, 100, 1)
        );
    }
}
