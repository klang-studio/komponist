//! Project → [`Program`] compiler (ARCHITECTURE §6.3, §6.5).
//!
//! Runs on the UI thread after each document change (`EngineSync` in the app). Incremental: a
//! track whose `Arc<Track>` is `ptr_eq` to the previous program's
//! [`TrackProgram::source`](crate::program::TrackProgram::source) (and whose
//! environment-derived values did not change) reuses the previous `Arc<TrackProgram>`.
//!
//! Sequence per sync (the app's `EngineSync`):
//! 1. `let next = build(&project, previous.as_ref(), &env);`
//! 2. `let changes = slot_changes(previous.as_ref(), &next);`
//! 3. For each `changes.added`: create the processor (builtin in process, external through
//!    `komponist-plugins`), activate it at the engine's rate, send `Command::InsertProcessor`.
//! 4. Send `Command::SetProgram(Box::new(next.clone()))`, keep `next` as `previous`.
//! 5. For each `changes.removed`: send `Command::RemoveProcessor`.
//!
//! Commands apply in order, so a slot is filled before a program uses it and emptied only after
//! the program that used it is gone.
//!
//! What the compiler does (golden-tested by WP-24):
//! - MIDI clips: the active take; clip loops expanded (`loop_length`), notes cut at repetition and
//!   clip ends; clip transpose and velocity applied; muted clips skipped.
//! - Step clips: the pattern repeated from the clip start to the clip end; swing on odd steps;
//!   ratchets expanded into evenly spaced hits (later hits at 75 % velocity,
//!   [`crate::program::PROBABILITY_FOLLOW`]); probability kept per note; muted rows and, with any
//!   row soloed, non-soloed rows skipped.
//! - Devices → [`ChainSlot`](crate::program::ChainSlot)s with stable slots; volume dB → linear
//!   gain; MIDI input names → [`MidiPort`]s; chain latencies → `pdc_delay`.
//! - Routing (WP-102, ARCHITECTURE §6.8.4–6.8.6): outputs to buses, sends and sidechains →
//!   [`Routing`](crate::route::Routing): the job order (`komponist_model::routing::topo_order`), the
//!   inputs of every bus and of the master in a fixed summing order, the taps, the delay
//!   compensation over the whole graph (every input of a bus and of the master lines up, sends and
//!   sidechains line up with their target, a late sidechain delays its holder), and the gates of
//!   mute and solo (`komponist_model::routing::gates`). A project without buses, sends and
//!   sidechains compiles to the flat routing, exactly as before wave 9.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use komponist_model::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};
use komponist_model::{
    Clip, ClipContent, Device, DeviceId, DeviceRole, MidiChannelSel, MidiClip, MidiInputSel,
    Project, StepClip, Tick, Track,
};

use crate::api::TrackMeter;
use crate::audio::{PlanEnv, SourceCache, build_plan, plan_matches};
use crate::program::{
    ChainSlot, InputFilter, MasterProgram, MidiPort, PROBABILITY_FOLLOW, Program, SeqNote, SlotId,
    SlotMap, TrackProgram,
};
use crate::route::GateTable;
use crate::{MAX_SLOTS, MAX_STAGES, MAX_TRACKS};

mod graph;

/// What the compiler needs from outside the document. Implemented by the app (from
/// `komponist-audio`'s MIDI ports and `komponist-plugins`' latencies); [`NoEnv`] for tests.
pub trait CompileEnv {
    /// The port id of the MIDI input named `device_name`, or `None` if it is not present.
    fn midi_port(&self, device_name: &str) -> Option<MidiPort>;

    /// Latency of `device`'s processor in frames at the engine's sample rate (0 if unknown or
    /// builtin without latency).
    fn device_latency(&self, device: DeviceId) -> u32;

    /// The engine's sample rate in Hz: audio clips are compiled to frames of it (a program whose
    /// audio plan has another rate than the engine plays silence). Default 48 kHz.
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    /// The folder of the project file, against which the relative links of audio clips resolve
    /// (`None` before the project has a file).
    fn project_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// An environment without MIDI devices and with zero latencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoEnv;

impl CompileEnv for NoEnv {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, _device: DeviceId) -> u32 {
        0
    }
}

