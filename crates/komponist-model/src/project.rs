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

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::audio::{AudioClip, AudioInput, AudioIo};
use crate::automation::{ClipAutomation, TrackAutomation};
use crate::ids::{ClipId, DeviceId, NoteId, SendId, TrackId};
use crate::macros::TrackMacros;
use crate::plugin_assets::AssetLink;
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

/// Catalog id of the built-in hexa instrument (in process, `komponist-builtin`). The value is what project
/// files store; it keeps the synth's former name "pisynth" so existing projects load.
pub const BUILTIN_HEXA: &str = "pisynth";
/// Display name of [`BUILTIN_HEXA`]. Project files written before the rename store "pisynth" as the plugin
/// name; loading replaces it (`persist::from_json`).
pub const HEXA_NAME: &str = "Hexa";
/// Catalog id of the built-in volt instrument. The value is what project files store; it keeps the synth's
/// former name "knurr" so existing projects load.
pub const BUILTIN_VOLT: &str = "knurr";
/// Display name of [`BUILTIN_VOLT`]. Project files written before the rename store "knurr" as the plugin
/// name; loading replaces it (`persist::from_json`).
pub const VOLT_NAME: &str = "Volt";
/// Display name of the built-in compressor (id `"presse"`, `komponist_builtin::Internal::Presse`); files before
/// 2026-11-16 store it in lowercase and are renamed on load (`persist::from_json`).
pub const PRESSE_NAME: &str = "Presse";
/// Catalog id of the built-in drum sampler (`komponist_sampler`, in process, WP-87).
pub const BUILTIN_SAMPLER: &str = "sampler";
/// Display name of the built-in drum sampler, whose catalog ID stays `sampler`.
pub const DRUM_SAMPLER_NAME: &str = "Klopfer";
/// Catalog id klopfer had as a built-in ("klopfer · INTERN", the bundled `Klopfer.clap`) before
/// the sampler replaced it. Only old projects hold it; the catalog resolves it to the scanned
/// klopfer CLAP plugin.
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
    /// Automation lanes of the master channel.
    #[serde(default, skip_serializing_if = "TrackAutomation::is_empty")]
    pub master_automation: Arc<TrackAutomation>,
    /// Next free id for [`TrackId`], [`ClipId`], [`NoteId`] and [`DeviceId`] (one counter for all
    /// kinds). Only grows.
    pub next_id: u64,
}

/// `a == b` for shared values, skipping the deep compare when both are the same allocation.
/// `Arc`'s own `PartialEq` does that only for `T: Eq`; these types hold `f32`s.
fn arc_same<T: PartialEq>(a: &Arc<T>, b: &Arc<T>) -> bool {
    Arc::ptr_eq(a, b) || **a == **b
}

fn arcs_same<T: PartialEq>(a: &[Arc<T>], b: &[Arc<T>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| arc_same(x, y))
}

impl Project {
    /// Whether `other` holds the same content, like `==`, but without a deep compare of what both
    /// share: tracks, the master channel, automation lanes, clips and devices are compared by
    /// `Arc::ptr_eq` first. An edit clones the project and replaces only what it changes, so this
    /// costs O(tracks) plus the changed tracks, not the whole project (the document compares on
    /// every edit and every drag frame, #220).
    ///
    /// Every struct is destructured without `..`, so a new field is a compile error here until it
    /// is compared.
    pub fn same_content(&self, other: &Project) -> bool {
        let Project {
            name,
            tempo,
            meter,
            loop_range,
            loop_on,
            metronome,
            count_in_bars,
            tracks,
            master,
            master_automation,
            next_id,
        } = self;
        *name == other.name
            && *tempo == other.tempo
            && *meter == other.meter
            && *loop_range == other.loop_range
            && *loop_on == other.loop_on
            && *metronome == other.metronome
            && *count_in_bars == other.count_in_bars
            && *next_id == other.next_id
            && tracks.len() == other.tracks.len()
            && tracks
                .iter()
                .zip(&other.tracks)
                .all(|(a, b)| Arc::ptr_eq(a, b) || a.same_content(b))
            && (Arc::ptr_eq(master, &other.master) || master.same_content(&other.master))
            && (Arc::ptr_eq(master_automation, &other.master_automation)
                || master_automation.same_content(&other.master_automation))
    }
}

