//! Every change to a project, as a pure function (ARCHITECTURE §5.3).
//!
//! Shape: `fn(&Project, args) -> Result<Project, EditError>`; edits that create objects return the
//! new ids as well (`Result<(Project, Id), EditError>`). An edit clones only the path to what it
//! changes (`Arc::make_mut`), allocates ids from [`Project::alloc_id`], keeps all invariants
//! documented in [`crate::project`] (sorted clips and notes, non-empty takes, …) and never mutates
//! its input. On error the input is untouched and no partial result exists.
//!
//! Callers go through [`crate::Document::apply`] / [`crate::Document::apply_with`] so every edit
//! becomes an undo step (or part of a gesture). The app turns UI interactions into these calls; the
//! acceptance tests call them directly.
//!
//! Conventions:
//! - Objects are addressed by id. Clips, notes and devices are found anywhere in the project.
//! - Note edits work on the **active take** of a MIDI clip.
//! - Numeric values from drags (volume, pan, velocity, swing, …) are **clamped** into their range;
//!   structurally invalid input (unknown id, wrong clip kind, hue not in [`Hue::ALL`], meter with a
//!   bad denominator) is an [`EditError`].
//! - Snapping is the caller's job (grid helpers in [`crate::time::Grid`]); edits take exact ticks.

use std::fmt;
use std::sync::Arc;

use crate::ids::{ClipId, DeviceId, NoteId, TrackId};
use crate::project::{
    ChannelRef, ClipKind, DeviceRole, Hue, MidiChannelSel, MidiInputSel, PluginRef, Project,
};
use crate::time::{Meter, StepResolution, Tempo, Tick, TickRange};

// ------------------------------------------------------------------------------------------------
// Errors
// ------------------------------------------------------------------------------------------------

/// Why an edit was rejected. The input project is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// No track with this id.
    TrackNotFound(TrackId),
    /// No clip with this id.
    ClipNotFound(ClipId),
    /// No note with this id in the clip's active take.
    NoteNotFound(NoteId),
    /// No device with this id.
    DeviceNotFound(DeviceId),
    /// The clip has the other content kind (note edit on a step clip, step edit on a MIDI clip).
    WrongClipKind {
        /// The clip.
        clip: ClipId,
        /// The kind the edit needs.
        expected: ClipKind,
    },
    /// An index (track position, chain slot, step row, step, take) is out of range.
    IndexOutOfRange {
        /// What the index addresses ("step row", "take", …).
        what: &'static str,
        /// The rejected index.
        index: usize,
    },
    /// A value that clamping cannot repair (hue not in [`Hue::ALL`], meter denominator not a power
    /// of two, tempo outside the range, an instrument as effect, …).
    Invalid(&'static str),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::TrackNotFound(id) => write!(f, "track {} not found", id.0),
            EditError::ClipNotFound(id) => write!(f, "clip {} not found", id.0),
            EditError::NoteNotFound(id) => write!(f, "note {} not found", id.0),
            EditError::DeviceNotFound(id) => write!(f, "device {} not found", id.0),
            EditError::WrongClipKind { clip, expected } => {
                write!(f, "clip {} is not a {expected:?} clip", clip.0)
            }
            EditError::IndexOutOfRange { what, index } => write!(f, "{what} {index} out of range"),
            EditError::Invalid(reason) => write!(f, "invalid edit: {reason}"),
        }
    }
}

impl std::error::Error for EditError {}

/// Result of an edit that returns only the new root.
pub type EditResult = Result<Project, EditError>;

// ------------------------------------------------------------------------------------------------
// Argument types
// ------------------------------------------------------------------------------------------------

/// A project-level property.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectProp {
    /// Project name (non-empty after trimming, else `Invalid`).
    Name(String),
    /// Tempo; outside [`Tempo::MIN_BPM`] ..= [`Tempo::MAX_BPM`] is `Invalid`.
    Tempo(Tempo),
    /// Meter; `num` 1 ..= 32 and `den` in {1, 2, 4, 8, 16}, else `Invalid`.
    Meter(Meter),
    /// Loop range; an empty range is `Invalid`, a start before 0 is clamped to 0. Apply with
    /// [`crate::Document::apply_untracked`] when set from the ruler.
    LoopRange(TickRange),
    /// Loop on/off. Apply with [`crate::Document::apply_untracked`].
    LoopOn(bool),
    /// Metronome on/off. Apply with [`crate::Document::apply_untracked`].
    Metronome(bool),
    /// Count-in bars, clamped to 0 ..= 4. Apply with [`crate::Document::apply_untracked`].
    CountInBars(u8),
}