/// Compiles `project` into a program, reusing unchanged track programs and the slot assignment of
/// `previous`. Pure apart from allocation; never fails. Tracks beyond [`crate::MAX_TRACKS`] are
/// left out.
///
/// Invalid data is skipped: notes outside `0 ..= 127` after transposition, notes shorter than one
/// tick or before tick 0, a MIDI clip whose active take does not exist, tracks and devices whose
/// id appeared before (the first occurrence in project order wins, the master comes last),
/// instruments anywhere but right behind the MIDI effects of a track chain, MIDI effects anywhere
/// but at the head of a track chain, instruments on the master, and devices that
/// find no free slot below [`crate::MAX_SLOTS`].
pub fn build(project: &Project, previous: Option<&Program>, env: &dyn CompileEnv) -> Program {
    let mut track_ids = HashSet::new();
    let tracks: Vec<&Arc<Track>> = project
        .tracks
        .iter()
        .filter(|t| track_ids.insert(t.id))
        .take(MAX_TRACKS)
        .collect();

    let mut device_ids = HashSet::new();
    let track_chains: Vec<Vec<&Device>> = tracks
        .iter()
        .map(|t| {
            // A bus runs audio effects only (it never holds an instrument or MIDI effects), and so
            // does an audio track: its source node is the clip playback (`AudioPlan`).
            let kind = if t.is_bus() || t.is_audio() {
                ChainKind::Master
            } else {
                ChainKind::Track
            };
            valid_chain(&t.channel.devices, kind, &mut device_ids)
        })
        .collect();
    let master_chain = valid_chain(&project.master.devices, ChainKind::Master, &mut device_ids);
    let slots = assign_slots(previous.map(|p| &*p.slots), device_ids);

    let chains: Vec<Vec<ChainSlot>> = track_chains
        .iter()
        .map(|chain| chain_slots(chain, &slots))
        .collect();
    let master_chain = chain_slots(&master_chain, &slots);
    let graph = graph::compile(
        project,
        &tracks,
        &chains,
        &master_chain,
        env,
        previous.map(|p| &*p.routing),
    );
    let dir = env.project_dir();
    let plan_env = PlanEnv {
        rate: env.sample_rate().round().max(1.0) as u32,
        tempo: project.tempo_map(),
        dir: dir.as_deref(),
    };
    let mut cache = SourceCache::new(
        previous
            .into_iter()
            .flat_map(|p| p.tracks.iter())
            .filter_map(|t| t.audio.as_ref())
            .flat_map(|plan| plan.sources().iter().cloned()),
    );
    let tracks = tracks
        .into_iter()
        .zip(chains)
        .zip(&graph.delays)
        .map(|((track, chain), &pdc_delay)| {
            let routing = TrackEnv {
                chain,
                input: input_filter(track, env),
                pdc_delay,
            };
            let previous = previous.and_then(|p| p.track(track.id));
            track_program(track, routing, previous, &plan_env, &mut cache)
        })
        .collect();

    let master = &project.master;
    Program {
        tempo_map: project.tempo_map(),
        loop_range: project.loop_range,
        loop_on: project.loop_on,
        metronome: project.metronome,
        count_in_bars: project.count_in_bars,
        song_end: project.song_end(),
        tracks,
        master: MasterProgram {
            chain: master_chain,
            gain: fader_gain(master.volume_db),
            pan: balance(master.pan),
            mute: master.mute,
        },
        slots: Arc::new(slots),
        routing: graph.routing,
    }
}

/// The gate table of `project`'s mute and solo states for `program` (compiled from an earlier
/// state of the same project): what `Command::SetGates` carries on the fast path when mute or
/// solo change during a gesture (ARCHITECTURE §6.8.6). Tracks and sends the program does not have
/// are left out; tracks of the program missing in `project` are closed.
pub fn gates(project: &Project, program: &Program) -> GateTable {
    let tracks: Vec<_> = program.tracks.iter().map(|t| t.id).collect();
    graph::gate_table(project, &tracks, &program.routing.sends)
}