impl Track {
    /// `==` that skips shared clips, devices and automation lanes ([`Project::same_content`]).
    pub fn same_content(&self, other: &Track) -> bool {
        let Track {
            id,
            name,
            hue,
            kind,
            channel,
            arm,
            monitor,
            input,
            midi_channel,
            audio,
            height,
            clips,
            output,
            sends,
            folder,
            folded,
            automation,
            macros,
        } = self;
        *id == other.id
            && *name == other.name
            && *hue == other.hue
            && *kind == other.kind
            && *arm == other.arm
            && *monitor == other.monitor
            && *input == other.input
            && *midi_channel == other.midi_channel
            && *audio == other.audio
            && *height == other.height
            && *output == other.output
            && *sends == other.sends
            && *folder == other.folder
            && *folded == other.folded
            && *macros == other.macros
            && channel.same_content(&other.channel)
            && arcs_same(clips, &other.clips)
            && automation.same_content(&other.automation)
    }
}

impl Channel {
    /// `==` that skips shared devices ([`Project::same_content`]).
    pub fn same_content(&self, other: &Channel) -> bool {
        let Channel {
            volume_db,
            pan,
            mute,
            solo,
            devices,
        } = self;
        *volume_db == other.volume_db
            && *pan == other.pan
            && *mute == other.mute
            && *solo == other.solo
            && arcs_same(devices, &other.devices)
    }
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
            master_automation: Arc::default(),
            next_id: 1,
        }
    }

    /// The tempo map for tick ↔ time conversions.
    pub fn tempo_map(&self) -> TempoMap {
        TempoMap::constant(self.tempo, self.meter)
    }

    /// Takes the next id from [`Project::next_id`]. Edits use it for every new object.
    /// Exhaustion leaves the counter unchanged; `u64::MAX` is reserved as an invalid next id.
    pub fn alloc_id(&mut self) -> Result<u64, crate::edits::EditError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .filter(|&next| next < u64::MAX)
            .ok_or(crate::edits::EditError::IdsExhausted)?;
        Ok(id)
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

    /// The send with `id`: the track that holds it and its index in [`Track::sends`].
    pub fn find_send(&self, id: SendId) -> Option<(&Arc<Track>, usize)> {
        self.tracks
            .iter()
            .find_map(|t| t.sends.iter().position(|s| s.id == id).map(|i| (t, i)))
    }

    /// The device with `id`, the channel that holds it and its index in that channel's chain.
    pub fn find_device(&self, id: DeviceId) -> Option<(ChannelRef, usize, &Arc<Device>)> {
        let chains = self
            .tracks
            .iter()
            .map(|t| (ChannelRef::Track(t.id), &t.channel.devices))
            .chain(std::iter::once((ChannelRef::Master, &self.master.devices)));
        for (channel, chain) in chains {
            if let Some(index) = chain.iter().position(|d| d.id == id) {
                return Some((channel, index, &chain[index]));
            }
        }
        None
    }

    /// Whether any track or bus is soloed. With buses, what solo silences follows the routing graph
    /// ([`crate::routing::Gates`], ARCHITECTURE §6.8.6); without buses it is
    /// `!mute && (!any_solo || solo)`.
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
    /// What the track carries: MIDI (clips, instrument), audio (audio clips, effects only) or a bus
    /// (sums its inputs, effects only).
    pub kind: TrackKind,
    /// Mixer channel: volume, pan, mute, solo, devices. A MIDI track's chain is
    /// `[MIDI effects…] → instrument → [audio effects…]` ([`Channel::midi_effect_count`],
    /// [`Channel::instrument`]); a track may have no instrument (silent). An audio track's chain
    /// holds audio effects only.
    pub channel: Channel,
    /// Record arm ("R"). Arming also switches [`Track::monitor`] on (S02), and an armed track
    /// monitors live input even with I off (auto-monitoring, owner decision 2026-11-15). On an
    /// audio track arming does not touch [`Track::monitor`]: [`AudioIo::monitor`] decides.
    pub arm: bool,
    /// Input monitoring ("I"): live MIDI input reaches the instrument. Always `false` on audio
    /// tracks ([`AudioIo::monitor`] replaces it).
    pub monitor: bool,
    /// Which MIDI input feeds the track ("Eingang").
    pub input: MidiInputSel,
    /// Which MIDI channel of that input feeds the track ("Kanal").
    pub midi_channel: MidiChannelSel,
    /// Audio input and monitoring of an audio track (epic #8); the default on every other kind,
    /// and then not written to the file.
    #[serde(default, skip_serializing_if = "AudioIo::is_default")]
    pub audio: AudioIo,
    /// Header height in design px, [`TRACK_HEIGHT_MIN`] ..= [`TRACK_HEIGHT_MAX`]. Part of the project.
    pub height: u16,
    /// Clips sorted by `start`, then by id. MIDI clips may overlap; overlapping clips all play.
    /// Audio clips never overlap (edits cut what lies below, [`AudioClip`]). Always empty on a bus;
    /// MIDI and step clips only on a MIDI track, audio clips only on an audio track.
    pub clips: Vec<Arc<Clip>>,
    /// Where the main output goes (after fader and pan): the master or a bus (wave 9, ARCHITECTURE
    /// §6.8). Never the track itself; the routing graph stays acyclic ([`crate::routing`]). A track
    /// whose output is a group ([`Track::folder`]) is that group's member in the arrangement.
    pub output: TrackOutput,
    /// Sends to buses, in display order (mixer send list). At most one send per target bus. Empty
    /// on buses in v1 (owner decision 11 A: a bus has only its output).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<TrackSend>,
    /// A bus shown as a group: a foldable folder of the tracks that output to it, which follow it
    /// directly in display order as one block ([`crate::routing::normalized_order`]). Set for a
    /// bus made with Strg G („In Gruppe zusammenfassen“); `false` for an effect bus („Send zu neuem
    /// Bus“, listed in the arrangement's „Busse“ section) and on every other kind. View only: the
    /// routing is the same for both.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folder: bool,
    /// A group whose members are hidden in the arrangement. Only on a [`Track::folder`] bus.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
    /// Automation lanes in display order.
    #[serde(default, skip_serializing_if = "TrackAutomation::is_empty")]
    pub automation: TrackAutomation,
    /// The track's fixed slots of assignable macros.
    #[serde(default, skip_serializing_if = "TrackMacros::is_default")]
    pub macros: TrackMacros,
}

