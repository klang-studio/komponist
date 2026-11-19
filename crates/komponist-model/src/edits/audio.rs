//! Audio edits (epic #8, ARCHITECTURE §6.9): audio tracks, audio clip properties, trimming, the
//! overlap-cut rule, tempo changes and file links.
//!
//! The rule that shapes most of this module: **audio clips on one track never overlap**. Whatever
//! puts a clip on a track (add, move, copy, duplicate, stretch, trim, inspector start) cuts the
//! clips below it ([`cut_below`]): a covered clip disappears, a clip hit on one side is shortened,
//! a clip with the new one in the middle is split in two (the right part gets a new id). Every
//! piece keeps its window into the file, so nothing is lost but the hidden part, and the whole is
//! one undo step. Fades of an edge a cut creates are reset (the playback puts
//! [`crate::EDGE_FADE_MS`] on it).

use std::sync::Arc;

use super::{
    DefaultNames, EditError, EditResult, MIN_LENGTH, edit_clip, edit_clip_with, track_mut,
    valid_hue, valid_name,
};
use crate::audio::ClipTimeMode;
use crate::audio::{
    AudioClip, AudioFile, AudioInput, AudioIo, AudioTake, CLIP_GAIN_DB_MAX, CLIP_GAIN_DB_MIN,
};
use crate::automation::{
    AUTOMATION_EXPANSION_LIMIT, AutomationTarget, Points, clip_expansion_points,
    count_clip_expansion, merged_clip_points,
};
use crate::ids::{ClipId, TrackId};
use crate::params::db_to_p;
use crate::project::{
    Channel, Clip, ClipContent, ClipKind, Hue, MidiChannelSel, MidiInputSel, Project,
    TRACK_HEIGHT_DEFAULT, Track, TrackKind, TrackOutput,
};
use crate::time::{Tempo, Tick};

/// What a new audio track gets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewAudioTrack {
    /// Name; `None` = [`DefaultNames::audio`] made unique ("Audio", "Audio 2", …).
    pub name: Option<String>,
    /// Colour; `None` = [`super::next_hue`].
    pub hue: Option<Hue>,
    /// Display index to insert at; `None` = append at the end.
    pub index: Option<usize>,
    /// Where it records from; the default is no input.
    pub input: AudioInput,
}

/// Adds an audio track: height [`TRACK_HEIGHT_DEFAULT`], 0 dB, centred, no clips, no devices, not
/// armed, monitoring off. Inserted before a group member it joins that member's group, elsewhere it
/// outputs to the master (like [`super::add_track`]).
pub fn add_audio_track(
    project: &Project,
    spec: NewAudioTrack,
) -> Result<(Project, TrackId), EditError> {
    add_audio_track_in(project, spec, &DefaultNames::DE)
}

/// [`add_audio_track`] with the name base of `names` for a track without a name.
pub fn add_audio_track_in(
    project: &Project,
    spec: NewAudioTrack,
    names: &DefaultNames,
) -> Result<(Project, TrackId), EditError> {
    let hue = match spec.hue {
        Some(hue) => valid_hue(hue)?,
        None => super::next_hue(project),
    };
    let name = match &spec.name {
        Some(name) => valid_name(name)?,
        None => super::unique_track_name(project, names.audio),
    };
    let mut p = project.clone();
    let id = TrackId(p.alloc_id()?);
    let index = spec.index.unwrap_or(p.tracks.len()).min(p.tracks.len());
    let group = p
        .tracks
        .get(index)
        .and_then(|t| crate::routing::group_of(&p, t.id));
    p.tracks.insert(
        index,
        Arc::new(Track {
            id,
            name,
            hue,
            kind: TrackKind::Audio,
            channel: Channel::default(),
            arm: false,
            monitor: false,
            input: MidiInputSel::All,
            midi_channel: MidiChannelSel::Omni,
            audio: AudioIo {
                input: spec.input,
                ..AudioIo::default()
            },
            height: TRACK_HEIGHT_DEFAULT,
            clips: Vec::new(),
            output: group.map_or(TrackOutput::Master, TrackOutput::Bus),
            sends: Vec::new(),
            folder: false,
            folded: false,
            automation: Default::default(),
            macros: Default::default(),
        }),
    );
    crate::routing::normalize_groups(&mut p);
    super::routes::check_routing(&p)?;
    Ok((p, id))
}