/// Devices whose slot appeared or disappeared between two programs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotChanges {
    /// Devices new in `next`, with their slot: create and insert a processor.
    pub added: Vec<(DeviceId, SlotId)>,
    /// Devices gone in `next`, with the slot they had: remove the processor.
    pub removed: Vec<(DeviceId, SlotId)>,
}

/// Compares the slot maps of `previous` (all devices are new when `None`) and `next`.
/// A device whose slot differs between the two maps (never produced by [`build`] with the same
/// `previous`) counts as removed and added.
pub fn slot_changes(previous: Option<&Program>, next: &Program) -> SlotChanges {
    let old = previous.map_or(&[][..], |p| p.slots.entries());
    let new = next.slots.entries();
    // Entries are sorted by device id and hold each device once, so they are sorted as pairs too.
    let missing = |from: &[(DeviceId, SlotId)], other: &[(DeviceId, SlotId)]| {
        from.iter()
            .filter(|e| other.binary_search(e).is_err())
            .copied()
            .collect()
    };
    SlotChanges {
        added: missing(new, old),
        removed: missing(old, new),
    }
}

// ------------------------------------------------------------------------------------------------
// Devices and slots
// ------------------------------------------------------------------------------------------------

/// Which chain a device list belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainKind {
    /// A track chain: MIDI effects, then an instrument may come first.
    Track,
    /// The master inserts and a bus chain: effects only.
    Master,
}

/// The devices of `devices` that may play: each id once across the project (`seen` collects them);
/// MIDI effects only at the head of a track chain (at most [`MAX_STAGES`]), an instrument only
/// right behind them.
fn valid_chain<'a>(
    devices: &'a [Arc<Device>],
    kind: ChainKind,
    seen: &mut HashSet<DeviceId>,
) -> Vec<&'a Device> {
    let midi = devices
        .iter()
        .take_while(|d| d.role == DeviceRole::MidiEffect)
        .count();
    devices
        .iter()
        .enumerate()
        .filter(|(index, d)| match d.role {
            DeviceRole::Effect => true,
            DeviceRole::MidiEffect => kind == ChainKind::Track && *index < midi.min(MAX_STAGES),
            DeviceRole::Instrument => kind == ChainKind::Track && *index == midi,
        })
        .filter(|(_, d)| seen.insert(d.id))
        .map(|(_, d)| &**d)
        .collect()
}

/// The slot map for `devices`: devices of `previous` keep their slot; new devices take the
/// smallest slot freed by the build before `previous`, else the next never-used slot. Slots of
/// devices that are gone become reusable from the next build on (their processors leave the engine
/// only after this program arrives).
fn assign_slots(previous: Option<&SlotMap>, devices: HashSet<DeviceId>) -> SlotMap {
    let empty = SlotMap::default();
    let previous = previous.unwrap_or(&empty);
    let mut devices: Vec<DeviceId> = devices.into_iter().collect();
    devices.sort_unstable();
    // Descending, so `pop` yields the smallest.
    let mut reusable = previous.free.clone();
    reusable.sort_unstable_by(|a, b| b.cmp(a));
    let mut next = previous.next;
    let mut entries = Vec::with_capacity(devices.len());
    for device in devices {
        let slot = previous
            .slot(device)
            .or_else(|| reusable.pop())
            .or_else(|| {
                (next < MAX_SLOTS as u32).then(|| {
                    next += 1;
                    SlotId(next - 1)
                })
            });
        if let Some(slot) = slot {
            entries.push((device, slot));
        }
    }
    let mut free = reusable;
    free.extend(
        previous
            .entries
            .iter()
            .filter(|(d, _)| entries.binary_search_by_key(d, |(e, _)| *e).is_err())
            .map(|&(_, slot)| slot),
    );
    free.sort_unstable();
    SlotMap {
        entries,
        free,
        next,
    }
}

/// The chain slots of `chain`; devices without a slot are left out.
fn chain_slots(chain: &[&Device], slots: &SlotMap) -> Vec<ChainSlot> {
    chain
        .iter()
        .filter_map(|d| {
            slots.slot(d.id).map(|slot| ChainSlot {
                device: d.id,
                slot,
                role: d.role,
                bypass: d.bypass,
            })
        })
        .collect()
}