impl Track {
    /// Whether this track is a bus ([`TrackKind::Bus`]).
    pub fn is_bus(&self) -> bool {
        self.kind == TrackKind::Bus
    }

    /// Whether this track is a group: a bus shown as a folder of its members.
    pub fn is_group(&self) -> bool {
        self.is_bus() && self.folder
    }

    /// Whether this track is an audio track ([`TrackKind::Audio`]).
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
    }

    /// The bus an audio track records ([`AudioInput::Bus`]), if it does.
    pub fn input_bus(&self) -> Option<TrackId> {
        match self.audio.input {
            AudioInput::Bus(bus) if self.is_audio() => Some(bus),
            _ => None,
        }
    }

    /// The send to `target`, if there is one.
    pub fn send_to(&self, target: TrackId) -> Option<&TrackSend> {
        self.sends.iter().find(|s| s.target == target)
    }
}

/// Where a track's main output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackOutput {
    /// The master strip (default; every track of a project from before wave 9).
    #[default]
    Master,
    /// The bus with this id ([`TrackKind::Bus`]).
    Bus(TrackId),
}

/// Where a send or a sidechain taps its source's signal (ARCHITECTURE §6.8.2). New sends tap
/// [`SEND_TAP_DEFAULT`], new sidechains [`SIDECHAIN_TAP_DEFAULT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tap {
    /// Behind the device chain, before fader, pan and mute („Vor Fader“).
    Pre,
    /// Behind fader, pan and mute, where the main output leaves („Nach Fader“).
    Post,
}

/// Level of a new send (owner decision 8 B: audible but safe).
pub const SEND_LEVEL_DEFAULT_DB: f32 = -6.0;
/// Tap of a new send (owner decision 9 A: post-fader, as in Ableton and Logic).
pub const SEND_TAP_DEFAULT: Tap = Tap::Post;
/// Tap of a new sidechain (owner decision 10 A: pre-fader, so ducking keeps working when the
/// source is pulled down or muted).
pub const SIDECHAIN_TAP_DEFAULT: Tap = Tap::Pre;

