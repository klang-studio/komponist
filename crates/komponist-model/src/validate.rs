//! Whole-project validation and the repair pass of loading (#209).
//!
//! - [`Project::validate`] checks every invariant the edits maintain (the model docs of
//!   [`crate::project`]). Edits keep it true; the tests run it after every edit.
//! - [`repair_loaded`] makes a deserialised project satisfy it, or says why that cannot be done.
//!   [`crate::persist`] runs it on every file, so a hand-edited or damaged file can neither crash
//!   the app (a note at pitch 200, a clip at tick 9·10¹⁸) nor exhaust its memory (a step pattern a
//!   billion ticks long).
//!
//! Policy per invariant, from the least to the most destructive: a value with an obvious safe
//! value is clamped; an object that cannot be used (a note without a pitch in range, a device the
//! chain cannot host) is dropped; only what cannot be repaired without guessing rejects the file
//! (meter, tempo, takes, pattern length, audio sources, routing cycles, content a track cannot
//! hold). Duplicate ids get new ids, the first holder in file order keeps its id, so references
//! stay consistent. Every repair is counted in [`Repairs`], which the caller shows to the user.
//!
//! A project that is valid already passes through unchanged.
//!
//! Thread: any non-RT thread.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use crate::audio::{CLIP_GAIN_DB_MAX, CLIP_GAIN_DB_MIN};
use crate::automation::AutomationTarget;
use crate::automation::{LANE_HEIGHT_MAX, LANE_HEIGHT_MIN};
use crate::edits::{MAX_COUNT_IN_BARS, valid_meter};
use crate::project::{
    Channel, Clip, ClipContent, DeviceRole, Hue, MAX_STEPS, Note, Project, StepClip,
    TRACK_HEIGHT_MAX, TRACK_HEIGHT_MIN, Track, TrackKind, VOLUME_DB_MAX, VOLUME_DB_MIN,
};
use crate::time::{MAX_DOC_TICK, Tempo, Tick, TickRange};
use crate::{TrackId, routing};

/// Highest swing of a step clip.
const MAX_SWING: f32 = 0.75;

// ------------------------------------------------------------------------------------------------
// Repair notes
// ------------------------------------------------------------------------------------------------

/// What kind of change loading made to a project file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepairKind {
    /// The loop range was out of bounds or empty and was clamped or reset.
    LoopRange,
    /// The count-in length was above the maximum.
    CountIn,
    /// A fader, pan, send level, track height, lane height or track colour was out of range.
    MixerValues,
    /// A device the master or a track chain cannot host was dropped.
    DroppedDevices,
    /// A track's device chain was put into the order MIDI effects, instrument, effects.
    ChainOrder,
    /// A clip's start, length, loop length, transposition or velocity change was out of range.
    ClipValues,
    /// A clip's loop length was removed (step and audio clips never loop).
    ClipLoops,
    /// A note's start, length or velocity was out of range.
    NoteValues,
    /// A note without a valid pitch was dropped.
    DroppedNotes,
    /// A step's velocity, probability or ratchet, a row's note or a pattern's swing was out of range.
    StepValues,
    /// An id that two objects shared was given to the second one anew.
    DuplicateIds,
    /// A macro mapping was dropped, clamped or reordered.
    MacroMappings,
    /// A duplicate or unusable automation lane or clip envelope was dropped, or a missing lane added.
    AutomationLanes,
    /// Automation points were put in order, clamped or thinned, or a clip envelope was dropped.
    AutomationPoints,
    /// Clips or notes were put in the order the model keeps them in.
    Order,
    /// A routing reference (output, send, sidechain, audio input, group order) was dropped or fixed.
    Routing,
    /// Overlapping audio clips were cut.
    AudioOverlaps,
    /// `next_id` was below an id in use and was raised.
    NextId,
}

impl RepairKind {
    /// A short English description, for the log.
    pub fn describe(self) -> &'static str {
        match self {
            RepairKind::LoopRange => "loop range outside the document or empty",
            RepairKind::CountIn => "count-in longer than the maximum",
            RepairKind::MixerValues => "mixer, height or colour value out of range",
            RepairKind::DroppedDevices => "device that its chain cannot host dropped",
            RepairKind::ChainOrder => "device chain reordered",
            RepairKind::ClipValues => "clip start, length or setting out of range",
            RepairKind::ClipLoops => "loop length removed from a clip that cannot loop",
            RepairKind::NoteValues => "note start, length or velocity out of range",
            RepairKind::DroppedNotes => "note with a pitch above 127 dropped",
            RepairKind::StepValues => "step pattern value out of range",
            RepairKind::DuplicateIds => "duplicate object id replaced",
            RepairKind::MacroMappings => "macro mapping repaired or dropped",
            RepairKind::AutomationLanes => "duplicate automation lane dropped",
            RepairKind::AutomationPoints => "automation points reordered, clamped or thinned",
            RepairKind::Order => "clips or notes put in order",
            RepairKind::Routing => "routing reference dropped or fixed",
            RepairKind::AudioOverlaps => "overlapping audio clips cut",
            RepairKind::NextId => "next id raised above the ids in use",
        }
    }
}

