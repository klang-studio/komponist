//! Audio tracks and audio clips (wave 10, epic #8; ARCHITECTURE §6.9).
//!
//! The model only describes audio: it never reads a sample. A clip links a file and carries the
//! file's length and rate, so edits, the arrangement and the saved project work with the file
//! missing; decoding, resampling and peaks are the engine's and the app's job (#10).
//!
//! Owner decisions (2026-11-17, epic #8) this module follows:
//! - Separate track kinds: [`crate::TrackKind::Audio`] holds audio clips only, MIDI and bus
//!   tracks refuse them, there are no hybrid tracks.
//! - Clip **positions** follow the bar grid ([`crate::Clip::start`] in ticks); the **content** stays
//!   fixed. Without Re-Pitch a clip keeps its duration in seconds when the tempo changes, with
//!   Re-Pitch ([`ClipTimeMode::RePitch`]) the content is resampled with the tempo and the clip keeps
//!   its length in ticks.
//! - Files are **linked** like the Sampler's samples ([`AudioFile`]: absolute path and path relative
//!   to the project file); „Sammeln und speichern“ copies them next to the project.
//! - No Reverse in v1.
//!
//! Units: positions and lengths on the timeline in ticks, positions inside the file in **frames of
//! the file's own sample rate**, gain in dB.
//!
//! Thread: plain data, used on the UI thread and on workers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::TrackId;
use crate::project::VOLUME_DB_MIN;
use crate::time::{PPQ, Tempo, Tick};

/// Lowest clip gain in dB; the value means silence (like [`VOLUME_DB_MIN`] on a fader).
pub const CLIP_GAIN_DB_MIN: f32 = VOLUME_DB_MIN;
/// Highest clip gain in dB (+24, as in most DAWs' clip gain).
pub const CLIP_GAIN_DB_MAX: f32 = 24.0;

/// Length in milliseconds of the short fade the playback puts on every edge a cut or an overlap
/// creates (owner decision: 3 ms edge fades, no crossfade in v1). It is not stored: the engine
/// applies it where a clip starts or ends against a neighbour or against the end of its content.
pub const EDGE_FADE_MS: f64 = 3.0;

/// A linked audio file: where it is, and what it is (length, rate, channels).
///
/// The link is the Sampler's (`komponist_builtin::sampler::links`): the file is looked up through
/// the path relative to the folder of the project file first, then through the absolute path, so a
/// project folder that moved as a whole keeps working. While the project has no file yet,
/// [`AudioFile::rel`] is empty; a recording of such a project lives in a user folder (its `abs`) and
/// moves into the project on the first save (the app knows that folder, #13).
///
/// `frames`, `sample_rate` and `channels` are what the file had when it was linked or relinked;
/// they let the model clamp lengths and the UI draw an extent for a missing file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioFile {
    /// The absolute path when the link was made or last saved.
    #[serde(with = "path_text")]
    pub abs: PathBuf,
    /// The path relative to the project file's folder when last saved; empty before the project has
    /// a file (not written then).
    #[serde(
        default,
        with = "path_text",
        skip_serializing_if = "path_text::is_empty"
    )]
    pub rel: PathBuf,
    /// Length of the file in frames, ≥ 1.
    pub frames: u64,
    /// Sample rate of the file in Hz, ≥ 1.
    pub sample_rate: u32,
    /// Channel count of the file (1 = mono, 2 = stereo, …), ≥ 1.
    pub channels: u16,
}

impl AudioFile {
    /// A link to `path` (made absolute against the current folder if it is relative) with a
    /// relative path if `project_dir` is known.
    pub fn new(
        path: &Path,
        project_dir: Option<&Path>,
        frames: u64,
        sample_rate: u32,
        channels: u16,
    ) -> AudioFile {
        let abs = crate::paths::absolute(path);
        let rel = project_dir
            .map(|d| crate::paths::relative(&abs, d))
            .unwrap_or_default();
        AudioFile {
            abs,
            rel,
            frames,
            sample_rate,
            channels,
        }
    }