/// A send from a track (or bus) to a bus: a copy of the source's signal at `tap`, scaled by
/// `level_db`, summed into the target bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackSend {
    /// Stable id.
    pub id: SendId,
    /// The bus this send feeds ([`TrackKind::Bus`], never the sending track).
    pub target: TrackId,
    /// Send level in dB, [`VOLUME_DB_MIN`] (= −∞) ..= [`VOLUME_DB_MAX`].
    pub level_db: f32,
    /// Pre or post fader.
    pub tap: Tap,
    /// Off: the send is kept with its level but feeds nothing (the engine skips it).
    pub enabled: bool,
}

/// The sidechain input of a device: which track's (or bus's) signal feeds it, and where it is
/// tapped. Only devices with a sidechain input use it (the model does not know which do).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sidechain {
    /// The track or bus whose signal is tapped (never the track that holds the device).
    pub source: TrackId,
    /// Pre or post fader of the source.
    pub tap: Tap,
}

impl Sidechain {
    /// A sidechain from `source` with the default tap ([`SIDECHAIN_TAP_DEFAULT`]).
    pub fn new(source: TrackId) -> Sidechain {
        Sidechain {
            source,
            tap: SIDECHAIN_TAP_DEFAULT,
        }
    }
}

/// Kind of a track. Kinds are separate, there are no hybrid tracks (owner, 2026-11-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    /// A MIDI track with an instrument.
    #[default]
    Midi,
    /// A bus (wave 9): sums the tracks that output or send to it, runs audio effects, has its own
    /// fader, pan, mute, solo and output. Holds no clips, no instrument and no MIDI effects and is
    /// never armed. Groups and effect buses are both buses ([`Track::folder`], ARCHITECTURE
    /// §6.8.1).
    Bus,
    /// An audio track (wave 10, epic #8): audio clips instead of MIDI clips, audio effects only, one
    /// more source node in the same routing graph (ARCHITECTURE §6.8.9, §6.9). Holds no MIDI or
    /// step clips, no instrument and no MIDI effects; records from [`Track::audio`]'s input.
    Audio,
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
    /// Device chain, processed in order. On MIDI tracks: first the MIDI effects
    /// ([`DeviceRole::MidiEffect`], they rewrite the note events), then the instrument when there
    /// is one ([`DeviceRole::Instrument`]), then the audio effects. Master chains hold audio
    /// effects only.
    pub devices: Vec<Arc<Device>>,
}

impl Channel {
    /// Number of leading MIDI effects (0 for a project saved before WP-88).
    pub fn midi_effect_count(&self) -> usize {
        self.devices
            .iter()
            .take_while(|d| d.role == DeviceRole::MidiEffect)
            .count()
    }

    /// Index and device of the instrument, right behind the MIDI effects.
    pub fn instrument(&self) -> Option<(usize, &Arc<Device>)> {
        let index = self.midi_effect_count();
        self.devices
            .get(index)
            .filter(|d| d.role == DeviceRole::Instrument)
            .map(|d| (index, d))
    }

    /// Index of the first audio effect: behind the MIDI effects and the instrument.
    pub fn first_effect_index(&self) -> usize {
        let index = self.midi_effect_count();
        index
            + usize::from(
                self.devices
                    .get(index)
                    .is_some_and(|d| d.role == DeviceRole::Instrument),
            )
    }
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
    /// Opaque plugin state (hexa: patch JSON; CLAP/VST3: the bridge's state blob). `None` = the
    /// plugin's defaults. Kept unchanged when the plugin is missing (S06). Stored as base64 in the
    /// project file.
    #[serde(default, with = "crate::persist::base64_state")]
    pub state: Option<Arc<[u8]>>,
    /// Project-owned links for CLAP file slots. Older devices and projects omit this.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub asset_links: BTreeMap<u32, AssetLink>,
    /// Where the device's sidechain input comes from (wave 9); `None` = no sidechain (silence).
    /// Not written when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<Sidechain>,
}