/// A property of an audio clip (inspector, clip handles).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AudioClipProp {
    /// Clip gain in dB, clamped to `CLIP_GAIN_DB_MIN ..= CLIP_GAIN_DB_MAX`; NaN is `Invalid`.
    GainDb(f32),
    /// Active take source start in file frames; clip position stays, length may shrink.
    SourceOffset(u64),
    /// Fade-in length in ticks, clamped to `0 ..= length − fade_out`.
    FadeIn(Tick),
    /// Fade-out length in ticks, clamped to `0 ..= length − fade_in`.
    FadeOut(Tick),
    /// Re-Pitch on or off ([`ClipTimeMode`]). The timeline length stays; it is clamped to what
    /// the file still has under the new mapping.
    TimeMode(ClipTimeMode),
    /// The original tempo of the content, `Tempo::MIN_BPM ..= Tempo::MAX_BPM` (else `Invalid`).
    /// The length is clamped like for [`AudioClipProp::TimeMode`].
    OriginalTempo(Tempo),
}

/// Sets a property of an audio clip. `WrongClipKind { expected: Audio }` for other clips.
pub fn set_audio_clip_prop(project: &Project, clip: ClipId, prop: AudioClipProp) -> EditResult {
    let tempo = project.tempo;
    edit_clip(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        match prop {
            AudioClipProp::SourceOffset(offset) => {
                a.offset = offset.min(a.file.frames.saturating_sub(1));
            }
            AudioClipProp::GainDb(db) => {
                if db.is_nan() {
                    return Err(EditError::Invalid("gain is not a number"));
                }
                a.gain_db = db.clamp(CLIP_GAIN_DB_MIN, CLIP_GAIN_DB_MAX);
            }
            AudioClipProp::FadeIn(t) => a.fade_in = t.max(Tick::ZERO).min(length - a.fade_out),
            AudioClipProp::FadeOut(t) => a.fade_out = t.max(Tick::ZERO).min(length - a.fade_in),
            AudioClipProp::TimeMode(mode) => {
                a.time_mode = mode;
                if a.warp.is_some() {
                    a.warp = Some(crate::AudioWarp::new(false));
                }
            }
            AudioClipProp::OriginalTempo(t) => {
                if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&t.bpm) {
                    return Err(EditError::Invalid("tempo out of range"));
                }
                a.original_tempo = t;
                if a.warp_enabled() {
                    a.warp = Some(crate::AudioWarp::new(true));
                }
            }
        }
        if matches!(
            prop,
            AudioClipProp::TimeMode(_)
                | AudioClipProp::OriginalTempo(_)
                | AudioClipProp::SourceOffset(_)
        ) {
            let max = a.max_length(tempo).max(MIN_LENGTH);
            let new_length = length.min(max);
            a.clamp(new_length);
            a.validate_warp(new_length)?;
            c.length = new_length;
        }
        Ok(())
    })
}

/// Enables/disables pitch-preserving tempo-follow data without extending the legacy time-mode
/// enum. The current window/start stays; source-limited length/fades are clamped as for mode
/// changes. Existing Warp keeps its split/trim phase; a newly enabled mapping starts locally.
/// This pure model edit is shared by the UI; worker state follows through program compilation.
pub fn set_audio_clip_warp(project: &Project, clip: ClipId, enabled: bool) -> EditResult {
    let tempo = project.tempo;
    edit_clip(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        if enabled {
            if !a.warp_enabled() {
                a.warp = Some(crate::AudioWarp::new(true));
            }
            a.time_mode = ClipTimeMode::Fixed;
        } else if a.warp.is_some() {
            a.warp = Some(crate::AudioWarp::new(false));
        }
        let length = length.min(a.max_length(tempo).max(MIN_LENGTH));
        a.validate_warp(length)?;
        a.clamp(length);
        c.length = length;
        Ok(())
    })
}

