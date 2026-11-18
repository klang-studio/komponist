//! Audio clips in the engine (ARCHITECTURE §6.9.3, issue #11).
//!
//! An audio track plays the clips of its [`AudioPlan`]: a sorted table of non-overlapping
//! [`Segment`]s in frames of the engine rate (built by [`compile::build`](crate::compile::build)
//! from the clips and the tempo map). Playback never decodes on the audio thread:
//!
//! - A **streamer** thread ([`streamer`]) reads the files (`komponist_audiofile`), converts them to
//!   the engine rate (and, for Re-Pitch, to the clip's speed), applies clip gain, fades and the
//!   3 ms edge fades, and writes the rendered track audio **in playback order** into a lock-free
//!   ring per track ([`Ring`]). Loop wraps are predicted: the stream continues at the loop start.
//! - The audio thread reads the ring ([`voice`]); where the ring has not caught up (right after a
//!   locate) it plays the **head** of the segment it is in (the first [`HEAD_MS`] of every segment,
//!   rendered in advance by the streamer and held in memory) and counts what it cannot cover as an
//!   underrun, playing silence: it never blocks.
//! - Offline renders (export, tests) have no deadline: the engine runs the streamer's work on the
//!   rendering thread before every block, so the result is the streamer's output exactly and does
//!   not depend on thread timing.
//!
//! Everything here that touches files or allocates runs on the streamer or the compiling thread;
//! `voice` is the only module of the audio thread.

mod render;
mod ring;
pub(crate) mod streamer;
mod stretch;
pub(crate) mod voice;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use klang_dsp::units::db_to_gain;
use komponist_audiofile::{FilePool, PoolFile};
use komponist_model::{
    AudioClip, CLIP_GAIN_DB_MIN, Clip, ClipContent, ClipId, EDGE_FADE_MS, TempoMap, Tick, Track,
    WarpMap,
};

pub use ring::{RING_FRAMES, Ring};
pub use streamer::StreamStats;
pub use stretch::{StretchError, WarpSegment};

/// Length in milliseconds of the head every segment keeps in memory: what plays right after a
/// locate or a jump while the streamer refills the ring.
pub const HEAD_MS: f64 = 100.0;

/// Longest edge fade in frames (3 ms stays below it up to 384 kHz).
pub const MAX_EDGE_FRAMES: usize = 2048;

/// A missing file is looked for again at most this often while a track plays it.
const RETRY_AFTER: Duration = Duration::from_millis(500);

/// What the program knows about the file behind a clip, for the UI (missing-file marks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceStatus {
    /// Not looked at yet: the streamer opens a file when a track first needs it.
    Pending,
    /// The file is open in the pool and plays.
    Ready,
    /// The link resolves to no file. The clip plays silence; a new look happens at the next
    /// locate or start.
    Missing,
    /// The file exists but cannot be decoded.
    Unreadable,
}

impl SourceStatus {
    fn from_u8(v: u8) -> SourceStatus {
        match v {
            1 => SourceStatus::Ready,
            2 => SourceStatus::Missing,
            3 => SourceStatus::Unreadable,
            _ => SourceStatus::Pending,
        }
    }
}

/// The file of one or more clips: shared by every clip that links it (one open [`PoolFile`], so
/// one pool entry), held for as long as a program plays a clip of it.
pub struct ClipSource {
    link: komponist_model::AudioFile,
    dir: Option<PathBuf>,
    status: AtomicU8,
    inner: Mutex<SourceInner>,
}

#[derive(Default)]
struct SourceInner {
    file: Option<Arc<PoolFile>>,
    tried: Option<Instant>,
}

impl std::fmt::Debug for ClipSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipSource")
            .field("file", &self.link.abs)
            .field("status", &self.status())
            .finish()
    }
}

impl ClipSource {
    fn new(link: komponist_model::AudioFile, dir: Option<PathBuf>) -> ClipSource {
        ClipSource {
            link,
            dir,
            status: AtomicU8::new(0),
            inner: Mutex::new(SourceInner::default()),
        }
    }