/// What loading repaired: how many objects of each [`RepairKind`] it changed. Empty for a valid
/// file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Repairs(BTreeMap<RepairKind, usize>);

impl Repairs {
    /// Whether nothing was repaired.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of objects changed, over all kinds.
    pub fn total(&self) -> usize {
        self.0.values().sum()
    }

    /// The kinds and their counts, in a fixed order.
    pub fn iter(&self) -> impl Iterator<Item = (RepairKind, usize)> + '_ {
        self.0.iter().map(|(&kind, &count)| (kind, count))
    }

    /// How many objects of `kind` were changed.
    pub fn count(&self, kind: RepairKind) -> usize {
        self.0.get(&kind).copied().unwrap_or(0)
    }

    /// Counts `count` more objects of `kind` (nothing for 0).
    pub(crate) fn add(&mut self, kind: RepairKind, count: usize) {
        if count > 0 {
            *self.0.entry(kind).or_default() += count;
        }
    }
}

impl fmt::Display for Repairs {
    /// "3 × note without a valid pitch dropped, 1 × duplicate object id replaced".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (kind, count)) in self.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{count} × {}", kind.describe())?;
        }
        Ok(())
    }
}

thread_local! {
    /// Where `Points`' deserialisation, which has no access to the project being loaded, reports
    /// its repairs. `Some` only while [`collect_deserialize_repairs`] runs.
    static SINK: RefCell<Option<Repairs>> = const { RefCell::new(None) };
}

/// Notes a repair made while deserialising (`Points::deserialize`); ignored outside
/// [`collect_deserialize_repairs`].
pub(crate) fn note_deserialize_repair(kind: RepairKind) {
    SINK.with(|sink| {
        if let Some(repairs) = sink.borrow_mut().as_mut() {
            repairs.add(kind, 1);
        }
    });
}

/// Runs `f` and returns its result with the repairs deserialisation made inside it. Does not
/// nest.
pub(crate) fn collect_deserialize_repairs<T>(f: impl FnOnce() -> T) -> (T, Repairs) {
    /// Clears the sink even when `f` panics.
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SINK.with(|sink| *sink.borrow_mut() = None);
        }
    }
    SINK.with(|sink| *sink.borrow_mut() = Some(Repairs::default()));
    let reset = Reset;
    let value = f();
    let repairs = SINK
        .with(|sink| sink.borrow_mut().take())
        .unwrap_or_default();
    drop(reset);
    (value, repairs)
}

// ------------------------------------------------------------------------------------------------
// Validation
// ------------------------------------------------------------------------------------------------

/// The first invariant a project breaks ([`Project::validate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(String);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

impl Project {
    /// Checks every invariant of the document model: the ranges of all values, the order of clips,
    /// notes and device chains, unique ids below [`Project::next_id`], what each kind of track may
    /// hold, the routing graph (see [`routing::check`]) and [`MAX_DOC_TICK`]. The edits keep a valid
    /// project valid; loading repairs a file until this passes ([`crate::persist::from_json`]).
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate(self).map_err(ValidationError)
    }
}

fn in_doc(t: Tick) -> bool {
    (Tick::ZERO..=MAX_DOC_TICK).contains(&t)
}

fn rank(role: DeviceRole) -> u8 {
    match role {
        DeviceRole::MidiEffect => 0,
        DeviceRole::Instrument => 1,
        DeviceRole::Effect => 2,
    }
}