/// Role of a device in its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    /// Makes sound from notes; on a track chain right behind the MIDI effects.
    Instrument,
    /// Processes audio.
    Effect,
    /// Rewrites note events before the instrument (WP-88: arpeggiator, scale). Only at the head of a
    /// track chain; never on the master. Bypassed: the events pass through unchanged.
    #[serde(rename = "midieffect")]
    MidiEffect,
}

/// Identifies a plugin independent of where it is installed.
///
/// Resolved against the catalog of `komponist-plugins` when a project opens. An unresolved
/// reference makes the device "fehlt" (S06), with its state kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    /// Plugin format.
    pub format: PluginFormat,
    /// Format-specific id: [`BUILTIN_HEXA`] / [`BUILTIN_SAMPLER`] for built-ins, the CLAP plugin
    /// id (`com.example.synth`), or the VST3 class id as 32 upper-case hex digits.
    pub id: String,
    /// Display name at the time the device was added ("Vital"); shown when the plugin is missing.
    pub name: String,
    /// Vendor at the time the device was added.
    pub vendor: String,
    /// Version string at the time the device was added (informational).
    pub version: String,
    /// File name of the plugin module at the time the device was added ("Vital.vst3",
    /// "Surge XT.clap"); shown in the missing-plugins list (S06). Empty for built-ins and for
    /// projects saved before this field existed; not written then, so such files stay unchanged.
    /// Bundled CLAP references use the stable `bundled-clap/` prefix and require the packaged
    /// catalog. A project cannot use this prefix to establish trust in a library.
    ///
    /// A `Box<str>`, not a `String`: it keeps `PluginRef` (and `PluginError`, which holds one)
    /// under clippy's `result_large_err` limit.
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub module: Box<str>,
}

/// Plugin format of a [`PluginRef`]. Shown as the format tag "INTERN" / "CLAP" / "VST3".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    /// Shipped with komponist (hexa, the sampler and the internal effects, all in process).
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
    /// Notes shifted outside `0 ..= 127` are dropped at compile time. MIDI and step clips only; 0 on
    /// audio clips.
    #[serde(default)]
    pub transpose: i8,
    /// Non-destructive velocity change in percent, `-100 ..= 100` ("Velocity +0 %"): every velocity
    /// is scaled by `1 + v / 100` and clamped to `1 ..= 127` at compile time. MIDI and step clips
    /// only; 0 on audio clips.
    #[serde(default)]
    pub velocity_pct: i8,
    /// MIDI notes, a step pattern or an audio window.
    pub content: ClipContent,
    /// Parameter envelopes in content-relative ticks, shared by all takes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<ClipAutomation>,
}

impl Clip {
    /// First tick after the clip on the timeline: `start + length`.
    pub fn end(&self) -> Tick {
        self.start + self.length
    }