    /// The link as the clip has it.
    pub fn link(&self) -> &komponist_model::AudioFile {
        &self.link
    }

    /// Whether the file plays, is missing, is unreadable or has not been looked at (any thread,
    /// never blocks).
    pub fn status(&self) -> SourceStatus {
        SourceStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    /// Non-RT, may block on file IO. The open file, looking for it (resolving the link, opening
    /// it in the pool) unless it is open or was looked for less than half a second ago.
    pub(crate) fn open(&self) -> Option<Arc<PoolFile>> {
        self.open_with(false)
    }

    /// Non-RT. Like [`ClipSource::open`] but looks again right now (export).
    pub fn open_now(&self) -> SourceStatus {
        self.open_with(true);
        self.status()
    }

    fn open_with(&self, force: bool) -> Option<Arc<PoolFile>> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !force && let Some(file) = &inner.file {
            return Some(Arc::clone(file));
        }
        if !force && inner.tried.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
            return None;
        }
        inner.tried = Some(Instant::now());
        // An export must validate the link again: PoolFile caches metadata, not an open
        // decoder, so a previously playable file can have disappeared or changed meanwhile.
        inner.file = None;
        let status = match self.link.resolve(self.dir.as_deref()) {
            None => SourceStatus::Missing,
            Some(path) => match FilePool::global().open(&path) {
                Ok(file) => {
                    inner.file = Some(file);
                    SourceStatus::Ready
                }
                Err(_) => SourceStatus::Unreadable,
            },
        };
        self.status.store(status as u8, Ordering::Relaxed);
        inner.file.clone()
    }
}

// `SourceStatus as u8` must match `from_u8`.
const _: () = assert!(SourceStatus::Ready as u8 == 1);

/// One clip's stretch of the timeline, in frames of the engine rate (song samples: sample 0 is
/// tick 0). Segments of a plan are sorted and never overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// The model clip (the right piece of a split one has its own id).
    pub clip: ClipId,
    /// First frame of the segment.
    pub start: i64,
    /// Frame behind the last one.
    pub end: i64,
    /// Index into [`AudioPlan::sources`].
    pub source: u32,
    /// The file frame at the segment start, converted to the engine rate and rounded: the stream
    /// the segment reads is the file resampled to the engine rate, this is where it starts.
    pub x0: i64,
    /// Engine-rate source frames per output frame: 1 for a Fixed clip, `project / original` tempo
    /// for Re-Pitch.
    pub speed: f64,
    /// Worker-only pitch-preserving transform, independent of the legacy resampling speed.
    pub warp: Option<WarpSegment>,
    /// Clip gain, linear.
    pub gain: f32,
    /// Fade-in and fade-out of the clip in frames (linear amplitude).
    pub fade_in: u32,
    /// See [`Segment::fade_in`].
    pub fade_out: u32,
    /// The 3 ms edge fade at the start: set unless the segment continues the one before it
    /// without a seam (a clip split in two keeps playing through the cut).
    pub edge_in: bool,
    /// The 3 ms edge fade at the end.
    pub edge_out: bool,
}

impl Segment {
    /// Length in frames.
    pub fn len(&self) -> i64 {
        self.end - self.start
    }

    /// Whether the segment has no frames (never true in a plan).
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Heads of the segments of a plan: planar stereo, `min(head frames, segment length)` frames each.
pub(crate) struct Heads {
    pub(crate) segments: Vec<[Box<[f32]>; 2]>,
}

static NEXT_PLAN: AtomicU64 = AtomicU64::new(1);

/// What an audio track plays: compiled on the UI thread, read by the audio thread (the table and
/// the heads), the streamer (everything) and the UI (the sources' status). A plan (its ring) belongs
/// to the one engine that plays its program: two engines must not play the same plan.
pub struct AudioPlan {
    id: u64,
    rate: u32,
    tempo: TempoMap,
    dir: Option<PathBuf>,
    sources: Vec<Arc<ClipSource>>,
    segments: Arc<[Segment]>,
    ring: Arc<Ring>,
    heads: OnceLock<Heads>,
    offline: bool,
    /// The Warp chain cut by an export and its exact cut frame.
    export_fade: Option<(i64, i64)>,
    edge_frames: u32,
    head_frames: usize,
    diagnostics: stretch::Diagnostics,
    /// The clips this was compiled from, for the incremental rebuild.
    clips: Vec<Arc<Clip>>,
}

impl std::fmt::Debug for AudioPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPlan")
            .field("id", &self.id)
            .field("rate", &self.rate)
            .field("segments", &self.segments.len())
            .finish()
    }
}