/// What a new track gets.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTrack {
    /// Name; `None` = the instrument's name (klopfer: "Drums"), or "MIDI" without instrument, made
    /// unique with [`unique_track_name`].
    pub name: Option<String>,
    /// Colour; `None` = [`next_hue`].
    pub hue: Option<Hue>,
    /// Instrument to put at `devices[0]`; `None` = no instrument.
    pub instrument: Option<PluginRef>,
    /// Display index to insert at; `None` = append at the end.
    pub index: Option<usize>,
    /// Arm the new track, disarm all others and switch its monitoring on (prototype `addTrack`, S10:
    /// an instrument dropped into the arrangement makes an armed track).
    pub arm_exclusive: bool,
}

/// A mixer value of a channel (track or master).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelProp {
    /// Fader level in dB, clamped to `VOLUME_DB_MIN ..= VOLUME_DB_MAX`; `-inf` becomes
    /// `VOLUME_DB_MIN`.
    VolumeDb(f32),
    /// Balance, clamped to `-1.0 ..= 1.0`.
    Pan(f32),
    /// Mute.
    Mute(bool),
    /// Solo (ignored on the master).
    Solo(bool),
}

/// A track property; applies to every track in the id list (drag across toggles, Alt height).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackProp {
    /// Rename (trimmed; empty is `Invalid`).
    Name(String),
    /// Colour; must be one of [`Hue::ALL`].
    Hue(Hue),
    /// A mixer value of the track's channel.
    Channel(ChannelProp),
    /// Record arm. `Arm(true)` also switches monitoring on (S02).
    Arm(bool),
    /// Input monitoring.
    Monitor(bool),
    /// MIDI input.
    Input(MidiInputSel),
    /// MIDI channel; `Channel(c)` with `c > 15` is `Invalid`.
    MidiChannel(MidiChannelSel),
    /// Header height, clamped to `TRACK_HEIGHT_MIN ..= TRACK_HEIGHT_MAX`.
    Height(u16),
}

/// Alt-click toggles: only this track has the flag afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExclusiveFlag {
    /// Solo only this track.
    Solo,
    /// Arm only this track (and switch its monitoring on).
    Arm,
}

/// A note to create; ids are allocated by the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NewNote {
    /// Clip-local start, clamped to ≥ 0.
    pub start: Tick,
    /// Length, clamped to ≥ 1 tick.
    pub length: Tick,
    /// Pitch, clamped to `0 ..= 127`.
    pub pitch: u8,
    /// Velocity, clamped to `1 ..= 127`.
    pub velocity: u8,
}

/// Name and note of a step row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepRowSpec {
    /// Row name ("Kick", or a CLAP note name).
    pub name: String,
    /// MIDI note the row plays.
    pub note: u8,
}

/// Content of a new clip.
#[derive(Debug, Clone, PartialEq)]
pub enum NewClipContent {
    /// A MIDI clip with one take holding these notes (empty = an empty clip; the piano roll then
    /// preselects the pencil).
    Midi {
        /// Initial notes, clip-local.
        notes: Vec<NewNote>,
        /// Name of the first take ("" for a drawn clip, "Take n" for a recording).
        take_name: String,
    },
    /// A step clip with these rows (empty = [`crate::project::DEFAULT_STEP_ROWS`]), 16 steps at 1/16,
    /// no swing, all steps off.
    Step {
        /// Row names and notes, top to bottom.
        rows: Vec<StepRowSpec>,
    },
}

/// What a new clip gets.
#[derive(Debug, Clone, PartialEq)]
pub struct NewClip {
    /// Timeline start, clamped to ≥ 0.
    pub start: Tick,
    /// Length, clamped to ≥ 1 tick (a new clip from a double-click is one bar).
    pub length: Tick,
    /// Clip name ("MIDI-Clip", "Pattern 2", "● Aufnahme"), chosen by the caller.
    pub name: String,
    /// Content.
    pub content: NewClipContent,
}

/// A clip property set from the inspector.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipProp {
    /// Rename (trimmed; empty is `Invalid`).
    Name(String),
    /// Timeline start, clamped to ≥ 0 (keeps the clip on its track).
    Start(Tick),
    /// Transposition in semitones, clamped to `-48 ..= 48`.
    Transpose(i8),
    /// Velocity change in percent, clamped to `-100 ..= 100`.
    VelocityPct(i8),
}