fn validate(p: &Project) -> Result<(), String> {
    macro_rules! ensure {
        ($cond:expr, $($msg:tt)+) => {
            if !$cond {
                return Err(format!($($msg)+));
            }
        };
    }
    let mut ids = HashSet::new();
    let mut id = |what: &str, raw: u64| -> Result<(), String> {
        if raw >= p.next_id || p.next_id == u64::MAX {
            return Err(format!("{what} id {raw} not below next_id {}", p.next_id));
        }
        if !ids.insert(raw) {
            return Err(format!("{what} id {raw} used twice"));
        }
        Ok(())
    };
    ensure!(
        valid_meter(p.meter),
        "meter {}/{} not supported",
        p.meter.num,
        p.meter.den
    );
    ensure!(
        (Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&p.tempo.bpm),
        "tempo {} out of range",
        p.tempo.bpm
    );
    ensure!(
        p.count_in_bars <= MAX_COUNT_IN_BARS,
        "count-in out of range"
    );
    routing::check(p).map_err(|issue| format!("routing: {issue}"))?;
    ensure!(
        !p.loop_range.is_empty() && in_doc(p.loop_range.start) && in_doc(p.loop_range.end),
        "loop range out of bounds"
    );
    validate_channel(&p.master, "master")?;
    for d in &p.master.devices {
        id("device", d.id.0)?;
        ensure!(d.role == DeviceRole::Effect, "master takes effects only");
    }
    let mut targets = HashSet::new();
    for lane in &p.master_automation.lanes {
        id("lane", lane.id.0)?;
        validate_lane(lane)?;
        ensure!(
            targets.insert(&lane.target),
            "master: two lanes for one target"
        );
    }
    for t in &p.tracks {
        id("track", t.id.0)?;
        ensure!(
            Hue::ALL.contains(&t.hue),
            "track {}: hue is not a token hue",
            t.id.0
        );
        ensure!(
            (TRACK_HEIGHT_MIN..=TRACK_HEIGHT_MAX).contains(&t.height),
            "track {}: height out of range",
            t.id.0
        );
        validate_channel(&t.channel, "track")?;
        for d in &t.channel.devices {
            id("device", d.id.0)?;
        }
        ensure!(
            t.channel.devices.is_sorted_by_key(|d| rank(d.role)),
            "track {}: chain order must be MIDI effects, instrument, effects",
            t.id.0
        );
        ensure!(
            t.channel
                .devices
                .iter()
                .filter(|d| d.role == DeviceRole::Instrument)
                .count()
                <= 1,
            "track {}: more than one instrument",
            t.id.0
        );
        ensure!(
            !t.is_bus() || t.sends.is_empty(),
            "track {}: a bus has no sends",
            t.id.0
        );
        for send in &t.sends {
            id("send", send.id.0)?;
            ensure!(
                (VOLUME_DB_MIN..=VOLUME_DB_MAX).contains(&send.level_db),
                "send {}: level out of range",
                send.id.0
            );
        }
        let mut targets = HashSet::new();
        for lane in &t.automation.lanes {
            id("lane", lane.id.0)?;
            validate_lane(lane)?;
            ensure!(
                targets.insert(&lane.target),
                "track {}: two lanes for one target",
                t.id.0
            );
        }
        ensure!(t.macros.is_valid(), "track {}: macro out of range", t.id.0);
        ensure!(
            t.clips.is_sorted_by_key(|c| (c.start, c.id)),
            "track {}: clips not sorted",
            t.id.0
        );
        if t.is_audio() {
            ensure!(
                t.clips.windows(2).all(|w| w[0].end() <= w[1].start),
                "track {}: audio clips overlap",
                t.id.0
            );
        }
        for clip in &t.clips {
            id("clip", clip.id.0)?;
            validate_clip(p, t, clip, &mut id)?;
        }
    }
    Ok(())
}

fn validate_channel(c: &Channel, what: &str) -> Result<(), String> {
    if !(VOLUME_DB_MIN..=VOLUME_DB_MAX).contains(&c.volume_db) {
        return Err(format!("{what}: volume out of range"));
    }
    if !(-1.0..=1.0).contains(&c.pan) {
        return Err(format!("{what}: pan out of range"));
    }
    Ok(())
}

fn validate_lane(lane: &crate::automation::Lane) -> Result<(), String> {
    if !(LANE_HEIGHT_MIN..=LANE_HEIGHT_MAX).contains(&lane.height) {
        return Err(format!("lane {}: height out of range", lane.id.0));
    }
    let bound = (-MAX_DOC_TICK)..=MAX_DOC_TICK;
    if !lane
        .points
        .as_slice()
        .iter()
        .all(|p| bound.contains(&p.tick))
    {
        return Err(format!("lane {}: point out of bounds", lane.id.0));
    }
    Ok(())
}

fn validate_clip(
    p: &Project,
    t: &Track,
    clip: &Clip,
    id: &mut impl FnMut(&str, u64) -> Result<(), String>,
) -> Result<(), String> {
    let name = clip.id.0;
    let fail = |what: &str| Err(format!("clip {name}: {what}"));
    if !in_doc(clip.start) || !(Tick(1)..=MAX_DOC_TICK).contains(&clip.length) {
        return fail("start or length out of range");
    }
    if !(-48..=48).contains(&clip.transpose) || !(-100..=100).contains(&clip.velocity_pct) {
        return fail("transposition or velocity change out of range");
    }
    if !clip.kind().fits(t.kind) {
        return fail("kind does not fit the track");
    }
    if matches!(clip.content, ClipContent::Audio(_)) && !clip.automation.is_empty() {
        return fail("an audio clip has no envelopes");
    }
    let mut targets = HashSet::new();
    for envelope in &clip.automation {
        if !targets.insert(&envelope.target) {
            return fail("two envelopes for one target");
        }
        let bound = (-MAX_DOC_TICK)..=MAX_DOC_TICK;
        if !envelope
            .points
            .as_slice()
            .iter()
            .all(|q| bound.contains(&q.tick))
        {
            return fail("automation point out of bounds");
        }
    }
    match &clip.content {
        ClipContent::Midi(m) => {
            if m.takes.is_empty() || m.active_take >= m.takes.len() {
                return fail("no active take");
            }
            if clip
                .loop_length
                .is_some_and(|l| !(Tick(1)..=MAX_DOC_TICK).contains(&l))
            {
                return fail("loop length out of range");
            }
            for take in &m.takes {
                if !take.notes.is_sorted_by_key(|n| (n.start, n.pitch, n.id)) {
                    return fail("notes not sorted");
                }
                for n in take.notes.iter() {
                    id("note", n.id.0)?;
                    if !note_ok(n) {
                        return Err(format!("note {}: value out of range", n.id.0));
                    }
                }
            }
        }
        ClipContent::Step(s) => {
            if clip.loop_length.is_some() {
                return fail("a step clip never loops");
            }
            if !(1..=MAX_STEPS).contains(&usize::from(s.length_steps)) {
                return fail("pattern length out of range");
            }
            if !(0.0..=MAX_SWING).contains(&s.swing) {
                return fail("swing out of range");
            }
            for row in &s.rows {
                if row.note > 127 {
                    return fail("row note out of range");
                }
                for step in &row.steps {
                    if !(1..=127).contains(&step.velocity)
                        || step.probability > 100
                        || !(1..=4).contains(&step.ratchet)
                    {
                        return fail("step value out of range");
                    }
                }
            }
        }
        ClipContent::Audio(a) => {
            if clip.loop_length.is_some() {
                return fail("an audio clip never loops");
            }
            if a.takes.is_empty() || a.active_take >= a.takes.len() {
                return fail("invalid audio take index");
            }
            if a.offset >= a.file.frames {
                return fail("offset behind the end of the file");
            }
            if clip.length > a.max_length(p.tempo).max(Tick(1)) {
                return fail("longer than the audio file");
            }
            if let Err(error) = a.validate_warp(clip.length) {
                return Err(format!("clip {name}: {error}"));
            }
            if a.fade_in < Tick::ZERO
                || a.fade_out < Tick::ZERO
                || a.fade_in + a.fade_out > clip.length
            {
                return fail("fades out of range");
            }
            if !(CLIP_GAIN_DB_MIN..=CLIP_GAIN_DB_MAX).contains(&a.gain_db) {
                return fail("gain out of range");
            }
        }
    }
    Ok(())
}

