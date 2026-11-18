//! Rendering of segments: file → engine rate → speed/Warp → gain and fades (streamer thread; allocates
//! and does file IO, never on the audio thread).
//!
//! The source of a segment is the **file resampled to the engine rate**, an infinite stream with
//! zeros before the start and behind the end of the file. [`SourceReader`] produces it from any
//! position: it seeks before the position (a pre-roll aligned to the rate ratio so the converter's
//! phase is right) and decodes forward, so a seek, a locate and a continuous play give the same
//! frames. A Fixed clip copies the stream; a Re-Pitch clip reads it at `speed` frames per output
//! frame with a four-point cubic (Catmull-Rom) interpolation. Warp uses fixed buffered
//! Signalsmith chunks, with priming/latency compensation before clip gain and fades.

use komponist_audiofile::{AudioFile, PoolFile, StreamResampler};

use super::stretch::{WarpFailure, WarpState};
use super::{AudioPlan, Segment, StretchError};

/// File frames per decoder read.
const READ_FRAMES: usize = 4096;
/// File frames a seek lands before its target when converting, so the converter's start-up
/// transient is over (rubato's FFT resampler has a delay of a few hundred frames).
const PREROLL: u64 = 8192;
/// A request this far behind the window (engine frames) seeks instead of decoding through.
const RESEEK_GAP: u64 = 1 << 16;
/// Frames kept behind the last read position (the interpolation reaches back).
const KEEP: u64 = 16;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The file at the engine rate, readable from any frame. See the module docs.
pub(crate) struct SourceReader {
    file: AudioFile,
    conv: Option<StreamResampler>,
    from: u64,
    to: u64,
    channels: usize,
    /// File frames a seek target is a multiple of (`from / gcd(from, to)`), so that its engine-rate
    /// frame is a whole number.
    align: u64,
    win: [Vec<f32>; 2],
    /// Engine-rate frame of `win[0][0]`.
    win_start: u64,
    eof: bool,
    inter: Vec<f32>,
    plane_in: Vec<Vec<f32>>,
    plane_out: Vec<Vec<f32>>,
}

impl SourceReader {
    /// Opens a decoder of `file` converting to `rate`.
    pub(crate) fn open(file: &PoolFile, rate: u32) -> Result<SourceReader, StretchError> {
        let decoder = AudioFile::open(file.path()).map_err(|_| StretchError::SourceRead)?;
        let info = *decoder.info();
        let channels = info.channels;
        let conv = if info.sample_rate == rate {
            None
        } else {
            Some(
                StreamResampler::new(info.sample_rate, rate, channels)
                    .map_err(|_| StretchError::SourceResample)?,
            )
        };
        let (from, to) = (u64::from(info.sample_rate), u64::from(rate));
        Ok(SourceReader {
            file: decoder,
            conv,
            from,
            to,
            channels,
            align: from / gcd(from, to),
            win: [Vec::new(), Vec::new()],
            win_start: 0,
            eof: false,
            inter: vec![0.0; READ_FRAMES * channels],
            plane_in: vec![Vec::new(); channels],
            plane_out: vec![Vec::new(); channels],
        })
    }

    /// Frames `q0 .. q0 + out.len()` of the engine-rate stream (zeros outside the file) into
    /// `out`.
    pub(crate) fn read(&mut self, q0: i64, out: [&mut [f32]; 2]) -> Result<(), StretchError> {
        let n = out[0].len();
        let lo = q0.max(0) as u64;
        let hi = (q0 + n as i64).max(0) as u64;
        let filled = if hi > lo { self.fill(lo, hi) } else { Ok(()) };
        let [out_l, out_r] = out;
        for (k, (l, r)) in out_l.iter_mut().zip(out_r.iter_mut()).enumerate() {
            let q = q0 + k as i64;
            let at = if q < 0 {
                usize::MAX
            } else {
                (q as u64).wrapping_sub(self.win_start) as usize
            };
            if at < self.win[0].len() {
                *l = self.win[0][at];
                *r = self.win[1][at];
            } else {
                *l = 0.0;
                *r = 0.0;
            }
        }
        let keep_from = hi.saturating_sub(KEEP);
        if keep_from > self.win_start {
            let drop = ((keep_from - self.win_start) as usize).min(self.win[0].len());
            self.win[0].drain(..drop);
            self.win[1].drain(..drop);
            self.win_start += drop as u64;
        }
        filled
    }

    /// Makes the window hold `lo .. hi` (as far as the file reaches).
    fn fill(&mut self, lo: u64, hi: u64) -> Result<(), StretchError> {
        let end = self.win_start + self.win[0].len() as u64;
        if lo < self.win_start || lo > end + RESEEK_GAP {
            self.seek(lo)?;
        }
        while self.win_start + (self.win[0].len() as u64) < hi && !self.eof {
            self.pump()?;
        }
        Ok(())
    }

