//! Waveform peaks: multi-resolution min/max per channel.
//!
//! Level 0 holds the min and max of every [`PeakData::BASE_BLOCK`] source frames; every further
//! level merges [`PeakData::FACTOR`] buckets of the one below, up to a level with a single bucket.
//! A drawing call ([`PeakData::query`]) asks for a frame range and a number of columns and gets
//! the min/max per column from the coarsest level whose buckets are at most a quarter of a
//! column wide, so the cost of a redraw depends on the width of the view, not on
//! the length of the file, and a zoomed-out 10-minute stereo file costs a few kilobytes per
//! frame of drawing.
//!
//! Frames are **source frames** (the file's own rate, [`PeakData::sample_rate`]); the caller
//! converts ticks to seconds to source frames. Below one bucket per column (zooming in beyond
//! `BASE_BLOCK` frames per column) the answer is level 0, and [`PeakData::query`]'s result
//! says how many frames a column spans; the arrangement then reads the raw samples instead.
//!
//! Memory: 8 bytes per bucket and channel, about `frames / 128 × 8 × 4/3` bytes per channel
//! (≈ 3 MB for a 10-minute 48 kHz mono file).
//!
//! Peaks are computed off the UI thread ([`PeakBuilder`] fed from a streaming decode; the
//! [`crate::FilePool`] does that on its worker thread) and cached in memory (in the pool entry)
//! and on disk ([`cache`]).

pub mod cache;

use crate::AudioError;
use crate::decode::{AudioFile, Decoded};

/// Minimum and maximum sample value of a range of frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    /// Smallest sample.
    pub min: f32,
    /// Largest sample.
    pub max: f32,
}

impl Peak {
    /// What a range without frames shows.
    pub const SILENT: Peak = Peak { min: 0.0, max: 0.0 };

    /// The largest absolute value.
    pub fn abs_max(self) -> f32 {
        self.min.abs().max(self.max.abs())
    }

    fn merge(self, other: Peak) -> Peak {
        Peak {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    const EMPTY: Peak = Peak {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
    };
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Level {
    /// Source frames per bucket.
    pub(crate) block: u64,
    /// `[channel][bucket]`
    pub(crate) data: Vec<Vec<Peak>>,
}

/// The peaks of one file, all levels. Immutable once built; share it in an `Arc`.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakData {
    pub(crate) sample_rate: u32,
    pub(crate) frames: u64,
    pub(crate) levels: Vec<Level>,
}

/// What [`PeakData::query`] returns.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakColumns {
    /// One entry per requested column; columns outside the file are [`Peak::SILENT`].
    pub columns: Vec<Peak>,
    /// Source frames one bucket of the level used covers: the resolution the data really has.
    /// [`PeakData::BASE_BLOCK`] when the columns are narrower than four base buckets (the caller
    /// may then draw the samples themselves), else at most a quarter of a column.
    pub frames_per_bucket: u64,
}

impl PeakData {
    /// Source frames per bucket at level 0.
    pub const BASE_BLOCK: u64 = 128;
    /// Buckets merged into one by each further level.
    pub const FACTOR: u64 = 4;

    /// Peaks of audio in memory.
    pub fn from_planar(sample_rate: u32, channels: &[Vec<f32>]) -> PeakData {
        let mut b = PeakBuilder::new(sample_rate, channels.len().max(1));
        b.push_planar(channels);
        b.finish()
    }

    /// Peaks of a decoded file.
    pub fn from_decoded(d: &Decoded) -> PeakData {
        PeakData::from_planar(d.sample_rate, &d.channels)
    }

    /// Peaks of the file `file` is open on, decoded in chunks from its current position to the end (so
    /// open it fresh, or seek to 0); never holds the whole file in memory.
    pub fn from_file(file: &mut AudioFile) -> Result<PeakData, AudioError> {
        let info = *file.info();
        let mut b = PeakBuilder::new(info.sample_rate, info.channels);
        let mut chunk = vec![0.0f32; 16 * 1024 * info.channels];
        loop {
            let n = file.read_full(&mut chunk)?;
            if n == 0 {
                break;
            }
            b.push(&chunk[..n * info.channels]);
        }
        Ok(b.finish())
    }

    /// Sample rate of the frames the peaks are counted in.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames of the file.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Channels (1 or 2).
    pub fn channels(&self) -> usize {
        self.levels.first().map_or(0, |l| l.data.len())
    }

    /// Number of levels (at least 1 for a non-empty file).
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Source frames per bucket at `level`.
    pub fn block_frames(&self, level: usize) -> Option<u64> {
        self.levels.get(level).map(|l| l.block)
    }

    /// The buckets of `channel` at `level`.
    pub fn level(&self, level: usize, channel: usize) -> Option<&[Peak]> {
        self.levels.get(level)?.data.get(channel).map(Vec::as_slice)
    }