/// Latency of a chain in frames: the sum over devices that are not bypassed (a bypassed device
/// passes audio through undelayed).
fn chain_latency(chain: &[ChainSlot], env: &dyn CompileEnv) -> u32 {
    chain.iter().filter(|s| !s.bypass).fold(0, |sum: u32, s| {
        sum.saturating_add(env.device_latency(s.device))
    })
}

// ------------------------------------------------------------------------------------------------
// Tracks
// ------------------------------------------------------------------------------------------------

/// The parts of a track program that depend on more than the track: slots, MIDI ports and the
/// latencies of all tracks.
struct TrackEnv {
    chain: Vec<ChainSlot>,
    input: InputFilter,
    pdc_delay: u32,
}

/// The program of `track`. Reuses `previous` (the previous program of the same track id) when the
/// track `Arc` and the routing are unchanged, and its notes while the clip `Arc`s are unchanged
/// (mixer, device and input edits). The meter `Arc` survives every rebuild.
fn track_program(
    track: &Arc<Track>,
    routing: TrackEnv,
    previous: Option<&Arc<TrackProgram>>,
    plan_env: &PlanEnv<'_>,
    cache: &mut SourceCache,
) -> Arc<TrackProgram> {
    // An audio track keeps its plan (its ring, its heads, its open files) while its clips, the
    // rate, the tempo map and the project folder are the same.
    let kept_plan = previous
        .and_then(|p| p.audio.as_ref())
        .filter(|plan| plan_matches(plan, &track.clips, plan_env));
    if let Some(prev) = previous
        && Arc::ptr_eq(&prev.source, track)
        && prev.chain == routing.chain
        && prev.input == routing.input
        && prev.pdc_delay == routing.pdc_delay
        && prev.audio.is_some() == (track.is_audio() && kept_plan.is_some())
    {
        return Arc::clone(prev);
    }
    let audio = track.is_audio().then(|| match kept_plan {
        Some(plan) => Arc::clone(plan),
        None => Arc::new(build_plan(
            track,
            plan_env,
            cache,
            previous.and_then(|p| p.audio.as_deref()),
        )),
    });
    let notes = match previous {
        Some(prev) if same_clips(&prev.source.clips, &track.clips) => Arc::clone(&prev.notes),
        _ => compile_notes(&track.clips),
    };
    let channel = &track.channel;
    Arc::new(TrackProgram {
        id: track.id,
        source: Arc::clone(track),
        notes,
        chain: routing.chain,
        gain: fader_gain(channel.volume_db),
        pan: balance(channel.pan),
        mute: channel.mute,
        solo: channel.solo,
        arm: track.arm,
        monitor: track.monitor,
        input: routing.input,
        pdc_delay: routing.pdc_delay,
        meter: previous.map_or_else(|| Arc::new(TrackMeter::new()), |p| Arc::clone(&p.meter)),
        audio,
    })
}

/// Whether two clip lists hold the same clip `Arc`s in the same order.
fn same_clips(a: &[Arc<Clip>], b: &[Arc<Clip>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| Arc::ptr_eq(a, b))
}

/// Live MIDI routing: a named input resolves to its port, or receives nothing when it is absent.
/// A channel above 15 (invalid) is read as 15.
fn input_filter(track: &Track, env: &dyn CompileEnv) -> InputFilter {
    let (port, port_missing) = match &track.input {
        MidiInputSel::All => (None, false),
        MidiInputSel::Device(name) => match env.midi_port(name) {
            Some(port) => (Some(port), false),
            None => (None, true),
        },
    };
    let channel = match track.midi_channel {
        MidiChannelSel::Omni => None,
        MidiChannelSel::Channel(c) => Some(c.min(15)),
    };
    InputFilter {
        port,
        channel,
        port_missing,
    }
}

/// Linear fader gain of `volume_db`: [`VOLUME_DB_MIN`] (−∞) and below, or NaN, give 0.0; values
/// above [`VOLUME_DB_MAX`] are clamped.
fn fader_gain(volume_db: f32) -> f32 {
    if volume_db > VOLUME_DB_MIN {
        klang_dsp::units::db_to_gain(volume_db.min(VOLUME_DB_MAX))
    } else {
        0.0
    }
}