fn note_ok(n: &Note) -> bool {
    in_doc(n.start)
        && (Tick(1)..=MAX_DOC_TICK).contains(&n.length)
        && n.pitch <= 127
        && (1..=127).contains(&n.velocity)
}

// ------------------------------------------------------------------------------------------------
// Repair
// ------------------------------------------------------------------------------------------------

/// `v` clamped into `lo ..= hi`; NaN becomes `default`.
fn clamp_f32(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_nan() { default } else { v.clamp(lo, hi) }
}

/// The token hue closest to `hue` on the colour circle.
fn nearest_hue(hue: Hue) -> Hue {
    let distance = |h: &Hue| {
        let d = (i32::from(h.0) - i32::from(hue.0)).rem_euclid(360);
        d.min(360 - d)
    };
    Hue::ALL
        .into_iter()
        .min_by_key(distance)
        .unwrap_or(Hue::GREEN)
}

/// Makes a freshly deserialised project satisfy [`Project::validate`], or says why it cannot (the
/// message goes to `PersistError::Malformed`). See the module docs for the policy.
pub(crate) fn repair_loaded(project: &mut Project) -> Result<Repairs, String> {
    let mut repairs = Repairs::default();
    if !valid_meter(project.meter) {
        let meter = project.meter;
        return Err(format!("meter {}/{} not supported", meter.num, meter.den));
    }
    let bpm = project.tempo.bpm;
    if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&bpm) {
        return Err(format!("tempo {bpm} out of range"));
    }
    repair_settings(project, &mut repairs);
    let tempo = project.tempo;
    repair_channel(Arc::make_mut(&mut project.master), &mut repairs);
    let master = Arc::make_mut(&mut project.master);
    let before = master.devices.len();
    master.devices.retain(|d| d.role == DeviceRole::Effect);
    repairs.add(RepairKind::DroppedDevices, before - master.devices.len());
    for track in &mut project.tracks {
        repair_track(track, tempo, &mut repairs)?;
    }
    repair_ids(project, &mut repairs)?;
    for track in &mut project.tracks {
        sort_track(track, &mut repairs);
    }
    let order: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
    let routed = repair_routing(project) + repair_audio_inputs(project);
    repairs.add(
        RepairKind::Routing,
        routed + usize::from(order != project.tracks.iter().map(|t| t.id).collect::<Vec<_>>()),
    );
    crate::edits::automation::repair_loaded(project, &mut repairs)
        .map_err(|error| error.to_string())?;
    let audio_before: Vec<Vec<Arc<Clip>>> = project
        .tracks
        .iter()
        .map(|t| {
            if t.is_audio() {
                t.clips.clone()
            } else {
                Vec::new()
            }
        })
        .collect();
    crate::edits::normalize_audio_overlaps(project).map_err(|error| error.to_string())?;
    let cut = project
        .tracks
        .iter()
        .zip(&audio_before)
        .filter(|(t, before)| {
            t.is_audio()
                && (t.clips.len() != before.len()
                    || t.clips.iter().zip(before.iter()).any(|(a, b)| a != b))
        })
        .count();
    repairs.add(RepairKind::AudioOverlaps, cut);
    routing::check(project).map_err(|issue| issue.to_string())?;
    project.validate().map_err(|error| error.to_string())?;
    Ok(repairs)
}

fn repair_settings(project: &mut Project, repairs: &mut Repairs) {
    if project.count_in_bars > MAX_COUNT_IN_BARS {
        project.count_in_bars = MAX_COUNT_IN_BARS;
        repairs.add(RepairKind::CountIn, 1);
    }
    let range = project.loop_range;
    let start = range.start.clamp(Tick::ZERO, MAX_DOC_TICK);
    let end = range.end.clamp(Tick::ZERO, MAX_DOC_TICK);
    if (start, end) != (range.start, range.end) || end <= start {
        project.loop_range = if end > start {
            TickRange::new(start, end)
        } else {
            TickRange::new(Tick::ZERO, Tick::from_steps(64))
        };
        repairs.add(RepairKind::LoopRange, 1);
    }
}