    /// The file name without extension (what a new clip is called).
    pub fn name(&self) -> String {
        let path = if self.abs.as_os_str().is_empty() {
            &self.rel
        } else {
            &self.abs
        };
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The file the link names: the relative path against `project_dir` if that exists, else the
    /// absolute path if that exists, else `None` (the file is missing).
    pub fn resolve(&self, project_dir: Option<&Path>) -> Option<PathBuf> {
        if let Some(dir) = project_dir
            && !self.rel.as_os_str().is_empty()
        {
            let candidate = crate::paths::normalize(&dir.join(&self.rel));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        (!self.abs.as_os_str().is_empty() && self.abs.is_file()).then(|| self.abs.clone())
    }

    /// The link for a project file whose folder is `new_dir`: if the file resolves (against
    /// `old_dir`, the folder of the file the project was loaded from) it gets its absolute path
    /// refreshed and a relative path to `new_dir`; a link that does not resolve stays as it is.
    pub fn rebased(&self, old_dir: Option<&Path>, new_dir: &Path) -> AudioFile {
        match self.resolve(old_dir) {
            Some(found) => AudioFile {
                rel: crate::paths::relative(&found, new_dir),
                abs: found,
                ..self.clone()
            },
            None => self.clone(),
        }
    }

    /// The same file pointed at `to` (a copy made by „Sammeln und speichern“): absolute path `to`,
    /// path relative to `project_dir`.
    pub fn moved_to(&self, to: &Path, project_dir: &Path) -> AudioFile {
        AudioFile {
            abs: to.to_path_buf(),
            rel: crate::paths::relative(to, project_dir),
            ..self.clone()
        }
    }

    /// Duration of the whole file in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }
}

/// serde for paths as text. A path that is not valid Unicode is written lossily (like the
/// Sampler's links) instead of failing the save.
mod path_text {
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&path.to_string_lossy())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        String::deserialize(deserializer).map(PathBuf::from)
    }

    pub fn is_empty(path: &Path) -> bool {
        path.as_os_str().is_empty()
    }
}

/// How an audio clip's content follows the project tempo. An enum, so one mode is active at a time
/// and later modes (warp, #14) are a new variant, not a new flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipTimeMode {
    /// The content plays at its own speed; the clip keeps its duration in seconds when the tempo
    /// changes ([`crate::edits::set_project_prop`]).
    #[default]
    Fixed,
    /// Re-Pitch: the content is resampled with the project tempo (it plays at `project / original`
    /// speed, pitch follows) and the clip keeps its length in ticks.
    RePitch,
}

/// Audio content of a clip: a window into a linked file.
///
/// The window starts at [`AudioClip::offset`] frames into the file and runs for the clip's
/// timeline length ([`crate::Clip::length`]): how many frames that is depends on the tempo, see
/// [`AudioClip::source_frames`]. Past the end of the file the clip is silent; edits keep the clip
/// inside the file ([`AudioClip::max_length`]).
///
/// **Overlap rule** (edits, not stored here): audio clips on one track never overlap. A clip that
/// is added, moved, copied or stretched over others cuts them (the clip below is shortened, split
/// or removed, each piece keeping its window into the file), so the model always says what plays;
/// the playback puts [`EDGE_FADE_MS`] fades on the cut edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    /// The linked file.
    pub file: AudioFile,
    /// Start of the window in the file, in frames of the file's rate, `0 ..< file.frames`.
    pub offset: u64,
    /// Clip gain in dB, [`CLIP_GAIN_DB_MIN`] ..= [`CLIP_GAIN_DB_MAX`]. Default 0.
    #[serde(default)]
    pub gain_db: f32,
    /// Fade-in length from the clip start, in timeline ticks, ≥ 0. `fade_in + fade_out` never
    /// exceeds the clip length.
    #[serde(default)]
    pub fade_in: Tick,
    /// Fade-out length to the clip end, in timeline ticks, ≥ 0.
    #[serde(default)]
    pub fade_out: Tick,
    /// How the content follows the project tempo ([`ClipTimeMode`]). Files without the field load
    /// as [`ClipTimeMode::Fixed`].
    #[serde(default)]
    pub time_mode: ClipTimeMode,
    /// The tempo the content was recorded or made at: the project tempo when the clip was created,
    /// changed by hand. Used by [`ClipTimeMode::RePitch`] (and kept while the mode is `Fixed`, so
    /// switching Re-Pitch on starts from it).
    pub original_tempo: Tempo,
}