    fn seek(&mut self, q: u64) -> Result<(), StretchError> {
        let f = (u128::from(q) * u128::from(self.from) / u128::from(self.to)) as u64;
        let f_seek = if self.conv.is_none() {
            f
        } else {
            f.saturating_sub(PREROLL) / self.align * self.align
        };
        let q_seek = (u128::from(f_seek) * u128::from(self.to) / u128::from(self.from)) as u64;
        self.win[0].clear();
        self.win[1].clear();
        self.win_start = q_seek;
        let result = self.file.seek(f_seek).map_err(|_| StretchError::SourceSeek);
        self.eof = result.is_err();
        if let Some(conv) = &mut self.conv {
            conv.reset();
        }
        result
    }

    /// Decodes and converts the next chunk into the window.
    fn pump(&mut self) -> Result<(), StretchError> {
        let channels = self.channels;
        let read = self.file.read(&mut self.inter);
        let n = read.as_ref().copied().unwrap_or(0);
        for plane in self.plane_out.iter_mut().chain(self.plane_in.iter_mut()) {
            plane.clear();
        }
        if n == 0 {
            self.eof = true;
            let finish = if let Some(conv) = &mut self.conv {
                let result = conv
                    .finish(&mut self.plane_out)
                    .map_err(|_| StretchError::SourceResample);
                self.append_out();
                result
            } else {
                Ok(())
            };
            return read.map_err(|_| StretchError::SourceRead).and(finish);
        }
        for frame in self.inter[..n * channels].chunks_exact(channels) {
            for (plane, v) in self.plane_in.iter_mut().zip(frame) {
                plane.push(*v);
            }
        }
        match &mut self.conv {
            Some(conv) => {
                let result = conv
                    .push(&self.plane_in, &mut self.plane_out)
                    .map_err(|_| StretchError::SourceResample);
                self.eof = result.is_err();
                self.append_out();
                result
            }
            None => {
                self.win[0].extend_from_slice(&self.plane_in[0]);
                let right = self.plane_in.get(1).unwrap_or(&self.plane_in[0]);
                self.win[1].extend_from_slice(right);
                Ok(())
            }
        }
    }

    fn append_out(&mut self) {
        self.win[0].extend_from_slice(&self.plane_out[0]);
        let right = self.plane_out.get(1).unwrap_or(&self.plane_out[0]);
        self.win[1].extend_from_slice(right);
    }
}

/// Renders segments of a plan; keeps the decoder of the file it is reading, so consecutive
/// frames of one segment (and of a seamless continuation in the next) decode once.
pub(crate) struct SegmentRenderer {
    current: Option<(u32, SourceReader)>,
    scratch: [Vec<f32>; 2],
    warp: Option<WarpState>,
}

impl SegmentRenderer {
    pub(crate) fn new() -> SegmentRenderer {
        SegmentRenderer {
            current: None,
            scratch: [Vec::new(), Vec::new()],
            warp: None,
        }
    }

    /// Forgets the open decoder (a new epoch or plan).
    pub(crate) fn reset(&mut self) {
        self.current = None;
        self.warp = None;
    }
    /// A transport jump discards DSP history/overhang on the worker, retaining the decoder.
    pub(crate) fn restart(&mut self) {
        self.warp = None;
    }

    pub(super) fn continues_warp(&self, seg: &Segment, j0: i64) -> bool {
        self.warp.as_ref().is_some_and(|w| w.matches(seg, j0))
    }