/// Points an audio clip at another file (relinking a missing file, „Suchen …“). The window (offset,
/// length, fades) stays; it must still lie inside the new file: an offset behind its end is
/// `Invalid`, a longer clip is shortened to what the file has.
pub fn set_audio_clip_file(project: &Project, clip: ClipId, file: AudioFile) -> EditResult {
    check_file(&file)?;
    let tempo = project.tempo;
    edit_clip(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        if a.offset >= file.frames {
            return Err(EditError::Invalid(
                "the file is shorter than the clip's offset",
            ));
        }
        a.file = file;
        let new_length = length.min(a.max_length(tempo).max(MIN_LENGTH));
        a.validate_warp(new_length)?;
        a.clamp(new_length);
        c.length = new_length;
        Ok(())
    })
}

/// Left edge drag: moves the start of an audio clip to `start` while the content stays where it is
/// on the timeline (the window's offset moves with the edge). `start` is clamped to
/// `0 ..< end` and to what the file has before the window; the right edge stays. The clip then cuts
/// what lies below ([`cut_below`]).
pub fn trim_audio_clip_start(project: &Project, clip: ClipId, start: Tick) -> EditResult {
    let tempo = project.tempo;
    let mut p = edit_clip(project, clip, |c| {
        let end = c.end();
        let (old_start, old_length) = (c.start, c.length);
        let a = audio_mut(c)?;
        let back = if a.warp_enabled() {
            (0..a.takes.len())
                .map(|i| a.warp_map(i).map(crate::WarpMap::available_before))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .min()
                .unwrap_or(Tick::ZERO)
        } else {
            a.takes
                .iter()
                .map(|take| {
                    let secs = take.offset as f64 / f64::from(take.file.sample_rate.max(1));
                    Tick((secs / a.source_secs_per_tick(tempo) + 1e-6).floor() as i64)
                })
                .min()
                .unwrap_or(Tick::ZERO)
        };
        let start = start
            .max(Tick::ZERO)
            .max(old_start - back)
            .min(end - MIN_LENGTH);
        let delta = start - old_start;
        a.shift_sources(delta, tempo)?;
        let length = (old_length - delta).min(a.max_length(tempo).max(MIN_LENGTH));
        a.fade_in = Tick::ZERO.max(a.fade_in);
        a.clamp(length);
        c.start = start;
        c.length = length;
        Ok(())
    })?;
    cut_below(&mut p, &[clip])?;
    Ok(p)
}

/// Appends a recorded source window and selects it. Other takes and shared clip settings stay.
/// The clip length is shortened only when the new source has less material behind its offset.
pub fn add_audio_take(
    project: &Project,
    clip: ClipId,
    mut take: AudioTake,
) -> Result<(Project, usize), EditError> {
    take.name = valid_name(&take.name)?;
    check_file(&take.file)?;
    if take.offset >= take.file.frames {
        return Err(EditError::Invalid("the audio take is empty"));
    }
    let tempo = project.tempo;
    edit_clip_with(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        a.takes.push(take);
        let index = a.takes.len() - 1;
        select_source(a, index, length, tempo).map(|length| {
            c.length = length;
            index
        })
    })
}

/// Selects an existing audio take. An exhausted inactive window cannot be selected.
pub fn select_audio_take(project: &Project, clip: ClipId, take: usize) -> EditResult {
    let tempo = project.tempo;
    edit_clip(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        let length = select_source(a, take, length, tempo)?;
        c.length = length;
        Ok(())
    })
}