    /// Whether this is a MIDI, a step or an audio clip.
    pub fn kind(&self) -> ClipKind {
        match self.content {
            ClipContent::Midi(_) => ClipKind::Midi,
            ClipContent::Step(_) => ClipKind::Step,
            ClipContent::Audio(_) => ClipKind::Audio,
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
    /// A window into a linked audio file (audio tracks only).
    Audio(AudioClip),
}

/// Kind of a clip's content, without the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipKind {
    /// [`ClipContent::Midi`].
    Midi,
    /// [`ClipContent::Step`].
    Step,
    /// [`ClipContent::Audio`].
    Audio,
}

impl ClipKind {
    /// Whether a track of `kind` can hold a clip of this kind: MIDI and step clips on MIDI tracks,
    /// audio clips on audio tracks, nothing on a bus.
    pub fn fits(self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Midi => matches!(self, ClipKind::Midi | ClipKind::Step),
            TrackKind::Audio => self == ClipKind::Audio,
            TrackKind::Bus => false,
        }
    }
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
/// `swing × step length / 2`. [`StepClip::hits`] implements these rules for the engine compiler and
/// `edits::step_clip_to_midi`.
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

/// Highest swing amount (75 %).
pub(crate) const MAX_SWING: f32 = 0.75;

/// Velocity of ratchet hits after the first, relative to the step velocity (prototype `schedule`).
const RATCHET_FOLLOW_VELOCITY: f32 = 0.75;

impl StepClip {
    /// Everything the pattern plays in a clip of `length` ticks, sorted by `start`, then `note`,
    /// then `row`, then `hit`.
    ///
    /// Rules:
    /// - The pattern (`length_steps` steps of `resolution` ticks) repeats from clip-local tick 0.
    ///   Step `k` counted from the clip start plays pattern index `i = k % length_steps`; steps at
    ///   or beyond `length_steps` (hidden) never play.
    /// - Swing: steps with an **odd pattern index** `i` start `round(swing × step / 2)` ticks late.
    ///   With an odd `length_steps` the parity follows the pattern index, not `k`.
    /// - A step plays when it is `on` and its row is audible: not muted and, if any row is soloed,
    ///   soloed itself.
    /// - Ratchet `n` (clamped to 1 ..= 4): `n` hits; hit `q` spans `start + q × step / n` to
    ///   `start + (q + 1) × step / n` (integer division, so the hits tile the step exactly). Hit 0
    ///   has the step velocity, later hits `round(velocity × 0.75)`, at least 1.
    /// - Every hit carries the step's `probability` unchanged, including 0. All hits of one step
    ///   share one roll: the engine rolls for `hit == 0` and lets later hits follow (`hit > 0`);
    ///   `edits::step_clip_to_midi` drops probability-0 steps and ignores the rest.
    /// - Hits are cut at `length`; hits starting at or after it are left out (also when swing
    ///   pushes them there).
    ///
    /// Clip-level transposition, velocity change and mute are not applied here.
    pub fn hits(&self, length: Tick) -> Vec<StepHit> {
        let step = self.resolution.ticks().0;
        let steps = usize::from(self.length_steps).clamp(1, MAX_STEPS);
        let swing =
            (f64::from(self.swing.clamp(0.0, MAX_SWING)) * step as f64 / 2.0).round() as i64;
        let any_solo = self.rows.iter().any(|r| r.solo);
        let audible = |r: &StepRow| !r.mute && (!any_solo || r.solo);
        let mut hits = Vec::new();
        for k in 0.. {
            let step_start = k * step;
            if step_start >= length.0 {
                break;
            }
            let index = k as usize % steps;
            let start = step_start + if index % 2 == 1 { swing } else { 0 };
            for (row_index, row) in self.rows.iter().enumerate().filter(|(_, r)| audible(r)) {
                let s = row.steps[index];
                if !s.on {
                    continue;
                }
                let count = i64::from(s.ratchet.clamp(1, 4));
                for q in 0..count {
                    let hit_start = start + q * step / count;
                    if hit_start >= length.0 {
                        break;
                    }
                    let hit_end = (start + (q + 1) * step / count).min(length.0);
                    let velocity = if q == 0 {
                        s.velocity
                    } else {
                        (f32::from(s.velocity) * RATCHET_FOLLOW_VELOCITY).round() as u8
                    };
                    hits.push(StepHit {
                        row: row_index,
                        step: index,
                        hit: q as u8,
                        note: row.note.min(127),
                        start: Tick(hit_start),
                        length: Tick(hit_end - hit_start),
                        velocity: velocity.clamp(1, 127),
                        probability: s.probability.min(100),
                    });
                }
            }
        }
        hits.sort_by_key(|h| (h.start, h.note, h.row, h.hit));
        hits
    }
}

/// One note a step pattern plays ([`StepClip::hits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepHit {
    /// Index of the row in [`StepClip::rows`].
    pub row: usize,
    /// Pattern index of the step, `0 .. length_steps`.
    pub step: usize,
    /// Index of the hit within its step, `0 .. ratchet`; 0 is the hit that rolls the probability.
    pub hit: u8,
    /// MIDI note (the row's note).
    pub note: u8,
    /// Start relative to the clip start, ≥ 0.
    pub start: Tick,
    /// Length, ≥ 1 tick.
    pub length: Tick,
    /// MIDI velocity `1 ..= 127`.
    pub velocity: u8,
    /// The step's probability in percent `0 ..= 100`, the same on every hit of the step.
    pub probability: u8,
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

    /// 200 tracks sharing the demo's clips, so unchanged tracks stay shared between clones.
    fn big_project() -> Project {
        let mut p = crate::fixtures::demo_project();
        let template = Arc::clone(&p.tracks[2]);
        for n in 0..200 {
            let mut t = (*template).clone();
            t.id = TrackId(10_000 + n);
            p.tracks.push(Arc::new(t));
        }
        p
    }