impl AudioClip {
    /// A window at `offset` frames into `file` with unity gain, no fades, Re-Pitch off and
    /// `original_tempo` = the project tempo.
    pub fn new(file: AudioFile, offset: u64, original_tempo: Tempo) -> AudioClip {
        AudioClip {
            file,
            offset,
            gain_db: 0.0,
            fade_in: Tick::ZERO,
            fade_out: Tick::ZERO,
            time_mode: ClipTimeMode::Fixed,
            original_tempo,
        }
    }

    /// Seconds of the file one timeline tick plays at the project tempo `project`: without Re-Pitch
    /// the content runs in real time (a tick lasts what the project tempo says); with it a tick
    /// plays what a tick lasts at the original tempo.
    pub fn source_secs_per_tick(&self, project: Tempo) -> f64 {
        let bpm = match self.time_mode {
            ClipTimeMode::RePitch => self.original_tempo.bpm,
            ClipTimeMode::Fixed => project.bpm,
        };
        60.0 / (bpm * PPQ as f64)
    }

    /// Playback speed relative to the file: `project / original` with Re-Pitch, else 1. The
    /// engine resamples by this on top of the rate conversion of the file (#11).
    pub fn speed(&self, project: Tempo) -> f64 {
        match self.time_mode {
            ClipTimeMode::RePitch => project.bpm / self.original_tempo.bpm,
            ClipTimeMode::Fixed => 1.0,
        }
    }

    /// How many frames of the file (at the file's rate) `ticks` timeline ticks play, rounded.
    pub fn source_frames(&self, ticks: Tick, project: Tempo) -> u64 {
        let secs = ticks.0.max(0) as f64 * self.source_secs_per_tick(project);
        (secs * f64::from(self.file.sample_rate)).round() as u64
    }

    /// How many whole timeline ticks `frames` frames of the file last, floored.
    pub fn frames_to_ticks(&self, frames: u64, project: Tempo) -> Tick {
        let secs = frames as f64 / f64::from(self.file.sample_rate.max(1));
        Tick((secs / self.source_secs_per_tick(project) + 1e-6).floor() as i64)
    }

    /// The longest clip length in ticks that stays inside the file: what is left behind
    /// [`AudioClip::offset`], floored. 0 when the window is empty.
    pub fn max_length(&self, project: Tempo) -> Tick {
        self.frames_to_ticks(self.file.frames.saturating_sub(self.offset), project)
    }

    /// Clamps gain and fades into range for a clip of `length` ticks: gain into
    /// `CLIP_GAIN_DB_MIN ..= CLIP_GAIN_DB_MAX`, fades ≥ 0 with `fade_in + fade_out ≤ length` (the
    /// fade-in keeps its length first).
    pub fn clamp(&mut self, length: Tick) {
        self.gain_db = self.gain_db.clamp(CLIP_GAIN_DB_MIN, CLIP_GAIN_DB_MAX);
        let length = length.max(Tick::ZERO);
        self.fade_in = self.fade_in.max(Tick::ZERO).min(length);
        self.fade_out = self.fade_out.max(Tick::ZERO).min(length - self.fade_in);
    }
}

/// Which hardware inputs feed an audio track: one channel, or two neighbouring ones as a stereo
/// pair. Channel numbers count from 0 on the audio device's inputs (shown as 1, 2, …); a number the
/// device does not have records silence, the selection is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareInput {
    /// First input channel, `0 ..`.
    pub first: u16,
    /// A stereo pair: channels `first` and `first + 1`. Off: mono, channel `first`.
    pub stereo: bool,
}

impl HardwareInput {
    /// Number of input channels it takes: 1 or 2.
    pub fn channel_count(self) -> u16 {
        if self.stereo { 2 } else { 1 }
    }
}

/// Where an audio track gets its signal from for recording and monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioInput {
    /// No input: the track plays clips only (the default).
    #[default]
    None,
    /// Hardware input channels of the audio device.
    Hardware(HardwareInput),
    /// A bus („Bus aufnehmen“, owner decision 16 A): the track records what leaves the bus (behind
    /// its fader, pan and mute, where its main output goes on). This is an **input edge** bus →
    /// track in the routing graph, [`crate::routing::EdgeKind::Input`], and counts for the cycle
    /// check like a sidechain: the track must not output into that bus or anything feeding it.
    Bus(TrackId),
}