fn repair_channel(channel: &mut Channel, repairs: &mut Repairs) {
    let volume = clamp_f32(channel.volume_db, VOLUME_DB_MIN, VOLUME_DB_MAX, 0.0);
    let pan = clamp_f32(channel.pan, -1.0, 1.0, 0.0);
    if volume != channel.volume_db || pan != channel.pan {
        channel.volume_db = volume;
        channel.pan = pan;
        repairs.add(RepairKind::MixerValues, 1);
    }
}

fn repair_track(track: &mut Arc<Track>, tempo: Tempo, repairs: &mut Repairs) -> Result<(), String> {
    let t = Arc::make_mut(track);
    if !t.macros.is_default() {
        t.macros.repair(repairs);
    }
    if !t.is_audio() && !t.audio.is_default() {
        t.audio = crate::AudioIo::default();
        repairs.add(RepairKind::MixerValues, 1);
    }
    if t.is_audio() && t.monitor {
        t.monitor = false;
        repairs.add(RepairKind::MixerValues, 1);
    }
    if !Hue::ALL.contains(&t.hue) {
        t.hue = nearest_hue(t.hue);
        repairs.add(RepairKind::MixerValues, 1);
    }
    let height = t.height.clamp(TRACK_HEIGHT_MIN, TRACK_HEIGHT_MAX);
    if height != t.height {
        t.height = height;
        repairs.add(RepairKind::MixerValues, 1);
    }
    repair_channel(&mut t.channel, repairs);
    if !t.channel.devices.is_sorted_by_key(|d| rank(d.role)) {
        t.channel.devices.sort_by_key(|d| rank(d.role));
        repairs.add(RepairKind::ChainOrder, 1);
    }
    if t.kind == TrackKind::Midi {
        // One instrument plays; the chain has room for no other.
        let mut seen = false;
        let before = t.channel.devices.len();
        t.channel.devices.retain(|d| {
            let extra = d.role == DeviceRole::Instrument && std::mem::replace(&mut seen, true);
            !extra
        });
        repairs.add(RepairKind::DroppedDevices, before - t.channel.devices.len());
    }
    if t.is_bus() && !t.sends.is_empty() {
        repairs.add(RepairKind::MixerValues, t.sends.len());
        t.sends.clear();
    }
    let mut fixed = 0;
    for send in &mut t.sends {
        let level = clamp_f32(send.level_db, VOLUME_DB_MIN, VOLUME_DB_MAX, 0.0);
        fixed += usize::from(level != send.level_db);
        send.level_db = level;
    }
    repairs.add(RepairKind::MixerValues, fixed);
    for clip in &mut t.clips {
        repair_clip(clip, tempo, repairs)?;
    }
    Ok(())
}

fn repair_clip(clip: &mut Arc<Clip>, tempo: Tempo, repairs: &mut Repairs) -> Result<(), String> {
    let c = Arc::make_mut(clip);
    let id = c.id.0;
    // A warped clip maps its whole span to the file: no length is no clip, not a clip of one tick.
    if matches!(&c.content, ClipContent::Audio(a) if a.warp_enabled()) && c.length <= Tick::ZERO {
        return Err(format!("clip {id}: empty audio warp span"));
    }
    let (start, length) = (
        c.start.clamp(Tick::ZERO, MAX_DOC_TICK),
        c.length.clamp(Tick(1), MAX_DOC_TICK),
    );
    let (transpose, velocity) = (c.transpose.clamp(-48, 48), c.velocity_pct.clamp(-100, 100));
    if (start, length, transpose, velocity) != (c.start, c.length, c.transpose, c.velocity_pct) {
        repairs.add(RepairKind::ClipValues, 1);
        (c.start, c.length, c.transpose, c.velocity_pct) = (start, length, transpose, velocity);
    }
    let loops = matches!(c.content, ClipContent::Midi(_));
    if !loops && c.loop_length.take().is_some() {
        repairs.add(RepairKind::ClipLoops, 1);
    }
    if let Some(l) = c.loop_length {
        let fixed = l.clamp(Tick(1), MAX_DOC_TICK);
        if fixed != l {
            c.loop_length = Some(fixed);
            repairs.add(RepairKind::ClipValues, 1);
        }
    }
    match &mut c.content {
        ClipContent::Midi(m) => {
            if m.takes.is_empty() || m.active_take >= m.takes.len() {
                return Err(format!("clip {id}: no active take"));
            }
            for take in &mut m.takes {
                repair_notes(&mut take.notes, repairs);
            }
        }
        ClipContent::Step(s) => {
            if !(1..=MAX_STEPS).contains(&usize::from(s.length_steps)) {
                return Err(format!("clip {id}: pattern length out of range"));
            }
            repair_step_clip(s, repairs);
        }
        ClipContent::Audio(a) => {
            // Validate structure before dereferencing the active take; malformed files
            // must return an error, never panic through field compatibility access.
            if a.takes.is_empty() || a.active_take >= a.takes.len() {
                return Err(format!("clip {id}: invalid audio take index"));
            }
            for take in &a.takes {
                let file = &take.file;
                if file.frames == 0 || file.sample_rate == 0 || file.channels == 0 {
                    return Err(format!("clip {id}: audio file without frames"));
                }
            }
            if a.offset >= a.file.frames {
                return Err(format!("clip {id}: offset behind the file's end"));
            }
            let bpm = a.original_tempo.bpm;
            if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&bpm) {
                return Err(format!("clip {id}: original tempo out of range"));
            }
            a.validate_warp(c.length)
                .map_err(|e| format!("clip {id}: {e}"))?;
            // Repair what a hand edit can break: a window longer than the file, fades
            // longer than the clip, a gain out of range.
            let length = c.length.min(a.max_length(tempo).max(Tick(1)));
            let mut fixed = a.clone();
            fixed.clamp(length);
            if length != c.length || fixed != *a {
                c.length = length;
                c.content = ClipContent::Audio(fixed);
                repairs.add(RepairKind::ClipValues, 1);
            }
        }
    }
    Ok(())
}