    /// Min/max of `channel` for frames `start..end` in `columns` equal columns. Frames outside
    /// the file read as silence. An empty range or no columns gives no columns.
    pub fn query(&self, channel: usize, start: u64, end: u64, columns: usize) -> PeakColumns {
        let Some(base) = self.levels.first() else {
            return PeakColumns {
                columns: vec![Peak::SILENT; columns],
                frames_per_bucket: Self::BASE_BLOCK,
            };
        };
        if columns == 0 || end <= start || channel >= base.data.len() {
            return PeakColumns {
                columns: if end <= start {
                    Vec::new()
                } else {
                    vec![Peak::SILENT; columns]
                },
                frames_per_bucket: base.block,
            };
        }
        let per_column = (end - start) / columns as u64;
        // The coarsest level whose buckets are at most a quarter of a column wide: a column
        // merges at least four buckets and shows at most a quarter column too much at each end.
        let level = self
            .levels
            .iter()
            .rev()
            .find(|l| l.block <= per_column / 4)
            .unwrap_or(base);
        let data = &level.data[channel];
        let span = end - start;
        let out = (0..columns as u64)
            .map(|c| {
                let from = start + span * c / columns as u64;
                let to = (start + span * (c + 1) / columns as u64).max(from + 1);
                if from >= self.frames {
                    return Peak::SILENT;
                }
                let first = (from / level.block) as usize;
                let last = (to.min(self.frames).div_ceil(level.block) as usize).min(data.len());
                data[first.min(last)..last]
                    .iter()
                    .fold(Peak::EMPTY, |acc, p| acc.merge(*p))
            })
            .map(|p| if p.min > p.max { Peak::SILENT } else { p })
            .collect();
        PeakColumns {
            columns: out,
            frames_per_bucket: level.block,
        }
    }

    /// [`PeakData::query`] for a range in seconds of the file.
    pub fn query_secs(&self, channel: usize, start: f64, end: f64, columns: usize) -> PeakColumns {
        let rate = f64::from(self.sample_rate);
        let to_frame = |s: f64| (s.max(0.0) * rate).round() as u64;
        self.query(channel, to_frame(start), to_frame(end), columns)
    }

    /// Approximate memory in bytes.
    pub fn byte_size(&self) -> usize {
        self.levels
            .iter()
            .map(|l| l.data.iter().map(|c| c.len() * 8).sum::<usize>())
            .sum()
    }
}

/// Builds [`PeakData`] from audio pushed in chunks of any size.
pub struct PeakBuilder {
    sample_rate: u32,
    channels: usize,
    frames: u64,
    /// Level 0 buckets, `[channel][bucket]`.
    base: Vec<Vec<Peak>>,
    /// The running bucket, per channel, and how many frames it holds.
    current: Vec<Peak>,
    filled: u64,
}

impl PeakBuilder {
    /// A builder for `channels` channels at `sample_rate` Hz.
    pub fn new(sample_rate: u32, channels: usize) -> PeakBuilder {
        let channels = channels.max(1);
        PeakBuilder {
            sample_rate,
            channels,
            frames: 0,
            base: vec![Vec::new(); channels],
            current: vec![Peak::EMPTY; channels],
            filled: 0,
        }
    }

    /// Adds interleaved frames.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            for (cur, v) in self.current.iter_mut().zip(frame) {
                cur.min = cur.min.min(*v);
                cur.max = cur.max.max(*v);
            }
            self.advance();
        }
    }

    /// Adds planar frames (one slice per channel, equal lengths).
    pub fn push_planar<V: AsRef<[f32]>>(&mut self, planar: &[V]) {
        let frames = planar.first().map_or(0, |c| c.as_ref().len());
        for i in 0..frames {
            for (cur, plane) in self.current.iter_mut().zip(planar) {
                let v = plane.as_ref()[i];
                cur.min = cur.min.min(v);
                cur.max = cur.max.max(v);
            }
            self.advance();
        }
    }

    fn advance(&mut self) {
        self.frames += 1;
        self.filled += 1;
        if self.filled == PeakData::BASE_BLOCK {
            self.close_bucket();
        }
    }

    fn close_bucket(&mut self) {
        for (data, cur) in self.base.iter_mut().zip(&mut self.current) {
            data.push(*cur);
            *cur = Peak::EMPTY;
        }
        self.filled = 0;
    }

    /// The peaks of everything pushed.
    pub fn finish(mut self) -> PeakData {
        if self.filled > 0 {
            self.close_bucket();
        }
        let mut levels = Vec::new();
        if self.frames > 0 {
            let mut level = Level {
                block: PeakData::BASE_BLOCK,
                data: self.base,
            };
            loop {
                let buckets = level.data[0].len();
                let next = (buckets > 1).then(|| Level {
                    block: level.block * PeakData::FACTOR,
                    data: level
                        .data
                        .iter()
                        .map(|c| {
                            c.chunks(PeakData::FACTOR as usize)
                                .map(|g| g.iter().fold(Peak::EMPTY, |a, p| a.merge(*p)))
                                .collect()
                        })
                        .collect(),
                });
                levels.push(level);
                match next {
                    Some(n) => level = n,
                    None => break,
                }
            }
        }
        PeakData {
            sample_rate: self.sample_rate,
            frames: self.frames,
            levels,
        }
    }
}