impl AudioPlan {
    /// The sample rate the plan was compiled for. The engine plays a plan of another rate as
    /// silence until the program is compiled again.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The segments, sorted by start.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The files the segments play ([`Segment::source`] indexes this).
    pub fn sources(&self) -> &[Arc<ClipSource>] {
        &self.sources
    }

    /// The source of the segment that plays `clip`.
    pub fn source_of(&self, clip: ClipId) -> Option<&Arc<ClipSource>> {
        let segment = self.segments.iter().find(|s| s.clip == clip)?;
        self.sources.get(segment.source as usize)
    }

    pub(crate) fn failure_count(&self, i: usize) -> u64 {
        self.diagnostics.count(i)
    }
    pub(crate) fn failure_reason(&self, i: usize) -> Option<StretchError> {
        self.diagnostics.error(i)
    }

    /// Number of worker stretch failures observed by this plan (including head preparation).
    pub fn stretch_failures(&self) -> u64 {
        self.diagnostics.total()
    }
    /// Last observed failure for a clip; historical until the plan is replaced.
    pub fn stretch_error(&self, clip: ClipId) -> Option<StretchError> {
        self.segments
            .iter()
            .position(|s| s.clip == clip)
            .and_then(|i| self.diagnostics.error(i))
    }

    /// Whether the heads of all segments are in memory (they are built in the background after
    /// the plan first plays).
    pub fn heads_ready(&self) -> bool {
        self.heads.get().is_some()
    }

    /// The project folder the plan's links resolve against.
    pub fn dir(&self) -> Option<&std::path::Path> {
        self.dir.as_deref()
    }

    /// Frames of the 3 ms edge fade at the plan's rate.
    pub fn edge_frames(&self) -> u32 {
        self.edge_frames
    }

    /// The segment containing frame `q`, or the first one starting after it.
    pub(crate) fn locate(&self, q: i64) -> Result<usize, Option<i64>> {
        let after = self.segments.partition_point(|s| s.start <= q);
        match after.checked_sub(1) {
            Some(i) if q < self.segments[i].end => Ok(i),
            _ => Err(self.segments.get(after).map(|s| s.start)),
        }
    }

    /// A fresh offline plan with its own ring and diagnostics, even if no clip needs trimming.
    /// Only the export's synchronous streamer services the plan, and nothing plays from `end`.
    pub(crate) fn for_export(&self, end: i64) -> Arc<AudioPlan> {
        let export_fade = self.locate(end).ok().and_then(|i| {
            let cut = &self.segments[i];
            // A cut at a seamless split can need a fade across both neighboring segments.
            cut.warp
                .filter(|_| cut.start < end || !cut.edge_in)
                .map(|warp| (warp.chain_start, end))
        });
        let segments: Vec<Segment> = self
            .segments
            .iter()
            .filter(|s| s.start < end)
            .map(|s| Segment {
                end: s.end.min(end),
                edge_out: if s.end > end {
                    s.warp.is_none()
                } else {
                    s.edge_out
                },
                ..s.clone()
            })
            .collect();
        Arc::new(AudioPlan {
            id: NEXT_PLAN.fetch_add(1, Ordering::Relaxed),
            rate: self.rate,
            tempo: self.tempo,
            dir: self.dir.clone(),
            sources: self.sources.clone(),
            diagnostics: stretch::Diagnostics::new(segments.len()),
            segments: segments.into(),
            ring: Arc::new(Ring::new()),
            heads: OnceLock::new(),
            offline: true,
            export_fade,
            edge_frames: self.edge_frames,
            head_frames: self.head_frames,
            clips: self.clips.clone(),
        })
    }
}

