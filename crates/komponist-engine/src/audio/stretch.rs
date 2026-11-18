//! Worker-owned Warp transform. Fixed DSP chunks are buffered across heads, ring writes and
//! seamless segment boundaries; neither callback block size nor export changes the algorithm.
use super::render::SourceReader;
use super::{AudioPlan, Segment};
use komponist_stretch::{CHUNK_FRAMES, Stretcher};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpSegment {
    /// Output/input duration ratio; compiled from the clip's original and project tempo.
    pub ratio: f64,
    /// Song frame of the first segment in this source-continuous Warp chain.
    pub chain_start: i64,
}

/// A worker failure. Realtime produces a counted clip underrun; export returns an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StretchError {
    Settings,
    Input,
    Position,
    Backend,
    SourceRead,
    SourceSeek,
    SourceResample,
}
impl StretchError {
    fn code(self) -> u8 {
        match self {
            Self::Settings => 1,
            Self::Input => 2,
            Self::Position => 3,
            Self::Backend => 4,
            Self::SourceRead => 5,
            Self::SourceSeek => 6,
            Self::SourceResample => 7,
        }
    }
    fn from_code(n: u8) -> Option<Self> {
        match n {
            0 => None,
            1 => Some(Self::Settings),
            2 => Some(Self::Input),
            3 => Some(Self::Position),
            5 => Some(Self::SourceRead),
            6 => Some(Self::SourceSeek),
            7 => Some(Self::SourceResample),
            _ => Some(Self::Backend),
        }
    }
}
impl std::fmt::Display for StretchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Settings => "unsupported audio stretch settings",
            Self::Input => "invalid audio samples for stretching",
            Self::Position => "audio stretch position out of range",
            Self::Backend => "audio stretch backend failed",
            Self::SourceRead => "could not read the audio source for stretching",
            Self::SourceSeek => "could not seek in the audio source for stretching",
            Self::SourceResample => "could not resample the audio source for stretching",
        })
    }
}
impl std::error::Error for StretchError {}
impl From<komponist_stretch::Error> for StretchError {
    fn from(e: komponist_stretch::Error) -> Self {
        use komponist_stretch::Error as E;
        match e {
            E::SampleRate | E::Ratio | E::Channels => Self::Settings,
            E::Input => Self::Input,
            E::FrameLimit => Self::Position,
            _ => Self::Backend,
        }
    }
}

pub(crate) struct Diagnostics {
    errors: Box<[AtomicU8]>,
    counts: Box<[AtomicU64]>,
}
impl Diagnostics {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            errors: (0..n).map(|_| AtomicU8::new(0)).collect(),
            counts: (0..n).map(|_| AtomicU64::new(0)).collect(),
        }
    }
    pub(crate) fn fail(&self, i: usize, e: StretchError) {
        self.errors[i].store(e.code(), Ordering::Relaxed);
        self.counts[i].fetch_add(1, Ordering::Release);
    }
    pub(crate) fn count(&self, i: usize) -> u64 {
        self.counts[i].load(Ordering::Acquire)
    }
    pub(crate) fn error(&self, i: usize) -> Option<StretchError> {
        StretchError::from_code(self.errors[i].load(Ordering::Acquire))
    }
    pub(crate) fn total(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }
}

pub(crate) struct WarpState {
    source: u32,
    chain: i64,
    ratio: f64,
    next: i64,
    input_at: i64,
    stretch: Stretcher,
    input: [Vec<f32>; 2],
    output: [Vec<f32>; 2],
    used: usize,
}
impl WarpState {
    pub(crate) fn matches(&self, seg: &Segment, j: i64) -> bool {
        seg.warp
            .is_some_and(|w| w.ratio == self.ratio && w.chain_start == self.chain)
            && seg.source == self.source
            && seg.start.checked_add(j) == Some(self.next)
    }
    pub(crate) fn new(
        plan: &AudioPlan,
        seg: &Segment,
        j: i64,
        reader: &mut SourceReader,
    ) -> Result<Self, StretchError> {
        let warp = seg.warp.ok_or(StretchError::Settings)?;
        let stretch = Stretcher::new(plan.rate, 2, warp.ratio)?;
        // Starts within a cached head replay from its origin, so head → ring is identical.
        // Other locates skip at most one chunk; arbitrary seeks never render a whole prefix.
        let anchor = if j < plan.head_frames as i64 {
            0
        } else {
            j / CHUNK_FRAMES as i64 * CHUNK_FRAMES as i64
        };
        let advance = (anchor as f64 / warp.ratio).round();
        if !advance.is_finite() || advance < 0.0 || advance >= i64::MAX as f64 {
            return Err(StretchError::Position);
        }
        let input_at = seg
            .x0
            .checked_add(advance as i64)
            .ok_or(StretchError::Position)?;
        let mut state = Self {
            source: seg.source,
            chain: warp.chain_start,
            ratio: warp.ratio,
            next: seg
                .start
                .checked_add(anchor)
                .ok_or(StretchError::Position)?,
            input_at,
            stretch,
            input: [Vec::new(), Vec::new()],
            output: [vec![0.0; CHUNK_FRAMES], vec![0.0; CHUNK_FRAMES]],
            used: CHUNK_FRAMES,
        };
        state.read_source(reader, state.stretch.prime_frames())?;
        state.stretch.seek(&[&state.input[0], &state.input[1]])?;
        let mut skip = (j - anchor) as usize;
        while skip > 0 {
            state.generate(reader)?;
            let n = skip.min(CHUNK_FRAMES);
            state.used = n;
            state.next += n as i64;
            skip -= n;
        }
        Ok(state)
    }
    fn read_source(&mut self, reader: &mut SourceReader, n: usize) -> Result<(), StretchError> {
        let end = self
            .input_at
            .checked_add(n as i64)
            .ok_or(StretchError::Position)?;
        for c in &mut self.input {
            c.resize(n, 0.0);
        }
        let [l, r] = &mut self.input;
        reader.read(self.input_at, [l, r])?;
        self.input_at = end;
        Ok(())
    }
    fn generate(&mut self, reader: &mut SourceReader) -> Result<(), StretchError> {
        let n = self.stretch.input_frames_required(CHUNK_FRAMES)?;
        self.read_source(reader, n)?;
        let [l, r] = &mut self.output;
        self.stretch
            .process(&[&self.input[0], &self.input[1]], &mut [l, r])?;
        self.used = 0;
        Ok(())
    }
    pub(crate) fn render(
        &mut self,
        reader: &mut SourceReader,
        out: [&mut [f32]; 2],
    ) -> Result<(), StretchError> {
        let [l, r] = out;
        let mut at = 0;
        while at < l.len() {
            if self.used == CHUNK_FRAMES {
                self.generate(reader)?;
            }
            let n = (CHUNK_FRAMES - self.used).min(l.len() - at);
            l[at..at + n].copy_from_slice(&self.output[0][self.used..self.used + n]);
            r[at..at + n].copy_from_slice(&self.output[1][self.used..self.used + n]);
            self.used += n;
            at += n;
            self.next = self
                .next
                .checked_add(n as i64)
                .ok_or(StretchError::Position)?;
        }
        Ok(())
    }
}