    /// Renders frames `j0 .. j0 + out.len()` of segment `i` of `plan` (clip gain, fades and edge
    /// fades applied) into `out`, which it overwrites. A file that is not there gives silence.
    pub(crate) fn render(
        &mut self,
        plan: &AudioPlan,
        i: usize,
        j0: i64,
        out: [&mut [f32]; 2],
    ) -> Result<(), WarpFailure> {
        let seg = &plan.segments[i];
        let n = out[0].len();
        match self.select(plan, seg) {
            Ok(true) => {}
            Err(error) if seg.warp.is_some() => {
                self.warp = None;
                plan.diagnostics.fail(i, error);
                return Err(error.into());
            }
            Ok(false) | Err(_) => {
                out[0].fill(0.0);
                out[1].fill(0.0);
                self.warp = None;
                return Ok(());
            }
        }
        let continues = self.continues_warp(seg, j0);
        let reader = &mut self.current.as_mut().expect("selected").1;
        let [out_l, out_r] = out;
        let mut failure = None;
        if seg.warp.is_some() {
            let result = (|| {
                if !continues {
                    self.warp = None;
                    self.warp = Some(WarpState::new(plan, seg, j0, reader)?);
                }
                self.warp
                    .as_mut()
                    .expect("initialized")
                    .render(reader, [&mut *out_l, &mut *out_r])
            })();
            if let Err(error) = result {
                self.warp = None;
                if matches!(
                    error.reason,
                    StretchError::SourceRead
                        | StretchError::SourceSeek
                        | StretchError::SourceResample
                ) {
                    self.current = None;
                }
                plan.diagnostics.fail(i, error.reason);
                failure = Some(error);
            }
        } else if seg.speed == 1.0 {
            self.warp = None;
            // Fixed and Re-Pitch retain the existing best-effort silence on source errors.
            let _ = reader.read(seg.x0 + j0, [&mut *out_l, &mut *out_r]);
        } else {
            self.warp = None;
            let pos = |k: i64| seg.x0 as f64 + (j0 + k) as f64 * seg.speed;
            let first = pos(0).floor() as i64 - 1;
            let last = pos(n as i64 - 1).floor() as i64 + 2;
            let len = (last - first + 1) as usize;
            for plane in &mut self.scratch {
                plane.clear();
                plane.resize(len, 0.0);
            }
            let [src_l, src_r] = &mut self.scratch;
            let _ = reader.read(first, [&mut src_l[..], &mut src_r[..]]);
            for (k, (l, r)) in out_l.iter_mut().zip(out_r.iter_mut()).enumerate() {
                let x = pos(k as i64);
                let at = (x.floor() as i64 - first) as usize;
                let t = (x - x.floor()) as f32;
                *l = cubic(&src_l[at - 1..=at + 2], t);
                *r = cubic(&src_r[at - 1..=at + 2], t);
            }
        }
        let rendered = failure.map_or(n, |error| error.rendered);
        for (k, (l, r)) in out_l[..rendered]
            .iter_mut()
            .zip(&mut out_r[..rendered])
            .enumerate()
        {
            let g = envelope(seg, j0 + k as i64, plan.edge_frames);
            *l *= g;
            *r *= g;
        }
        failure.map_or(Ok(()), Err)
    }

    /// Selects the segment's decoder. Live unresolved sources keep the existing silence path.
    fn select(&mut self, plan: &AudioPlan, seg: &Segment) -> Result<bool, StretchError> {
        if self.current.as_ref().is_some_and(|(s, _)| *s == seg.source) {
            return Ok(true);
        }
        self.current = None;
        let Some(file) = plan.sources[seg.source as usize].open() else {
            return if plan.offline {
                Err(StretchError::SourceRead)
            } else {
                Ok(false)
            };
        };
        self.current = Some((seg.source, SourceReader::open(&file, plan.rate)?));
        Ok(true)
    }
}

/// Catmull-Rom interpolation between `p[1]` and `p[2]` at `t` in `0 .. 1`.
fn cubic(p: &[f32], t: f32) -> f32 {
    let (p0, p1, p2, p3) = (p[0], p[1], p[2], p[3]);
    0.5 * (2.0 * p1
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t)
}

/// The gain of segment frame `j`: the clip gain, the clip's linear fades and the `edge` frames
/// fade at the borders that have one. Fade-in frame `j` has gain `j / length` (0 at the first
/// frame), fade-out frame `j` has `(len − 1 − j) / length` (0 at the last frame).
pub(crate) fn envelope(seg: &Segment, j: i64, edge: u32) -> f32 {
    let from_end = seg.len() - 1 - j;
    let mut gain = seg.gain;
    if j < i64::from(seg.fade_in) {
        gain *= j as f32 / seg.fade_in as f32;
    }
    if from_end < i64::from(seg.fade_out) {
        gain *= from_end as f32 / seg.fade_out as f32;
    }
    let edge_len = edge as f32;
    if seg.edge_in && j < i64::from(edge) {
        gain *= j as f32 / edge_len;
    }
    if seg.edge_out && from_end < i64::from(edge) {
        gain *= from_end as f32 / edge_len;
    }
    gain
}

/// The head of segment `i` of `plan`: its first `min(head frames, length)` frames.
pub(crate) fn render_head(plan: &AudioPlan, i: usize) -> [Box<[f32]>; 2] {
    let n = usize::try_from(plan.segments[i].len())
        .unwrap_or(0)
        .min(plan.head_frames);
    let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
    if let Err(error) = SegmentRenderer::new().render(plan, i, 0, [&mut l, &mut r]) {
        l.truncate(error.rendered);
        r.truncate(error.rendered);
    }
    [l.into(), r.into()]
}