    #[test]
    fn same_content_agrees_with_eq_and_sees_a_change_anywhere() {
        let p = big_project();
        assert!(p.same_content(&p.clone()));
        type Change = Box<dyn Fn(&mut Project)>;
        let changes: Vec<(&str, Change)> = vec![
            ("name", Box::new(|p| p.name.push('x'))),
            ("loop_on", Box::new(|p| p.loop_on = !p.loop_on)),
            ("metronome", Box::new(|p| p.metronome = !p.metronome)),
            ("count_in", Box::new(|p| p.count_in_bars += 1)),
            ("next_id", Box::new(|p| p.next_id += 1)),
            (
                "master volume",
                Box::new(|p| Arc::make_mut(&mut p.master).volume_db -= 1.0),
            ),
            (
                "master automation",
                Box::new(|p| Arc::make_mut(&mut p.master_automation).shown ^= true),
            ),
            (
                "track name",
                Box::new(|p| Arc::make_mut(p.tracks.last_mut().unwrap()).name.push('x')),
            ),
            (
                "track channel",
                Box::new(|p| Arc::make_mut(&mut p.tracks[0]).channel.pan += 0.5),
            ),
            (
                "track height",
                Box::new(|p| Arc::make_mut(&mut p.tracks[1]).height += 1),
            ),
            (
                "clip",
                Box::new(|p| {
                    let t = Arc::make_mut(&mut p.tracks[2]);
                    Arc::make_mut(&mut t.clips[0]).start = Tick(1);
                }),
            ),
            ("track order", Box::new(|p| p.tracks.swap(0, 1))),
            ("track removed", Box::new(|p| drop(p.tracks.pop()))),
        ];
        for (what, change) in changes {
            let mut q = p.clone();
            change(&mut q);
            assert_ne!(p, q, "{what}: the change must be one");
            assert!(!p.same_content(&q), "{what} not detected");
            assert!(!q.same_content(&p), "{what} not detected (reversed)");
        }
    }

    /// An unchanged track is not compared deeply: a track that is unequal to itself (a NaN level,
    /// which a deep `==` never matches) passes while it is shared, and an equal copy of another
    /// track costs one compare of that track only.
    #[test]
    fn same_content_skips_shared_tracks() {
        let mut p = big_project();
        Arc::make_mut(&mut p.tracks[5]).channel.volume_db = f32::NAN;
        let q = p.clone();
        assert_ne!(p, q, "a deep compare trips over the NaN track");
        assert!(p.same_content(&q), "shared tracks are skipped");
        // An edit replaces one track with an equal copy of itself: only that one is compared.
        let mut r = p.clone();
        let copy = (*r.tracks[100]).clone();
        r.tracks[100] = Arc::new(copy);
        assert!(p.same_content(&r));
        // … and one that really differs is found.
        Arc::make_mut(&mut r.tracks[100]).name.push('x');
        assert!(!p.same_content(&r));
    }

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

    /// A pattern of `rows` rows (notes 36, 37, …), `steps` long at 1/16, all steps off.
    fn pattern(rows: u8, steps: u8) -> StepClip {
        StepClip {
            rows: (0..rows)
                .map(|i| StepRow {
                    name: format!("Pad {}", i + 1),
                    note: 36 + i,
                    mute: false,
                    solo: false,
                    steps: [Step::default(); MAX_STEPS],
                })
                .collect(),
            length_steps: steps,
            resolution: StepResolution::Sixteenth,
            swing: 0.0,
        }
    }

    fn on(p: &mut StepClip, row: usize, step: usize) -> &mut Step {
        let s = &mut p.rows[row].steps[step];
        s.on = true;
        s
    }

    /// `(start, length, note, velocity)` of each hit.
    fn played(hits: &[StepHit]) -> Vec<(i64, i64, u8, u8)> {
        hits.iter()
            .map(|h| (h.start.0, h.length.0, h.note, h.velocity))
            .collect()
    }