/// Input monitoring of an audio track: whether the live input reaches the track's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorMode {
    /// Never (the default of a new audio track: no feedback surprises).
    #[default]
    Off,
    /// While the track is armed and the transport is stopped or recording, not during playback of
    /// its clips (the usual tape-style auto monitoring).
    Auto,
    /// Always, whatever the transport does.
    On,
}

/// The audio-specific settings of a track. Only an audio track uses them; the other kinds keep the
/// default and the file does not write it then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AudioIo {
    /// Where the track records and monitors from.
    #[serde(default)]
    pub input: AudioInput,
    /// Input monitoring.
    #[serde(default)]
    pub monitor: MonitorMode,
}

impl AudioIo {
    /// Whether this is the default (written to the file only when it is not).
    pub fn is_default(&self) -> bool {
        *self == AudioIo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(frames: u64) -> AudioFile {
        AudioFile {
            abs: PathBuf::from("/a/loop.wav"),
            rel: PathBuf::new(),
            frames,
            sample_rate: 48_000,
            channels: 2,
        }
    }

    #[test]
    fn name_is_the_file_stem() {
        assert_eq!(file(1).name(), "loop");
    }

    #[test]
    fn real_time_clip_follows_the_project_tempo_for_its_source_frames() {
        let clip = AudioClip::new(file(480_000), 0, Tempo::new(120.0));
        // One bar at 120 bpm is 2 s = 96 000 frames.
        assert_eq!(clip.source_frames(Tick(3840), Tempo::new(120.0)), 96_000);
        // At 60 bpm a bar takes 4 s: twice the frames.
        assert_eq!(clip.source_frames(Tick(3840), Tempo::new(60.0)), 192_000);
        assert_eq!(clip.speed(Tempo::new(60.0)), 1.0);
    }

    #[test]
    fn repitch_clip_plays_the_same_frames_per_tick_whatever_the_project_tempo() {
        let mut clip = AudioClip::new(file(480_000), 0, Tempo::new(120.0));
        clip.time_mode = ClipTimeMode::RePitch;
        assert_eq!(clip.source_frames(Tick(3840), Tempo::new(60.0)), 96_000);
        assert_eq!(clip.source_frames(Tick(3840), Tempo::new(180.0)), 96_000);
        assert_eq!(clip.speed(Tempo::new(180.0)), 1.5);
    }

    #[test]
    fn max_length_is_what_is_left_of_the_file() {
        let mut clip = AudioClip::new(file(192_000), 48_000, Tempo::new(120.0));
        // 144 000 frames = 3 s = 1.5 bars at 120 bpm.
        assert_eq!(clip.max_length(Tempo::new(120.0)), Tick(5760));
        // Slower tempo: the same 3 s are fewer ticks.
        assert_eq!(clip.max_length(Tempo::new(60.0)), Tick(2880));
        clip.offset = 192_000;
        assert_eq!(clip.max_length(Tempo::new(120.0)), Tick::ZERO);
    }

    #[test]
    fn clamp_keeps_gain_and_fades_in_range() {
        let mut clip = AudioClip::new(file(1000), 0, Tempo::new(120.0));
        clip.gain_db = 99.0;
        clip.fade_in = Tick(300);
        clip.fade_out = Tick(900);
        clip.clamp(Tick(1000));
        assert_eq!(clip.gain_db, CLIP_GAIN_DB_MAX);
        assert_eq!((clip.fade_in, clip.fade_out), (Tick(300), Tick(700)));
        clip.fade_in = Tick(-5);
        clip.clamp(Tick(100));
        assert_eq!((clip.fade_in, clip.fade_out), (Tick(0), Tick(100)));
    }

    #[test]
    fn rebasing_follows_a_file_that_resolves_and_keeps_one_that_does_not() {
        let missing = file(10);
        assert_eq!(missing.rebased(None, Path::new("/p")), missing);
        let moved = missing.moved_to(Path::new("/p/S/loop.wav"), Path::new("/p"));
        assert_eq!(moved.rel, Path::new("S/loop.wav"));
        assert_eq!(moved.abs, Path::new("/p/S/loop.wav"));
        assert_eq!(moved.frames, 10);
    }
}
