//! The program: an immutable, RT-ready snapshot of everything the sequencer and mixer need
//! (ARCHITECTURE §6.3).
//!
//! Built on the UI thread by [`crate::compile::build`], sent to the engine as `Box<Program>` with
//! `Command::SetProgram`, read (never written) on the audio thread, and returned through the garbage
//! queue to be dropped on the UI thread. The app keeps a clone as `previous` for the next build;
//! cloning is cheap because the per-track parts are `Arc`s.
//!
//! The audio thread reads every field except [`TrackProgram::source`] and [`Program::slots`], which
//! exist for the compiler on the UI thread.

use std::sync::Arc;

use komponist_model::automation::{
    AutomationMode, AutomationOwner, AutomationTarget, TrackAutomation,
};
use komponist_model::{DeviceId, DeviceRole, Tick, TickRange, TrackId};
use komponist_model::{TempoMap, Track};

use crate::api::TrackMeter;
use crate::audio::{AudioPlan, ClipSource};
use crate::automation::{CompiledTarget, EngineTarget};
pub use crate::macros::MacroProgram;
pub use crate::note_schedule::NoteSchedule;
use crate::route::Routing;

/// [`SeqNote::probability`] value meaning "plays if and only if the last rolled note with the same
/// key on this track played". Ratchet hits after the first carry it, so a step with ratchets rolls
/// its probability once (prototype `schedule`).
pub const PROBABILITY_FOLLOW: u8 = u8::MAX;

/// Index of a processor slot in the engine's preallocated slot table, `0 .. MAX_SLOTS`.
///
/// One slot per device ([`SlotMap`]); the app fills slots with `Command::InsertProcessor`. A slot
/// without a processor behaves like a bypassed device (effects pass through, instruments are
/// silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(pub u32);

/// Engine-side id of a MIDI input port.
///
/// Assigned by the app (from `komponist-audio`'s port ids, which are stable per device name for the
/// session) and resolved from track input names by [`crate::compile::CompileEnv::midi_port`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MidiPort(pub u16);

/// The compiled song for the engine.
#[derive(Debug, Clone)]
pub struct Program {
    /// Tempo and meter.
    pub tempo_map: TempoMap,
    /// Loop range in ticks.
    pub loop_range: TickRange,
    /// Whether playback loops.
    pub loop_on: bool,
    /// Metronome during playback (the click always sounds during the count-in).
    pub metronome: bool,
    /// Count-in length in bars for `Command::Record`.
    pub count_in_bars: u8,
    /// End of the last clip ("Ganzer Song" export range).
    pub song_end: Tick,
    /// Tracks in project order, at most [`crate::MAX_TRACKS`].
    pub tracks: Vec<Arc<TrackProgram>>,
    /// The master strip.
    pub master: MasterProgram,
    /// Device → slot assignment of this program. UI thread only.
    pub slots: Arc<SlotMap>,
    /// Buses, sends, sidechains, job order, gates and the delays between nodes (ARCHITECTURE
    /// §6.8.4). `Routing::default()` is the flat graph: every track straight into the master.
    pub routing: Arc<Routing>,
}

impl Program {
    /// The program of track `id`.
    pub fn track(&self, id: TrackId) -> Option<&Arc<TrackProgram>> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Visit the current plain value of every parameter driven by a track macro.
    /// Missing telemetry falls back to the macros' stored values.
    pub fn for_each_macro_parameter(
        &self,
        values: impl Fn(TrackId) -> Option<[f32; komponist_model::macros::MACRO_COUNT]>,
        mut emit: impl FnMut(SlotId, u32, f64),
    ) {
        for track in &self.tracks {
            if track.macros.targets.is_empty() {
                continue;
            }
            let values = values(track.id).unwrap_or(track.macros.base);
            for mapping in track.macros.targets.iter() {
                if let EngineTarget::Slot { slot, wire_id, .. } = mapping.target {
                    let plain = mapping
                        .map
                        .plain(mapping.normalized(values[usize::from(mapping.driver)]));
                    emit(slot, wire_id, plain);
                }
            }
        }
    }