/// How [`set_notes_velocity`] changes velocities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VelocityChange {
    /// Every note gets this velocity (velocity lane), clamped to `1 ..= 127`.
    Set(u8),
    /// Every note's velocity moves by this amount (Ctrl-drag ↕), clamped per note to `1 ..= 127`.
    Offset(i16),
}

/// A property of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepProp {
    /// On or off (click, paint drag). Turning on keeps the stored velocity, probability, ratchet.
    On(bool),
    /// Velocity, clamped to `1 ..= 127` (drag ↕, lane).
    Velocity(u8),
    /// Probability in percent, clamped to `0 ..= 100` (Alt-drag ↕).
    Probability(u8),
    /// Ratchet, clamped to `1 ..= 4` (Shift-click cycles 1 → 2 → 3 → 4 → 1; the caller computes the
    /// next value).
    Ratchet(u8),
}

/// A pattern property of a step clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepClipProp {
    /// Pattern length in steps, clamped to `1 ..= 32`. Doubling from `n` to `2n` while steps
    /// `n .. 2n` of every row are off copies steps `0 .. n` into them (prototype `toggleLen`);
    /// otherwise hidden steps keep their values.
    LengthSteps(u8),
    /// Step resolution.
    Resolution(StepResolution),
    /// Swing, clamped to `0.0 ..= 0.75`.
    Swing(f32),
}

/// A property of a step row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StepRowProp {
    /// Row name.
    Name(String),
    /// MIDI note, clamped to `0 ..= 127`.
    Note(u8),
    /// Row mute.
    Mute(bool),
    /// Row solo.
    Solo(bool),
}

/// A device to insert; the id is allocated by the edit, bypass starts off.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDevice {
    /// Instrument (only at index 0 of a track chain) or effect.
    pub role: DeviceRole,
    /// The plugin.
    pub plugin: PluginRef,
    /// Initial state (`None` = plugin defaults; a preset from the browser passes its state).
    pub state: Option<Arc<[u8]>>,
}

// ------------------------------------------------------------------------------------------------
// Helpers for callers
// ------------------------------------------------------------------------------------------------

/// The hue a new track gets: the first of [`Hue::AUTO_ORDER`] not used by any track; if all are
/// used, `Hue::ALL[track_count % 14]` (prototype `addTrack`).
pub fn next_hue(project: &Project) -> Hue {
    todo!("WP-12")
}

/// `base` if no track has that name, else `"{base} 2"`, `"{base} 3"`, … (first free).
pub fn unique_track_name(project: &Project, base: &str) -> String {
    todo!("WP-12")
}