    #[test]
    fn hits_repeat_the_pattern() {
        let mut p = pattern(2, 4);
        on(&mut p, 0, 0).velocity = 90;
        on(&mut p, 1, 2);
        // Hidden steps never play.
        on(&mut p, 1, 5);
        let hits = p.hits(Tick(8 * 240));
        assert_eq!(
            played(&hits),
            [
                (0, 240, 36, 90),
                (480, 240, 37, 100),
                (960, 240, 36, 90),
                (1440, 240, 37, 100),
            ]
        );
        let h = hits[3];
        assert_eq!((h.row, h.step, h.hit, h.probability), (1, 2, 0, 100));
        assert!(p.hits(Tick::ZERO).is_empty());
        assert!(pattern(8, 16).hits(Tick(3840)).is_empty());
        // Eighths: 480 ticks per step.
        p.resolution = StepResolution::Eighth;
        assert_eq!(played(&p.hits(Tick(1920)))[1], (960, 480, 37, 100));
    }

    #[test]
    fn hits_swing_on_odd_pattern_indices() {
        let mut p = pattern(1, 3);
        for i in 0..3 {
            on(&mut p, 0, i);
        }
        p.swing = 0.5;
        // Index 1 starts 60 ticks late; the pattern has 3 steps, so step k = 4 is index 1 again.
        let starts: Vec<(i64, usize)> = p
            .hits(Tick(6 * 240))
            .iter()
            .map(|h| (h.start.0, h.step))
            .collect();
        assert_eq!(
            starts,
            [(0, 0), (300, 1), (480, 2), (720, 0), (1020, 1), (1200, 2)]
        );
        // Swing is clamped to 75 %: 90 ticks.
        p.swing = 5.0;
        assert_eq!(p.hits(Tick(480))[1].start, Tick(330));
    }

    #[test]
    fn hits_split_ratchets() {
        let mut p = pattern(1, 16);
        let s = on(&mut p, 0, 0);
        s.ratchet = 3;
        s.velocity = 90;
        s.probability = 40;
        let hits = p.hits(Tick(3840));
        assert_eq!(
            played(&hits),
            [(0, 80, 36, 90), (80, 80, 36, 68), (160, 80, 36, 68)]
        );
        let ids: Vec<(u8, u8)> = hits.iter().map(|h| (h.hit, h.probability)).collect();
        assert_eq!(ids, [(0, 40), (1, 40), (2, 40)]);
        // 1/16 triplets: 160 ticks in three hits that tile the step.
        p.resolution = StepResolution::SixteenthTriplet;
        let tiles: Vec<(i64, i64)> = p
            .hits(Tick(160))
            .iter()
            .map(|h| (h.start.0, h.length.0))
            .collect();
        assert_eq!(tiles, [(0, 53), (53, 53), (106, 54)]);
        // Velocity 1 stays 1 on later hits.
        p.rows[0].steps[0].velocity = 1;
        assert!(p.hits(Tick(160)).iter().all(|h| h.velocity == 1));
    }

    #[test]
    fn hits_keep_probability_and_follow_mute_and_solo() {
        let mut p = pattern(3, 4);
        on(&mut p, 0, 0).probability = 0;
        on(&mut p, 1, 0);
        on(&mut p, 2, 0);
        let rows = |p: &StepClip| -> Vec<(usize, u8)> {
            p.hits(Tick(240))
                .iter()
                .map(|h| (h.row, h.probability))
                .collect()
        };
        assert_eq!(rows(&p), [(0, 0), (1, 100), (2, 100)]);
        p.rows[1].mute = true;
        assert_eq!(rows(&p), [(0, 0), (2, 100)]);
        p.rows[0].solo = true;
        p.rows[1].solo = true;
        assert_eq!(rows(&p), [(0, 0)], "muted wins over solo");
    }

    #[test]
    fn hits_are_cut_at_the_clip_end() {
        let mut p = pattern(1, 4);
        on(&mut p, 0, 0);
        on(&mut p, 0, 1).ratchet = 2;
        p.swing = 0.5;
        // Step 1 starts at 300 (swing); its hits at 300 and 420, the second cut out at 400.
        assert_eq!(
            played(&p.hits(Tick(400))),
            [(0, 240, 36, 100), (300, 100, 36, 100)]
        );
        // A swung step pushed to or past the end is left out.
        assert_eq!(played(&p.hits(Tick(300))), [(0, 240, 36, 100)]);
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