fn repair_notes(notes: &mut Arc<Vec<Note>>, repairs: &mut Repairs) {
    if notes.iter().all(note_ok) {
        return;
    }
    let list = Arc::make_mut(notes);
    let before = list.len();
    list.retain(|n| n.pitch <= 127);
    repairs.add(RepairKind::DroppedNotes, before - list.len());
    let mut fixed = 0;
    for n in list.iter_mut() {
        let (start, length, velocity) = (
            n.start.clamp(Tick::ZERO, MAX_DOC_TICK),
            n.length.clamp(Tick(1), MAX_DOC_TICK),
            n.velocity.clamp(1, 127),
        );
        if (start, length, velocity) != (n.start, n.length, n.velocity) {
            fixed += 1;
            (n.start, n.length, n.velocity) = (start, length, velocity);
        }
    }
    repairs.add(RepairKind::NoteValues, fixed);
}

fn repair_step_clip(s: &mut StepClip, repairs: &mut Repairs) {
    let mut fixed = 0;
    let swing = clamp_f32(s.swing, 0.0, MAX_SWING, 0.0);
    fixed += usize::from(swing != s.swing);
    s.swing = swing;
    for row in &mut s.rows {
        fixed += usize::from(row.note > 127);
        row.note = row.note.min(127);
        for step in &mut row.steps {
            let (velocity, probability, ratchet) = (
                step.velocity.clamp(1, 127),
                step.probability.min(100),
                step.ratchet.clamp(1, 4),
            );
            if (velocity, probability, ratchet) != (step.velocity, step.probability, step.ratchet) {
                fixed += 1;
                (step.velocity, step.probability, step.ratchet) = (velocity, probability, ratchet);
            }
        }
    }
    repairs.add(RepairKind::StepValues, fixed);
}

/// What an id names, for [`visit_ids`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdKind {
    Track,
    Device,
    Send,
    Other,
}

/// Who holds an object: the master or the track at an index.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Master,
    Track(usize),
}

/// A new id for an object, and whether automation of its owner that targeted the old id follows it.
struct Replace {
    new: u64,
    retarget: bool,
}

/// Points the `Param` and `Send` targets of `kind` `old` to `new`.
fn retarget(target: &mut AutomationTarget, kind: IdKind, old: u64, new: u64) {
    match target {
        AutomationTarget::Param { device, .. } if kind == IdKind::Device && device.0 == old => {
            device.0 = new;
        }
        AutomationTarget::Send { send } if kind == IdKind::Send && send.0 == old => send.0 = new,
        _ => {}
    }
}