/// Files shared between the plans of one build: seeded with the sources of the previous program so
/// a recompile keeps the open files (and the pool entries).
pub(crate) struct SourceCache {
    map: HashMap<(PathBuf, PathBuf, Option<PathBuf>), Arc<ClipSource>>,
}

impl SourceCache {
    pub(crate) fn new(previous: impl Iterator<Item = Arc<ClipSource>>) -> SourceCache {
        let map = previous
            .map(|s| ((s.link.abs.clone(), s.link.rel.clone(), s.dir.clone()), s))
            .collect();
        SourceCache { map }
    }

    fn get(
        &mut self,
        link: &komponist_model::AudioFile,
        dir: Option<&std::path::Path>,
    ) -> Arc<ClipSource> {
        let key = (link.abs.clone(), link.rel.clone(), dir.map(Into::into));
        Arc::clone(
            self.map
                .entry(key)
                .or_insert_with(|| Arc::new(ClipSource::new(link.clone(), dir.map(Into::into)))),
        )
    }
}

/// What the compile step needs of the environment for audio.
pub(crate) struct PlanEnv<'a> {
    pub(crate) rate: u32,
    pub(crate) tempo: TempoMap,
    pub(crate) dir: Option<&'a std::path::Path>,
}

/// Whether `previous` plays exactly what `clips` and `env` say (same clip `Arc`s, same rate, tempo
/// map and project folder): then the plan, its ring and its heads stay.
pub(crate) fn plan_matches(previous: &AudioPlan, clips: &[Arc<Clip>], env: &PlanEnv<'_>) -> bool {
    previous.rate == env.rate
        && previous.tempo == env.tempo
        && previous.dir.as_deref() == env.dir
        && previous.clips.len() == clips.len()
        && previous
            .clips
            .iter()
            .zip(clips)
            .all(|(a, b)| Arc::ptr_eq(a, b))
}

/// Keep the model coordinates during compilation so seamless Warp joins do not have to
/// infer continuity from rounded engine frames.
struct PlannedSegment {
    segment: Segment,
    clip_start: Tick,
    clip_length: Tick,
    warp_map: Option<WarpMap>,
}

impl PlannedSegment {
    fn warp_continues_with(&self, next: &Self) -> bool {
        let (Some(origin), Some(continuation)) = (self.warp_map, next.warp_map) else {
            return false;
        };
        let at = next.clip_start - self.clip_start;
        origin.continues_with(at, continuation, next.clip_length)
    }
}

/// The plan of audio track `track`: one segment per unmuted clip with frames, the files from
/// `cache`, the ring of `previous` if there is one.
pub(crate) fn build_plan(
    track: &Track,
    env: &PlanEnv<'_>,
    cache: &mut SourceCache,
    previous: Option<&AudioPlan>,
) -> AudioPlan {
    let rate = f64::from(env.rate);
    let map = &env.tempo;
    let frame = |tick: Tick| map.tick_to_sample(tick, rate);
    let mut sources: Vec<Arc<ClipSource>> = Vec::new();
    let mut planned = Vec::new();
    for clip in track.clips.iter().filter(|c| !c.mute) {
        let ClipContent::Audio(audio) = &clip.content else {
            continue;
        };
        let (start, end) = (frame(clip.start), frame(clip.start + clip.length));
        if end <= start || audio.file.sample_rate == 0 {
            continue;
        }
        let source = cache.get(&audio.file, env.dir);
        let index = match sources.iter().position(|s| Arc::ptr_eq(s, &source)) {
            Some(i) => i,
            None => {
                sources.push(source);
                sources.len() - 1
            }
        };
        planned.push(PlannedSegment {
            segment: segment(clip, audio, index as u32, (start, end), env, &frame),
            clip_start: clip.start,
            clip_length: clip.length,
            warp_map: audio.warp_map(audio.active_take).ok(),
        });
    }
    planned.sort_by_key(|s| s.segment.start);
    // The model keeps audio clips apart; a hand-made track that overlaps loses the earlier one's
    // end (the later clip starts, so it wins). It cannot join with its now mismatched Warp map.
    for i in 1..planned.len() {
        let next = planned[i].segment.start;
        let prev = &mut planned[i - 1];
        if prev.segment.end > next {
            prev.segment.end = next;
            prev.warp_map = None;
        }
    }
    planned.retain(|s| !s.segment.is_empty());
    join_seamless(&mut planned);
    let segments: Vec<_> = planned.into_iter().map(|s| s.segment).collect();
    let edge = (EDGE_FADE_MS / 1000.0 * rate).round().max(1.0) as u32;
    AudioPlan {
        id: NEXT_PLAN.fetch_add(1, Ordering::Relaxed),
        rate: env.rate,
        tempo: env.tempo,
        dir: env.dir.map(Into::into),
        sources,
        diagnostics: stretch::Diagnostics::new(segments.len()),
        segments: segments.into(),
        ring: previous.map_or_else(|| Arc::new(Ring::new()), |p| Arc::clone(&p.ring)),
        heads: OnceLock::new(),
        offline: false,
        export_fade: None,
        edge_frames: edge.min(MAX_EDGE_FRAMES as u32),
        head_frames: (HEAD_MS / 1000.0 * rate).round() as usize,
        clips: track.clips.clone(),
    }
}