    /// Visit compiled plug-in parameter lanes and parameters driven by automated macros.
    /// For a macro target, the value is its output at the stored macro position.
    pub fn for_each_automated_parameter(&self, mut emit: impl FnMut(DeviceId, u32, Option<f64>)) {
        for lane in self
            .tracks
            .iter()
            .flat_map(|track| track.automation.iter())
            .chain(self.master.automation.iter())
        {
            if let EngineTarget::Slot {
                device, wire_id, ..
            } = lane.target
            {
                emit(device, wire_id, None);
            }
        }
        for track in &self.tracks {
            for mapping in track.macros.targets.iter() {
                if let EngineTarget::Slot {
                    device, wire_id, ..
                } = mapping.target
                    && track.automation.iter().any(|lane| {
                        lane.target == EngineTarget::Macro(mapping.driver) && lane.reads()
                    })
                {
                    let base = track.macros.base[usize::from(mapping.driver)];
                    let value = mapping.map.plain(mapping.normalized(base));
                    emit(device, wire_id, Some(value));
                }
            }
        }
    }

    /// Resolves a model lane to its runtime identity in this program.
    pub fn automation_target(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> Option<EngineTarget> {
        self.automation_of(owner)?
            .iter()
            .find(|lane| &lane.model == target)
            .map(|lane| lane.target)
    }

    /// Whether a resolved target has an effective playback curve, including clip overrides.
    pub fn automation_reads(&self, owner: AutomationOwner, target: &AutomationTarget) -> bool {
        self.automation_of(owner)
            .and_then(|automation| automation.iter().find(|lane| &lane.model == target))
            .is_some_and(CompiledTarget::reads)
    }

    /// Mode of the resolved lane in the installed program.
    pub fn automation_mode(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> Option<AutomationMode> {
        self.automation_of(owner)?
            .iter()
            .find(|lane| &lane.model == target)
            .map(|lane| lane.mode)
    }

    /// Compiled base value of this lane, in its normalised domain.
    pub fn automation_base(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> Option<f32> {
        self.automation_of(owner)?
            .iter()
            .find(|lane| &lane.model == target)
            .map(|lane| lane.base)
    }

    /// Latest normalised override applied by the engine, including during stopped playback.
    /// When the override ends, the current lane curve supplies the effective value.
    pub fn automation_override(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> Option<f32> {
        self.automation_of(owner)?
            .iter()
            .find(|lane| &lane.model == target)?
            .applied_override()
    }

    /// Converts a control's plain value to its compiled lane domain.
    ///
    /// Fader and send values are in dB, pan is −1 to +1, and device values are given in
    /// their own plain ranges. Returns `None` for an unresolved target.
    pub fn automation_parameter_value(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
        plain: f64,
    ) -> Option<f32> {
        let lane = self
            .automation_of(owner)?
            .iter()
            .find(|lane| &lane.model == target)?;
        Some(match lane.target {
            EngineTarget::Slot { .. } => lane.map.normalize(plain),
            EngineTarget::Gain | EngineTarget::Send(_) => {
                1.0 - komponist_model::params::db_to_p(plain as f32)
            }
            EngineTarget::Pan => (plain.clamp(-1.0, 1.0) as f32 + 1.0) * 0.5,
            EngineTarget::Macro(_) => plain.clamp(0.0, 1.0) as f32,
        })
    }

    /// Project identity corresponding to an engine capture event.
    pub fn automation_model_target(
        &self,
        owner: AutomationOwner,
        target: EngineTarget,
    ) -> Option<&AutomationTarget> {
        self.automation_of(owner)?
            .iter()
            .find(|lane| lane.target == target)
            .map(|lane| &lane.model)
    }

    fn automation_of(&self, owner: AutomationOwner) -> Option<&[CompiledTarget]> {
        match owner {
            AutomationOwner::Track(id) => Some(&self.track(id)?.automation),
            AutomationOwner::Master => Some(&self.master.automation),
        }
    }

    /// The level meter of track `id` (the UI reads it every frame with [`TrackMeter::take`]).
    pub fn meter(&self, id: TrackId) -> Option<&Arc<TrackMeter>> {
        self.track(id).map(|t| &t.meter)
    }

    /// The files audio clips play in this program, each once (status for the UI's missing-file
    /// marks: [`ClipSource::status`]).
    pub fn audio_sources(&self) -> Vec<Arc<ClipSource>> {
        let mut out: Vec<Arc<ClipSource>> = Vec::new();
        for source in self
            .tracks
            .iter()
            .filter_map(|t| t.audio.as_ref())
            .flat_map(|p| p.sources())
        {
            if !out.iter().any(|s| Arc::ptr_eq(s, source)) {
                out.push(Arc::clone(source));
            }
        }
        out
    }

    /// The file the audio clip `clip` of track `track` plays (`None` for a clip that makes no
    /// segment: muted, empty, or not an audio clip).
    pub fn clip_source(
        &self,
        track: TrackId,
        clip: komponist_model::ClipId,
    ) -> Option<&Arc<ClipSource>> {
        self.track(track)?.audio.as_ref()?.source_of(clip)
    }
}

/// One compiled track.
#[derive(Debug, Clone)]
pub struct TrackProgram {
    /// The track.
    pub id: TrackId,
    /// The model track this was compiled from; the compiler reuses this `TrackProgram` while the
    /// track `Arc` is unchanged (`Arc::ptr_eq`). UI thread only.
    pub source: Arc<Track>,
    /// All notes the track plays, sorted by start and key, with large MIDI loops kept compact.
    pub notes: Arc<NoteSchedule>,
    /// The processor chain in order: MIDI effects, the instrument, audio effects.
    pub chain: Vec<ChainSlot>,
    /// Fader gain, linear (from `volume_db`; `VOLUME_DB_MIN` → 0.0). Ramped by the engine.
    pub gain: f32,
    /// Balance `-1.0 ..= 1.0`, applied with `klang_dsp::pan::balance`.
    pub pan: f32,
    /// Mute. In a flat program audible = `!mute && (!any_solo || solo)`, evaluated by the engine;
    /// with routing the program's [`Routing::gates`] decide.
    pub mute: bool,
    /// Solo.
    pub solo: bool,
    /// Record arm: live input is captured while recording.
    pub arm: bool,
    /// Monitoring (I): live input reaches the chain. An armed track monitors as well.
    pub monitor: bool,
    /// Which live MIDI input the track listens to.
    pub input: InputFilter,
    /// Delay of the main output in frames, so that it lines up with everything else reaching its
    /// target (the master or a bus): `in(target) − out(track)` of ARCHITECTURE §6.8.5, without
    /// buses the max chain latency − own chain latency. ≤ [`crate::MAX_PDC_FRAMES`]; 0 when
    /// compensation is off.
    pub pdc_delay: u32,
    /// Peak meter after fader, pan, mute and solo (buses too); the same `Arc` survives rebuilds of
    /// the track.
    pub meter: Arc<TrackMeter>,
    /// Resolved track macros.
    pub macros: MacroProgram,
    /// Compiled automation lanes, sorted by target.
    pub automation: Arc<[CompiledTarget]>,
    /// Delay to the fader from processors in this track.
    pub chain_latency: u32,
    /// What an audio track plays (`Some` on audio tracks, also without clips); its source node
    /// replaces sequencer and instrument at the head of the chain (ARCHITECTURE §6.9.3).
    pub audio: Option<Arc<AudioPlan>>,
}

impl TrackProgram {
    /// Number of MIDI effects at the head of the chain (WP-88).
    pub fn stage_count(&self) -> usize {
        self.chain
            .iter()
            .take_while(|d| d.role == DeviceRole::MidiEffect)
            .count()
    }

    /// The instrument device: right behind the MIDI effects.
    pub fn instrument(&self) -> Option<&ChainSlot> {
        self.chain
            .get(self.stage_count())
            .filter(|d| d.role == DeviceRole::Instrument)
    }
}

/// The master strip of a program.
#[derive(Debug, Clone)]
pub struct MasterProgram {
    /// Compiled automation lanes, sorted by target.
    pub automation: Arc<[CompiledTarget]>,
    /// Source lanes retained for incremental compilation.
    pub automation_source: Arc<TrackAutomation>,
    /// Delay from the song clock through the longest route and the master inserts.
    pub chain_latency: u32,
    /// Master inserts in order (effects only).
    pub chain: Vec<ChainSlot>,
    /// Master fader gain, linear.
    pub gain: f32,
    /// Master balance `-1.0 ..= 1.0`.
    pub pan: f32,
    /// Master mute.
    pub mute: bool,
}

/// One device of a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainSlot {
    /// The device.
    pub device: DeviceId,
    /// Its processor slot.
    pub slot: SlotId,
    /// Instrument or effect: decides the fallback (silence or passthrough).
    pub role: DeviceRole,
    /// Bypassed: the engine skips `process` (passthrough / silence) and calls `reset` once when
    /// bypass turns on.
    pub bypass: bool,
}

/// Live MIDI routing of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputFilter {
    /// `None` = all ports; `Some(p)` = only port `p`.
    pub port: Option<MidiPort>,
    /// `None` = Omni; `Some(c)` = only MIDI channel `c` (0 ..= 15).
    pub channel: Option<u8>,
    /// The track names a port that is not present: it receives nothing.
    pub port_missing: bool,
}

/// One note of the sequence, in absolute timeline ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqNote {
    /// Start tick.
    pub start: Tick,
    /// Length in ticks, ≥ 1. The engine sends the note-off from its active-notes table at
    /// `start + length`, at a loop wrap, on stop or locate, whichever comes first.
    pub length: Tick,
    /// MIDI key 0 ..= 127.
    pub key: u8,
    /// MIDI velocity 1 ..= 127.
    pub velocity: u8,
    /// Chance in percent 0 ..= 100 that the note plays, rolled by the engine with a per-track
    /// `klang_dsp::rng::Rng` seeded per playback start; or [`PROBABILITY_FOLLOW`].
    pub probability: u8,
    /// MIDI channel 0 ..= 15 (0 for everything the sequencer generates in v0.1).
    pub channel: u8,
}

/// Which slot each device uses. Owned by a program; built by the compiler from the previous
/// program's map, so a device keeps its slot as long as it exists.
///
/// Slot ids freed by a build are reused only from the next build on, after callers have queued the
/// intermediate program and its processor removals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotMap {
    /// Pairs sorted by device id.
    pub(crate) entries: Vec<(DeviceId, SlotId)>,
    /// Slots that may be reused by the next build.
    pub(crate) free: Vec<SlotId>,
    /// Lowest never-used slot id.
    pub(crate) next: u32,
}

impl SlotMap {
    /// The slot of `device` in this map.
    pub fn slot(&self, device: DeviceId) -> Option<SlotId> {
        self.entries
            .binary_search_by_key(&device, |(d, _)| *d)
            .ok()
            .map(|i| self.entries[i].1)
    }

    /// The device assigned to `slot`, for delivering engine events to the UI.
    pub fn device(&self, slot: SlotId) -> Option<DeviceId> {
        self.entries
            .iter()
            .find_map(|&(device, assigned)| (assigned == slot).then_some(device))
    }

    /// All `(device, slot)` pairs, sorted by device id.
    pub fn entries(&self) -> &[(DeviceId, SlotId)] {
        &self.entries
    }
}