/// Balance clamped to `-1.0 ..= 1.0`; NaN is centre.
fn balance(pan: f32) -> f32 {
    if pan.is_nan() {
        0.0
    } else {
        pan.clamp(-1.0, 1.0)
    }
}

// ------------------------------------------------------------------------------------------------
// Notes
// ------------------------------------------------------------------------------------------------

/// Probability of notes from MIDI clips: they always play.
const ALWAYS: u8 = 100;

/// All notes of the unmuted `clips`, sorted by start, then key (stable, so equal notes keep clip
/// order).
fn compile_notes(clips: &[Arc<Clip>]) -> Arc<[SeqNote]> {
    let mut notes = Vec::new();
    for clip in clips.iter().filter(|c| !c.mute) {
        match &clip.content {
            ClipContent::Midi(midi) => midi_notes(clip, midi, &mut notes),
            ClipContent::Step(steps) => step_notes(clip, steps, &mut notes),
            // Audio clips make no notes; the track's `AudioPlan` plays them.
            ClipContent::Audio(_) => {}
        }
    }
    notes.sort_by_key(|n| (n.start, n.key));
    notes.into()
}

/// The active take, played once or, with a clip loop, every `loop_length` ticks; each note is cut
/// at its repetition's end and at the clip end. Notes starting at or after `loop_length` never
/// play in a looped clip.
fn midi_notes(clip: &Clip, midi: &MidiClip, out: &mut Vec<SeqNote>) {
    let Some(take) = midi.takes.get(midi.active_take) else {
        return;
    };
    let length = clip.length;
    if length <= Tick::ZERO {
        return;
    }
    let period = match clip.loop_length {
        Some(l) if l > Tick::ZERO => l,
        _ => length,
    };
    let mut offset = Tick::ZERO;
    while offset < length {
        for note in take.notes.iter() {
            let start = offset + note.start;
            if note.start < Tick::ZERO || note.start >= period || start >= length {
                continue;
            }
            let end = (start + note.length).min(offset + period).min(length);
            out.extend(place(
                clip,
                SeqNote {
                    start,
                    length: end - start,
                    key: note.pitch,
                    velocity: note.velocity,
                    probability: ALWAYS,
                    channel: 0,
                },
            ));
        }
        offset += period;
    }
}

/// The step pattern over the clip length ([`StepClip::hits`]: repetition, swing, ratchets, row
/// mute and solo). The first hit of a step carries its probability, later hits follow its roll.
fn step_notes(clip: &Clip, steps: &StepClip, out: &mut Vec<SeqNote>) {
    for hit in steps.hits(clip.length) {
        out.extend(place(
            clip,
            SeqNote {
                start: hit.start,
                length: hit.length,
                key: hit.note,
                velocity: hit.velocity,
                probability: if hit.hit == 0 {
                    hit.probability
                } else {
                    PROBABILITY_FOLLOW
                },
                channel: 0,
            },
        ));
    }
}

/// Moves a clip-local note to the timeline and applies the clip's transposition and velocity
/// change. `None` for invalid notes: key outside `0 ..= 127` (before or after transposition),
/// length below 1, start before tick 0.
fn place(clip: &Clip, local: SeqNote) -> Option<SeqNote> {
    let key = i16::from(local.key) + i16::from(clip.transpose);
    let start = clip.start + local.start;
    let valid = local.key <= 127
        && (0..=127).contains(&key)
        && local.length >= Tick(1)
        && start >= Tick::ZERO;
    valid.then(|| SeqNote {
        start,
        key: key as u8,
        velocity: scale_velocity(local.velocity, clip.velocity_pct),
        ..local
    })
}

/// `velocity × (1 + pct / 100)` rounded half up, clamped to `1 ..= 127` (`pct` clamped to
/// `-100 ..= 100`).
fn scale_velocity(velocity: u8, pct: i8) -> u8 {
    let factor = 100 + i32::from(pct.clamp(-100, 100));
    ((i32::from(velocity) * factor + 50) / 100).clamp(1, 127) as u8
}