/// Renames one audio take without changing which one plays or the clip's own name.
pub fn rename_audio_take(project: &Project, clip: ClipId, take: usize, name: String) -> EditResult {
    let name = valid_name(&name)?;
    edit_clip(project, clip, |c| {
        let a = audio_mut(c)?;
        let take = a.takes.get_mut(take).ok_or(EditError::IndexOutOfRange {
            what: "take",
            index: take,
        })?;
        take.name = name;
        Ok(())
    })
}

/// Removes a take, keeping the active source when possible. The final take cannot be removed;
/// remove the clip instead. Removing the active take chooses the nearest remaining playable one.
pub fn remove_audio_take(project: &Project, clip: ClipId, take: usize) -> EditResult {
    let tempo = project.tempo;
    edit_clip(project, clip, |c| {
        let length = c.length;
        let a = audio_mut(c)?;
        if take >= a.takes.len() {
            return Err(EditError::IndexOutOfRange {
                what: "take",
                index: take,
            });
        }
        if a.takes.len() == 1 {
            return Err(EditError::Invalid("cannot remove the final audio take"));
        }
        let old_active = a.active_take;
        a.takes.remove(take);
        let active = if take != old_active {
            old_active - usize::from(take < old_active)
        } else {
            a.takes
                .iter()
                .enumerate()
                .filter(|(_, t)| t.offset < t.file.frames)
                .min_by_key(|(i, _)| i.abs_diff(take))
                .map(|(i, _)| i)
                .ok_or(EditError::Invalid("no playable audio take remains"))?
        };
        let length = select_source(a, active, length, tempo)?;
        c.length = length;
        Ok(())
    })
}

fn select_source(
    a: &mut AudioClip,
    index: usize,
    length: Tick,
    tempo: Tempo,
) -> Result<Tick, EditError> {
    let take = a.takes.get(index).ok_or(EditError::IndexOutOfRange {
        what: "take",
        index,
    })?;
    if take.offset >= take.file.frames {
        return Err(EditError::Invalid("the audio take is empty"));
    }
    a.active_take = index;
    let length = length.min(a.max_length(tempo));
    if length < MIN_LENGTH {
        return Err(EditError::Invalid("the audio take is empty"));
    }
    a.validate_warp(length)?;
    a.clamp(length);
    Ok(length)
}

/// The audio content of a clip or `WrongClipKind { expected: Audio }`.
pub(super) fn audio_mut(clip: &mut Clip) -> Result<&mut AudioClip, EditError> {
    let id = clip.id;
    match &mut clip.content {
        ClipContent::Audio(a) => Ok(a),
        _ => Err(EditError::WrongClipKind {
            clip: id,
            expected: ClipKind::Audio,
        }),
    }
}

/// Rejects a file link the model cannot work with.
pub(super) fn check_file(file: &AudioFile) -> Result<(), EditError> {
    if file.frames == 0 || file.sample_rate == 0 || file.channels == 0 {
        return Err(EditError::Invalid("an audio file has no frames"));
    }
    Ok(())
}

/// A new audio clip window: validates `file` and `offset`, makes the clip, and returns it with the
/// length clamped to the file (`Invalid` when the window is empty).
pub(super) fn new_audio(
    file: AudioFile,
    offset: u64,
    tempo: Tempo,
    length: Tick,
) -> Result<(AudioClip, Tick), EditError> {
    check_file(&file)?;
    if offset >= file.frames {
        return Err(EditError::Invalid("offset behind the end of the file"));
    }
    let mut a = AudioClip::new(file, offset, tempo);
    let max = a.max_length(tempo);
    if max < MIN_LENGTH {
        return Err(EditError::Invalid("the audio window is empty"));
    }
    let length = length.max(MIN_LENGTH).min(max);
    a.clamp(length);
    Ok((a, length))
}