/// Calls `f` with the kind, the owner and the id of every object that has one: tracks first
/// (references point to them), then per track its devices, sends, lanes, clips and notes, then the
/// master's devices and lanes. A device or send that gets a new id takes the automation targets
/// and macro mappings of its own owner along when `f` says so.
fn visit_ids(project: &mut Project, f: &mut impl FnMut(IdKind, Owner, u64) -> Option<Replace>) {
    fn apply(
        kind: IdKind,
        owner: Owner,
        id: &mut u64,
        f: &mut impl FnMut(IdKind, Owner, u64) -> Option<Replace>,
        moved: &mut Vec<(IdKind, u64, u64)>,
    ) {
        if let Some(r) = f(kind, owner, *id) {
            if r.retarget {
                moved.push((kind, *id, r.new));
            }
            *id = r.new;
        }
    }
    for (i, track) in project.tracks.iter_mut().enumerate() {
        apply(
            IdKind::Track,
            Owner::Track(i),
            &mut Arc::make_mut(track).id.0,
            f,
            &mut Vec::new(),
        );
    }
    for (i, track) in project.tracks.iter_mut().enumerate() {
        let owner = Owner::Track(i);
        let t = Arc::make_mut(track);
        let mut moved = Vec::new();
        for device in &mut t.channel.devices {
            apply(
                IdKind::Device,
                owner,
                &mut Arc::make_mut(device).id.0,
                f,
                &mut moved,
            );
        }
        for send in &mut t.sends {
            apply(IdKind::Send, owner, &mut send.id.0, f, &mut moved);
        }
        for lane in &mut t.automation.lanes {
            apply(
                IdKind::Other,
                owner,
                &mut Arc::make_mut(lane).id.0,
                f,
                &mut moved,
            );
        }
        for clip in &mut t.clips {
            let c = Arc::make_mut(clip);
            apply(IdKind::Other, owner, &mut c.id.0, f, &mut moved);
            if let ClipContent::Midi(m) = &mut c.content {
                for take in &mut m.takes {
                    for note in Arc::make_mut(&mut take.notes).iter_mut() {
                        apply(IdKind::Other, owner, &mut note.id.0, f, &mut moved);
                    }
                }
            }
        }
        for (kind, old, new) in moved {
            for lane in &mut t.automation.lanes {
                retarget(&mut Arc::make_mut(lane).target, kind, old, new);
            }
            for clip in &mut t.clips {
                for envelope in &mut Arc::make_mut(clip).automation {
                    retarget(&mut envelope.target, kind, old, new);
                }
            }
            for slot in &mut t.macros.0 {
                for mapping in &mut slot.mappings {
                    retarget(&mut mapping.target, kind, old, new);
                }
            }
        }
    }
    let mut moved = Vec::new();
    for device in &mut Arc::make_mut(&mut project.master).devices {
        apply(
            IdKind::Device,
            Owner::Master,
            &mut Arc::make_mut(device).id.0,
            f,
            &mut moved,
        );
    }
    for lane in &mut Arc::make_mut(&mut project.master_automation).lanes {
        apply(
            IdKind::Other,
            Owner::Master,
            &mut Arc::make_mut(lane).id.0,
            f,
            &mut moved,
        );
    }
    for (kind, old, new) in moved {
        for lane in &mut Arc::make_mut(&mut project.master_automation).lanes {
            retarget(&mut Arc::make_mut(lane).target, kind, old, new);
        }
    }
}

/// Every id the project refers to without owning it: routing targets, sidechain sources, input
/// buses and the devices and sends that automation and macros address. A replacement id must not
/// be one of them, or a reference to something that is missing would find a new object.
fn referenced_ids(project: &Project) -> HashSet<u64> {
    let mut ids = HashSet::new();
    let mut target = |t: &AutomationTarget| match t {
        AutomationTarget::Param { device, .. } => {
            ids.insert(device.0);
        }
        AutomationTarget::Send { send } => {
            ids.insert(send.0);
        }
        _ => {}
    };
    for lane in &project.master_automation.lanes {
        target(&lane.target);
    }
    for t in &project.tracks {
        t.automation
            .lanes
            .iter()
            .for_each(|lane| target(&lane.target));
        t.clips
            .iter()
            .flat_map(|c| &c.automation)
            .for_each(|e| target(&e.target));
        t.macros
            .0
            .iter()
            .flat_map(|slot| &slot.mappings)
            .for_each(|m| target(&m.target));
    }
    for t in &project.tracks {
        if let crate::project::TrackOutput::Bus(b) = t.output {
            ids.insert(b.0);
        }
        ids.extend(t.sends.iter().map(|s| s.target.0));
        ids.extend(
            t.channel
                .devices
                .iter()
                .filter_map(|d| d.sidechain)
                .map(|s| s.source.0),
        );
        ids.extend(t.input_bus().map(|b| b.0));
    }
    ids.extend(
        project
            .master
            .devices
            .iter()
            .filter_map(|d| d.sidechain)
            .map(|s| s.source.0),
    );
    ids
}

/// Raises `next_id` above every id, then gives every id that is used twice a new one. The first
/// object in [`visit_ids`] order keeps it, so references (routing, sidechains, automation targets)
/// still name the object they named before, which is also what the routing graph and `find_*`
/// resolve to (tracks before the master). Devices and sends that lose their id keep the automation
/// targets and macro mappings of their own track or master. Replacement ids skip the ids that
/// [`referenced_ids`] names. Rejects a project whose ids are used up.
fn repair_ids(project: &mut Project, repairs: &mut Repairs) -> Result<(), String> {
    const EXHAUSTED: &str = "project object identifiers exhausted";
    let mut max = 0;
    visit_ids(project, &mut |_, _, id| {
        max = max.max(id);
        None
    });
    let needed = max.checked_add(1).ok_or(EXHAUSTED)?;
    if project.next_id < needed {
        repairs.add(RepairKind::NextId, 1);
    }
    let mut next = project.next_id.max(needed);
    if next == u64::MAX {
        return Err(EXHAUSTED.to_owned());
    }
    let reserved = referenced_ids(project);
    let mut first: HashMap<u64, (IdKind, Owner)> = HashMap::new();
    let mut replaced = 0;
    let mut exhausted = false;
    visit_ids(project, &mut |kind, owner, id| {
        let Some(&(first_kind, first_owner)) = first.get(&id) else {
            first.insert(id, (kind, owner));
            return None;
        };
        while first.contains_key(&next) || reserved.contains(&next) {
            match next.checked_add(1) {
                Some(n) => next = n,
                None => {
                    exhausted = true;
                    break;
                }
            }
        }
        let new = next;
        first.insert(new, (kind, owner));
        replaced += 1;
        match next.checked_add(1).filter(|&n| n < u64::MAX) {
            Some(n) => next = n,
            None => exhausted = true,
        }
        // The first holder is another object than the one automation of this owner means.
        let retarget = !(first_kind == kind && first_owner == owner);
        Some(Replace { new, retarget })
    });
    if exhausted {
        return Err(EXHAUSTED.to_owned());
    }
    project.next_id = next;
    repairs.add(RepairKind::DuplicateIds, replaced);
    Ok(())
}