/// Number for the next recorded take: one more than the highest `n` of any clip or take named
/// `"Take n"` in the project, starting at 1. The caller formats the UI name "Take n".
pub fn next_take_number(project: &Project) -> u32 {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Project
// ------------------------------------------------------------------------------------------------

/// Sets a project-level property.
pub fn set_project_prop(project: &Project, prop: ProjectProp) -> EditResult {
    todo!("WP-12")
}

/// Sets volume, pan or mute of the master strip.
pub fn set_master_prop(project: &Project, prop: ChannelProp) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Tracks
// ------------------------------------------------------------------------------------------------

/// Adds a MIDI track (Ctrl T, instrument drop). Height [`crate::project::TRACK_HEIGHT_DEFAULT`],
/// 0 dB, centred, input "all", Omni.
pub fn add_track(project: &Project, spec: NewTrack) -> Result<(Project, TrackId), EditError> {
    todo!("WP-12")
}

/// Removes tracks with all their clips and devices (Entf with tracks selected).
pub fn remove_tracks(project: &Project, tracks: &[TrackId]) -> EditResult {
    todo!("WP-12")
}

/// Moves tracks (keeping their relative order) so the first of them lands at display index
/// `to_index` of the list *without* the moved tracks (drag reorder, 2 px insert line). `to_index`
/// is clamped to the list end.
pub fn move_tracks(project: &Project, tracks: &[TrackId], to_index: usize) -> EditResult {
    todo!("WP-12")
}

/// Sets a property on every listed track.
pub fn set_track_prop(project: &Project, tracks: &[TrackId], prop: TrackProp) -> EditResult {
    todo!("WP-12")
}

/// Alt-click on S or R: the flag is on for `track` and off for all others.
pub fn set_exclusive(project: &Project, track: TrackId, flag: ExclusiveFlag) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Clips
// ------------------------------------------------------------------------------------------------

/// Adds a clip to a track (double-click on empty lane, recording start).
pub fn add_clip(
    project: &Project,
    track: TrackId,
    spec: NewClip,
) -> Result<(Project, ClipId), EditError> {
    todo!("WP-12")
}

/// Removes clips (Entf).
pub fn remove_clips(project: &Project, clips: &[ClipId]) -> EditResult {
    todo!("WP-12")
}

/// Moves clips by `delta` ticks and `track_delta` tracks (drag; negative = up). The group moves
/// together: `delta` is clamped so no clip starts before tick 0 and `track_delta` so no clip leaves
/// the track list.
pub fn move_clips(
    project: &Project,
    clips: &[ClipId],
    delta: Tick,
    track_delta: i32,
) -> EditResult {
    todo!("WP-12")
}

/// Alt-drag: copies clips (new clip and note ids) to the moved position, originals stay. Same
/// clamping as [`move_clips`]. Returns the copies in the order of `clips`.
pub fn copy_clips(
    project: &Project,
    clips: &[ClipId],
    delta: Tick,
    track_delta: i32,
) -> Result<(Project, Vec<ClipId>), EditError> {
    todo!("WP-12")
}

/// Ctrl D: copies each clip right behind itself (`start + length`) on the same track. Returns the
/// copies in the order of `clips`.
pub fn duplicate_clips(
    project: &Project,
    clips: &[ClipId],
) -> Result<(Project, Vec<ClipId>), EditError> {
    todo!("WP-12")
}

/// Sets the timeline length (right edge drag), clamped to ≥ 1 tick. The start never moves; content
/// beyond the new end is kept but not played.
pub fn set_clip_length(project: &Project, clip: ClipId, length: Tick) -> EditResult {
    todo!("WP-12")
}

/// Sets the clip loop (top-right corner drag, inspector "Loop" / "Loop-Länge"): `loop_length`
/// `Some(l)` with `l ≥ 1` repeats `0 .. l`; `None` turns looping off. `length` is the new timeline
/// length (≥ 1). Step clips accept only `None` (`WrongClipKind` otherwise) and just take the length.
pub fn set_clip_loop(
    project: &Project,
    clip: ClipId,
    loop_length: Option<Tick>,
    length: Tick,
) -> EditResult {
    todo!("WP-12")
}

/// Ctrl E: splits every listed clip that strictly contains `at` (timeline tick) into a left part
/// (same id, ends at `at`) and a right part (new id, starts at `at`). Clips not containing `at` are
/// skipped. MIDI: notes starting before `at` stay left (cut at `at`), the others move right; a looped
/// clip is unrolled first, so both parts are unlooped. Step: both parts keep the pattern; the right
/// part's rows are rotated so the audible result does not change. Returns the right parts.
pub fn split_clips(
    project: &Project,
    clips: &[ClipId],
    at: Tick,
) -> Result<(Project, Vec<ClipId>), EditError> {
    todo!("WP-12")
}

/// Key `0`: mutes or unmutes clips.
pub fn set_clips_muted(project: &Project, clips: &[ClipId], muted: bool) -> EditResult {
    todo!("WP-12")
}

/// Sets a clip property from the inspector.
pub fn set_clip_prop(project: &Project, clip: ClipId, prop: ClipProp) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Notes (active take of a MIDI clip)
// ------------------------------------------------------------------------------------------------

/// Adds notes (pencil, double-click, recording, paste). Returns their ids in input order.
pub fn add_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NewNote],
) -> Result<(Project, Vec<NoteId>), EditError> {
    todo!("WP-12")
}

/// Deletes notes (Entf, eraser, pointer double-click).
pub fn remove_notes(project: &Project, clip: ClipId, notes: &[NoteId]) -> EditResult {
    todo!("WP-12")
}

/// Moves notes in time and pitch (drag, ← →, ↑ ↓, Shift ↑ ↓ = ±12). The group moves together:
/// `delta` is clamped so no note starts before clip-local 0, `pitch_delta` so all stay in
/// `0 ..= 127`.
pub fn move_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    delta: Tick,
    pitch_delta: i32,
) -> EditResult {
    todo!("WP-12")
}

/// Alt-drag: copies notes to the moved position (same clamping as [`move_notes`]); originals stay.
/// Returns the copies in the order of `notes`.
pub fn copy_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    delta: Tick,
    pitch_delta: i32,
) -> Result<(Project, Vec<NoteId>), EditError> {
    todo!("WP-12")
}

/// Edge drag: changes the length of all listed notes by `delta` ticks, each clamped to ≥ 1 tick.
pub fn resize_notes(project: &Project, clip: ClipId, notes: &[NoteId], delta: Tick) -> EditResult {
    todo!("WP-12")
}