fn retained_automation_base(track: &Track, target: &AutomationTarget) -> Option<f32> {
    match *target {
        AutomationTarget::Volume => Some(1.0 - db_to_p(track.channel.volume_db)),
        AutomationTarget::Pan => Some((track.channel.pan.clamp(-1.0, 1.0) + 1.0) * 0.5),
        AutomationTarget::Send { send } => track
            .sends
            .iter()
            .find(|item| item.id == send)
            .map(|item| 1.0 - db_to_p(item.level_db)),
        AutomationTarget::Macro { index } => {
            track.macros.0.get(usize::from(index)).map(|m| m.value)
        }
        AutomationTarget::Param { .. } => None,
    }
}

/// Checks that automation of the retained mixer controls and macros fits into arrangement lanes.
/// The arrangement has no compact repeat representation, so the check includes disabled lanes.
pub fn validate_flatten_automation(track: &Track) -> Result<(), EditError> {
    let retained_lanes = track
        .automation
        .lanes
        .iter()
        .filter(|lane| retained_automation_base(track, &lane.target).is_some());
    let estimate = retained_lanes
        .clone()
        .map(|lane| clip_expansion_points(track, &lane.target))
        .fold(0, u128::saturating_add);
    if estimate > AUTOMATION_EXPANSION_LIMIT {
        let mut budget = AUTOMATION_EXPANSION_LIMIT;
        for lane in retained_lanes {
            let points = count_clip_expansion(track, &lane.target, budget)
                .ok_or(EditError::AutomationExpansionTooLarge)?;
            budget -= points;
        }
    }
    Ok(())
}

/// Replaces a MIDI track's arrangement clips and device chain with a rendered audio file.
/// Keeping its ID and channel strip preserves routing, sends and sidechains that read this track.
pub fn flatten_midi_track(
    project: &Project,
    id: TrackId,
    start: Tick,
    file: AudioFile,
) -> Result<(Project, ClipId), EditError> {
    let index = project
        .track_index(id)
        .ok_or(EditError::TrackNotFound(id))?;
    let source = &project.tracks[index];
    if source.kind != TrackKind::Midi || source.clips.is_empty() {
        return Err(EditError::Invalid(
            "flattening requires a MIDI track with clips",
        ));
    }
    validate_flatten_automation(source)?;
    let mut next = project.clone();
    let track = Arc::make_mut(&mut next.tracks[index]);
    track.kind = TrackKind::Audio;
    super::macros::remove_targets(&mut track.macros, |target| {
        matches!(target, AutomationTarget::Param { device, .. }
            if track.channel.devices.iter().any(|item| item.id == *device))
    });
    track.channel.devices.clear();
    track
        .automation
        .lanes
        .retain(|lane| !matches!(lane.target, AutomationTarget::Param { .. }));
    // The render excludes the fader, pan and sends. Preserve their curves and those of track
    // macros that can still drive them before deleting the MIDI clips that override the lanes.
    for lane in &mut track.automation.lanes {
        if let Some(base) = retained_automation_base(source, &lane.target)
            && let Some(points) = merged_clip_points(source, lane, base)
        {
            Arc::make_mut(lane).points = Points::new(points);
        }
    }
    track.clips.clear();
    track.arm = false;
    track.monitor = false;
    track.input = Default::default();
    track.midi_channel = Default::default();
    track.audio = Default::default();
    super::automation::prune_hidden_unused(&mut next, id)?;
    super::add_clip(
        &next,
        id,
        super::NewClip {
            name: source.name.clone(),
            start,
            length: Tick(i64::MAX),
            content: super::NewClipContent::Audio { file, offset: 0 },
        },
    )
}