/// Puts clips and notes in the order the model keeps them in.
fn sort_track(track: &mut Arc<Track>, repairs: &mut Repairs) {
    let t = Arc::make_mut(track);
    if !t.clips.is_sorted_by_key(|c| (c.start, c.id)) {
        t.clips.sort_by_key(|c| (c.start, c.id));
        repairs.add(RepairKind::Order, 1);
    }
    for clip in &mut t.clips {
        let ClipContent::Midi(m) = &clip.content else {
            continue;
        };
        let sorted = m
            .takes
            .iter()
            .all(|take| take.notes.is_sorted_by_key(|n| (n.start, n.pitch, n.id)));
        if sorted {
            continue;
        }
        let ClipContent::Midi(m) = &mut Arc::make_mut(clip).content else {
            continue;
        };
        for take in &mut m.takes {
            if !take.notes.is_sorted_by_key(|n| (n.start, n.pitch, n.id)) {
                Arc::make_mut(&mut take.notes).sort_by_key(|n| (n.start, n.pitch, n.id));
                repairs.add(RepairKind::Order, 1);
            }
        }
    }
}

/// Clears the input bus of an audio track that names a missing track, the track itself or a track
/// that is not a bus. A cycle through an input stays for [`routing::check`] to reject.
fn repair_audio_inputs(project: &mut Project) -> usize {
    let mut cleared = 0;
    let buses: HashSet<_> = project
        .tracks
        .iter()
        .filter(|t| t.is_bus())
        .map(|t| t.id)
        .collect();
    for track in &mut project.tracks {
        if track.input_bus().is_some_and(|b| !buses.contains(&b)) {
            Arc::make_mut(track).audio.input = crate::AudioInput::None;
            cleared += 1;
        }
    }
    cleared
}

/// Drops routing references that cannot work: outputs and sends that name a missing track, the
/// track itself or a track that is not a bus (outputs then go to the master), second sends to the
/// same bus, sidechains from a missing track or from the device's own track, `folder` on a track
/// that is not a bus, `folded` on one that is not a group, arm and monitoring on a bus. Then puts
/// every group's members behind it. Leaves cycles and bus content to [`routing::check`], which
/// rejects them. Returns how many tracks and master devices it changed.
fn repair_routing(project: &mut Project) -> usize {
    let mut changed = 0;
    use crate::project::TrackOutput;

    let kinds: HashMap<TrackId, bool> = project.tracks.iter().map(|t| (t.id, t.is_bus())).collect();
    let is_bus = |own, id| id != own && kinds.get(&id) == Some(&true);
    let bad_sidechain = |own: Option<_>, sc: Option<crate::Sidechain>| {
        sc.is_some_and(|sc| Some(sc.source) == own || !kinds.contains_key(&sc.source))
    };
    for index in 0..project.tracks.len() {
        let t = &project.tracks[index];
        let own = t.id;
        let mut targets = HashSet::new();
        let broken = matches!(t.output, TrackOutput::Bus(b) if !is_bus(own, b))
            || !t
                .sends
                .iter()
                .all(|s| is_bus(own, s.target) && targets.insert(s.target))
            || (t.folder && !t.is_bus())
            || (t.folded && !t.is_group())
            || (t.is_bus() && (t.arm || t.monitor))
            || t.channel
                .devices
                .iter()
                .any(|d| bad_sidechain(Some(own), d.sidechain));
        if !broken {
            continue;
        }
        changed += 1;
        let t = Arc::make_mut(&mut project.tracks[index]);
        if matches!(t.output, TrackOutput::Bus(b) if !is_bus(own, b)) {
            t.output = TrackOutput::Master;
        }
        let mut targets = HashSet::new();
        t.sends
            .retain(|s| is_bus(own, s.target) && targets.insert(s.target));
        t.folder &= t.is_bus();
        t.folded &= t.folder;
        if t.is_bus() {
            t.arm = false;
            t.monitor = false;
        }
        for d in &mut t.channel.devices {
            if bad_sidechain(Some(own), d.sidechain) {
                Arc::make_mut(d).sidechain = None;
            }
        }
    }
    if project
        .master
        .devices
        .iter()
        .any(|d| bad_sidechain(None, d.sidechain))
    {
        for d in &mut Arc::make_mut(&mut project.master).devices {
            if bad_sidechain(None, d.sidechain) {
                Arc::make_mut(d).sidechain = None;
                changed += 1;
            }
        }
    }
    routing::normalize_groups(project);
    changed
}