/// Changes velocities (Ctrl-drag ↕, velocity lane).
pub fn set_notes_velocity(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    change: VelocityChange,
) -> EditResult {
    todo!("WP-12")
}

/// Key `Q`: snaps start and length of the listed notes (`None` = all notes of the take) to `grid`
/// ticks: `start = round(start / grid) × grid`, `length = max(grid, round(length / grid) × grid)`
/// (prototype `quantize`).
pub fn quantize_notes(
    project: &Project,
    clip: ClipId,
    notes: Option<&[NoteId]>,
    grid: Tick,
) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Steps
// ------------------------------------------------------------------------------------------------

/// Sets a property of step `step` (`0 .. 32`) in row `row` of a step clip.
pub fn set_step(
    project: &Project,
    clip: ClipId,
    row: usize,
    step: usize,
    prop: StepProp,
) -> EditResult {
    todo!("WP-12")
}

/// Sets a pattern property of a step clip (Länge, Auflösung, Swing).
pub fn set_step_clip_prop(project: &Project, clip: ClipId, prop: StepClipProp) -> EditResult {
    todo!("WP-12")
}

/// Sets a property of step row `row`.
pub fn set_step_row(project: &Project, clip: ClipId, row: usize, prop: StepRowProp) -> EditResult {
    todo!("WP-12")
}

/// Replaces the row names and notes of a step clip (instrument changed, CLAP note names arrived).
/// Rows keep their steps by index; extra rows start empty, missing rows are dropped.
pub fn set_step_rows(project: &Project, clip: ClipId, rows: &[StepRowSpec]) -> EditResult {
    todo!("WP-12")
}

/// "Als MIDI-Clip": turns a step clip into a MIDI clip with the same id, name, start and length.
/// The notes are what the step clip plays (pattern repeated over the clip length, swing and
/// ratchets applied, muted and non-soloed rows left out). Steps with probability 0 are left out;
/// other probabilities are dropped (the notes always play).
pub fn step_clip_to_midi(project: &Project, clip: ClipId) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Devices
// ------------------------------------------------------------------------------------------------

/// Inserts a device into a chain at `index` (clamped to the chain length). An instrument may only
/// go to index 0 of a track chain that has no instrument; an effect never goes before a track's
/// instrument (index raised to 1). Master chains take effects only.
pub fn insert_device(
    project: &Project,
    channel: ChannelRef,
    index: usize,
    spec: NewDevice,
) -> Result<(Project, DeviceId), EditError> {
    todo!("WP-12")
}

/// Puts `plugin` as the instrument of `track` (browser drop on a track): replaces the existing
/// instrument (new device id, state `None`) or inserts one at index 0.
pub fn replace_instrument(
    project: &Project,
    track: TrackId,
    plugin: PluginRef,
) -> Result<(Project, DeviceId), EditError> {
    todo!("WP-12")
}

/// Removes a device from its chain.
pub fn remove_device(project: &Project, device: DeviceId) -> EditResult {
    todo!("WP-12")
}

/// Moves an effect within its chain to `to_index` of the chain without it (drag reorder). An
/// instrument cannot move, and no effect can move before it (`Invalid`).
pub fn move_device(project: &Project, device: DeviceId, to_index: usize) -> EditResult {
    todo!("WP-12")
}

/// Sets bypass (switch on the device card, Alt-click on an insert slot).
pub fn set_device_bypass(project: &Project, device: DeviceId, bypass: bool) -> EditResult {
    todo!("WP-12")
}

/// Stores a plugin state snapshot. Apply with [`crate::Document::apply_untracked`] for snapshots
/// taken by the plugin manager; a preset loaded on purpose is an undoable edit.
pub fn set_device_state(
    project: &Project,
    device: DeviceId,
    state: Option<Arc<[u8]>>,
) -> EditResult {
    todo!("WP-12")
}

// ------------------------------------------------------------------------------------------------
// Takes
// ------------------------------------------------------------------------------------------------

/// Appends an empty take named `name` to a MIDI clip and makes it active (loop recording: one take
/// per pass). Returns the take index.
pub fn add_take(
    project: &Project,
    clip: ClipId,
    name: String,
) -> Result<(Project, usize), EditError> {
    todo!("WP-12")
}

/// Makes take `take` the active one (inspector take list).
pub fn select_take(project: &Project, clip: ClipId, take: usize) -> EditResult {
    todo!("WP-12")
}