/// Clamps the length of an audio clip's window in `c` to the file ([`AudioClip::max_length`]) and
/// its fades to the length; other clips get `length` as it is.
pub(super) fn fit_length(c: &mut Clip, length: Tick, tempo: Tempo) -> Result<(), EditError> {
    let length = length.max(MIN_LENGTH);
    if let ClipContent::Audio(a) = &mut c.content {
        let max = a.max_length(tempo);
        if max < MIN_LENGTH {
            return Err(EditError::Invalid("the audio window is empty"));
        }
        let length = length.min(max);
        a.validate_warp(length)?;
        a.clamp(length);
        c.length = length;
    } else {
        c.length = length;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// The overlap-cut rule
// ------------------------------------------------------------------------------------------------

/// The clip `piece` of `c` between `a` and `b` (timeline ticks, `c.start ≤ a < b ≤ c.end()`): the
/// window moves with the start, an edge the cut created loses its fade.
fn piece(c: &Clip, a: Tick, b: Tick, tempo: Tempo) -> Result<Option<Clip>, EditError> {
    let ClipContent::Audio(_) = &c.content else {
        return Ok(None);
    };
    let mut out = c.clone();
    let ClipContent::Audio(na) = &mut out.content else {
        unreachable!("a copy of an audio clip");
    };
    if a > c.start {
        na.shift_sources(a - c.start, tempo)?;
        na.fade_in = Tick::ZERO;
    }
    if b < c.end() {
        na.fade_out = Tick::ZERO;
    }
    out.start = a;
    let length = (b - a).min(na.max_length(tempo));
    if length < MIN_LENGTH {
        return Ok(None);
    }
    out.length = length;
    na.validate_warp(length)?;
    na.clamp(length);
    Ok(Some(out))
}

/// Cuts the audio clips of track `track_index` that `is_victim` accepts against the intervals
/// `tops` (start, end): what remains of a victim outside all intervals stays, as one clip or
/// several (the first keeps the id). Nothing changes (no `Arc` is replaced) when no victim
/// overlaps an interval.
fn cut_victims(
    p: &mut Project,
    track_index: usize,
    tops: &[(Tick, Tick)],
    is_victim: impl Fn(&Clip) -> bool,
) -> Result<(), EditError> {
    let tempo = p.tempo;
    let mut tops = tops.to_vec();
    tops.sort();
    let old = p.tracks[track_index].clips.clone();
    let mut out: Vec<Arc<Clip>> = Vec::with_capacity(old.len());
    let mut changed = false;
    for clip in old {
        let hit = matches!(clip.content, ClipContent::Audio(_))
            && is_victim(&clip)
            && tops.iter().any(|&(s, e)| s < clip.end() && clip.start < e);
        if !hit {
            out.push(clip);
            continue;
        }
        changed = true;
        let mut cursor = clip.start;
        let mut spans: Vec<(Tick, Tick)> = Vec::new();
        for &(s, e) in &tops {
            if e <= cursor || s >= clip.end() {
                continue;
            }
            if s > cursor {
                spans.push((cursor, s));
            }
            cursor = cursor.max(e);
        }
        if cursor < clip.end() {
            spans.push((cursor, clip.end()));
        }
        let mut first = true;
        for (a, b) in spans {
            if let Some(mut cut) = piece(&clip, a, b, tempo)? {
                if !std::mem::take(&mut first) {
                    cut.id = ClipId(p.alloc_id()?);
                }
                out.push(Arc::new(cut));
            }
        }
    }
    if changed {
        out.sort_by_key(|c| (c.start, c.id));
        track_mut(p, track_index).clips = out;
    }
    Ok(())
}

/// The overlap-cut rule: the clips `tops` (ids, on whatever tracks) cut the other audio clips of
/// their tracks. Ids that are not audio clips are ignored.
pub(super) fn cut_below(p: &mut Project, tops: &[ClipId]) -> Result<(), EditError> {
    for track_index in 0..p.tracks.len() {
        let track = &p.tracks[track_index];
        if !track.is_audio() {
            continue;
        }
        let intervals: Vec<(Tick, Tick)> = track
            .clips
            .iter()
            .filter(|c| tops.contains(&c.id))
            .map(|c| (c.start, c.end()))
            .collect();
        if !intervals.is_empty() {
            cut_victims(p, track_index, &intervals, |c| !tops.contains(&c.id))?;
        }
    }
    Ok(())
}

/// Makes every audio track overlap-free: where clips overlap, the one that starts later cuts the
/// one before it (equal starts: the higher id wins). A project made by the edits has no overlaps;
/// this repairs a hand-edited file and the effect of a tempo change.
pub(crate) fn normalize_overlaps(p: &mut Project) -> Result<(), EditError> {
    for track_index in 0..p.tracks.len() {
        if !p.tracks[track_index].is_audio() {
            continue;
        }
        let tops: Vec<(ClipId, Tick, Tick)> = p.tracks[track_index]
            .clips
            .iter()
            .map(|c| (c.id, c.start, c.end()))
            .collect();
        for (id, start, end) in tops {
            cut_victims(p, track_index, &[(start, end)], |c| {
                c.start < start || (c.start == start && c.id < id)
            })?;
        }
    }
    Ok(())
}

/// After a tempo change from `old` to `new`: audio clips in Fixed mode keep their duration in
/// seconds (length and fades scale with the tempo, the start stays on the bar grid). Where a longer
/// clip now runs into the next one, the clip that starts later wins ([`normalize_overlaps`]).
/// Re-Pitch clips keep their ticks.
pub(super) fn retime(p: &mut Project, old: Tempo, new: Tempo) -> Result<(), EditError> {
    if old.bpm == new.bpm {
        return Ok(());
    }
    let factor = new.bpm / old.bpm;
    let scale = |t: Tick| Tick((t.0 as f64 * factor).round() as i64);
    let mut any = false;
    for track_index in 0..p.tracks.len() {
        if !p.tracks[track_index].is_audio() {
            continue;
        }
        let clips: Vec<Arc<Clip>> = p.tracks[track_index]
            .clips
            .iter()
            .map(|c| match &c.content {
                ClipContent::Audio(a)
                    if a.time_mode == ClipTimeMode::Fixed && !a.warp_enabled() =>
                {
                    any = true;
                    let mut c = Clip::clone(c);
                    let ClipContent::Audio(a) = &mut c.content else {
                        unreachable!("matched above");
                    };
                    let length = scale(c.length).max(MIN_LENGTH);
                    a.fade_in = scale(a.fade_in);
                    a.fade_out = scale(a.fade_out);
                    let length = length.min(a.max_length(new).max(MIN_LENGTH));
                    a.clamp(length);
                    c.length = length;
                    Arc::new(c)
                }
                _ => Arc::clone(c),
            })
            .collect();
        track_mut(p, track_index).clips = clips;
    }
    if any {
        normalize_overlaps(p)?;
    }
    Ok(())
}

/// `project` with `f` applied to every audio file link of every audio clip; `None` when `f` returns
/// `None` for all of them (nothing to change). Tracks and clips that do not change keep their
/// `Arc`. Not an undo step by itself: the app applies it with `Document::rewrite_untracked` when it
/// saves or collects, like the Sampler's links.
pub fn rewrite_audio_files(
    project: &Project,
    mut f: impl FnMut(&AudioFile) -> Option<AudioFile>,
) -> Option<Project> {
    let mut next = project.clone();
    let mut changed = false;
    for track in &mut next.tracks {
        if !track
            .clips
            .iter()
            .any(|c| matches!(c.content, ClipContent::Audio(_)))
        {
            continue;
        }
        let mut track_changed = false;
        let clips: Vec<Arc<Clip>> = track
            .clips
            .iter()
            .map(|c| match &c.content {
                ClipContent::Audio(a) => {
                    let mut next = a.clone();
                    let mut modified = false;
                    for take in &mut next.takes {
                        if let Some(file) = f(&take.file)
                            && file != take.file
                        {
                            take.file = file;
                            modified = true;
                        }
                    }
                    if modified {
                        track_changed = true;
                        let mut c = Clip::clone(c);
                        c.content = ClipContent::Audio(next);
                        Arc::new(c)
                    } else {
                        Arc::clone(c)
                    }
                }
                _ => Arc::clone(c),
            })
            .collect();
        if track_changed {
            Arc::make_mut(track).clips = clips;
            changed = true;
        }
    }
    changed.then_some(next)
}