fn segment(
    clip: &Clip,
    audio: &AudioClip,
    source: u32,
    (start, end): (i64, i64),
    env: &PlanEnv<'_>,
    frame: &impl Fn(Tick) -> i64,
) -> Segment {
    let len = end - start;
    let ratio = f64::from(env.rate) / f64::from(audio.file.sample_rate);
    let gain = if audio.gain_db > CLIP_GAIN_DB_MIN {
        db_to_gain(audio.gain_db)
    } else {
        0.0
    };
    let fade_in = (frame(clip.start + audio.fade_in) - start).clamp(0, len);
    let fade_out = (end - frame(clip.start + clip.length - audio.fade_out)).clamp(0, len);
    Segment {
        clip: clip.id,
        start,
        end,
        source,
        x0: if audio.warp_enabled() {
            let native = u128::from(audio.file.sample_rate);
            let frame = (u128::from(audio.offset) * u128::from(env.rate) + native / 2) / native;
            i64::try_from(frame).unwrap_or(i64::MAX)
        } else {
            (audio.offset as f64 * ratio).round() as i64
        },
        speed: audio.speed(env.tempo.tempo),
        warp: audio.warp_enabled().then(|| WarpSegment {
            ratio: audio.original_tempo.bpm / env.tempo.tempo.bpm,
            chain_start: start,
        }),
        gain,
        fade_in: fade_in as u32,
        fade_out: fade_out as u32,
        edge_in: true,
        edge_out: true,
    }
}

/// Carries Warp history only across exact model-space joins. Fixed and Re-Pitch
/// retain their one-engine-frame tolerance.
fn join_seamless(planned: &mut [PlannedSegment]) {
    let mut chain = 0;
    for i in 1..planned.len() {
        let (prev, next) = (&planned[i - 1], &planned[i]);
        let (a, b) = (&prev.segment, &next.segment);
        let continuous = match (a.warp, b.warp) {
            (Some(a), Some(b)) => {
                a.ratio == b.ratio
                    && prev.clip_start + prev.clip_length == next.clip_start
                    && prev.warp_continues_with(next)
                    && planned[chain].warp_continues_with(next)
            }
            (None, None) => {
                let played = (a.len() as f64 * a.speed).round() as i64;
                (i128::from(b.x0) - i128::from(a.x0) - i128::from(played)).abs() <= 1
            }
            _ => false,
        };
        let seamless = a.source == b.source
            && a.end == b.start
            && a.speed == b.speed
            && a.gain == b.gain
            && a.fade_out == 0
            && b.fade_in == 0
            && continuous;
        if seamless {
            let origin = planned[chain].segment.start;
            planned[i - 1].segment.edge_out = false;
            planned[i].segment.edge_in = false;
            if let Some(next) = &mut planned[i].segment.warp {
                next.chain_start = origin;
            }
        } else {
            chain = i;
        }
    }
}
