//! The waveform of an audio clip in the arrangement (audio proposal `SPEC.md` §3.1, D1 A): a
//! mirrored envelope, the peak envelope in `track.wave-peak` and a lighter core in `track.wave-rms`.
//!
//! A [`WaveMesh`] is built once per (clip `Arc` pointer, zoom, body height) from the file's peak
//! pyramid ([`PeakData::query`]) and kept by the caller, exactly like the [`super::PreviewMesh`] of
//! a MIDI clip: it holds one small column per point of clip width, so drawing costs the visible
//! width, not the file's length, and an edited clip (a new `Arc`) rebuilds by itself. The clip
//! gain and the fades are applied in the mesh, so the waveform shows them at once and parts above
//! 0 dBFS get a `meter.clip` tip.
//!
//! The inner envelope uses the measured RMS energy from the peak pyramid.

use egui::{Color32, Rangef, Rect, pos2};
use komponist_audiofile::PeakData;

use super::mesh::Quads;
use crate::widgets::metrics::timeline as metrics;

/// Most columns one mesh holds (a clip wider than this many points shows its first part
/// correctly scaled and cuts the rest; 32 768 points are 450 bars at the default zoom).
pub const MAX_COLUMNS: usize = 32_768;

/// What a [`WaveMesh`] is built for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveSpec {
    /// Width of the clip in points (`length × px_per_tick`).
    pub width: f32,
    /// Horizontal zoom in points per tick (the mesh is rebuilt when it changes).
    pub px_per_tick: f32,
    /// Body height in points ([`super::clip::body_height`]).
    pub body_height: f32,
    /// First frame of the window in the file (the file's own rate).
    pub offset: u64,
    /// Frames of the file the window shows.
    pub frames: u64,
    /// Linear gain of the clip (`10^(dB/20)`).
    pub gain: f32,
    /// Fade-in length in points.
    pub fade_in: f32,
    /// Fade-out length in points.
    pub fade_out: f32,
}

/// One point column: extents above and below the centre line in whole points, the core's share
/// of them, and whether the gained peak passes 0 dBFS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Col {
    up: i16,
    down: i16,
    core_up: i16,
    core_down: i16,
    over: bool,
}

/// The laid-out waveform of one clip at one zoom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaveMesh {
    cols: Vec<Col>,
    width: i32,
    px_per_tick: f32,
    body_height: f32,
}

impl WaveMesh {
    /// A mesh without data: the flat centre line of a clip whose peaks are not ready (or whose
    /// file is missing).
    pub fn flat(width: f32, px_per_tick: f32, body_height: f32) -> Self {
        Self {
            cols: Vec::new(),
            width: width.round().max(0.0) as i32,
            px_per_tick,
            body_height,
        }
    }

    /// The waveform of `peaks` for `spec`.
    pub fn from_peaks(peaks: &PeakData, spec: &WaveSpec) -> Self {
        let mut mesh = Self::flat(spec.width, spec.px_per_tick, spec.body_height);
        let columns = (mesh.width.max(0) as usize).min(MAX_COLUMNS);
        if columns == 0 || spec.frames == 0 {
            return mesh;
        }
        let frames =
            (spec.frames as f64 * columns as f64 / f64::from(spec.width.max(1.0))).ceil() as u64;
        let end = spec.offset.saturating_add(frames);
        let mut peak = vec![(0.0f32, 0.0f32); columns];
        let mut rms = vec![0.0f32; columns];
        for channel in 0..peaks.channels().min(2) {
            let q = peaks.query(channel, spec.offset, end, columns);
            for (slot, value) in rms
                .iter_mut()
                .zip(peaks.query_rms(channel, spec.offset, end, columns).columns)
            {
                *slot = slot.max(value);
            }
            for (slot, p) in peak.iter_mut().zip(&q.columns) {
                slot.0 = slot.0.min(p.min);
                slot.1 = slot.1.max(p.max);
            }
        }
        let half = ((spec.body_height - 2.0 * metrics::WAVE_PAD) / 2.0).max(0.0);
        let scale = |v: f32| (v * half).round().min(f32::from(i16::MAX)) as i16;
        mesh.cols = peak
            .iter()
            .enumerate()
            .map(|(i, &(lo, hi))| {
                let x = i as f32 + 0.5;
                let mut f = 1.0;
                if spec.fade_in > 0.0 {
                    f *= (x / spec.fade_in).min(1.0);
                }
                if spec.fade_out > 0.0 {
                    f *= ((spec.width - x) / spec.fade_out).clamp(0.0, 1.0);
                }
                let k = spec.gain * f;
                let (up, down) = (hi.max(0.0) * k, (-lo).max(0.0) * k);
                Col {
                    up: scale(up.min(1.0)),
                    down: scale(down.min(1.0)),
                    core_up: scale((rms[i] * k).min(up).min(1.0)),
                    core_down: scale((rms[i] * k).min(down).min(1.0)),
                    over: up > 1.0 || down > 1.0,
                }
            })
            .collect();
        mesh.smooth_core();
        mesh
    }

    /// The core follows the envelope with a short average, so it reads as a body and not as a
    /// copy of the peaks' outline.
    fn smooth_core(&mut self) {
        let n = self.cols.len();
        if n < 3 {
            return;
        }
        let src = self.cols.clone();
        for i in 0..n {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
            let avg = |f: fn(&Col) -> i16| {
                let sum: i32 = (a..=b).map(|j| i32::from(f(&src[j]))).sum();
                (sum / (b - a + 1) as i32) as i16
            };
            self.cols[i].core_up = avg(|c| c.core_up).min(self.cols[i].up);
            self.cols[i].core_down = avg(|c| c.core_down).min(self.cols[i].down);
        }
    }

    /// Number of columns (0 for a flat mesh).
    pub fn len(&self) -> usize {
        self.cols.len()
    }

    /// Whether there is no data: only the centre line is drawn.
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    /// The zoom it was built for.
    pub fn px_per_tick(&self) -> f32 {
        self.px_per_tick
    }

    /// Whether it was built for this zoom and body height (else rebuild).
    pub fn fits(&self, px_per_tick: f32, body_height: f32) -> bool {
        self.px_per_tick == px_per_tick && self.body_height == body_height
    }

    /// The peak extent `(up, down)` of column `i` in points, for tests.
    pub fn extent(&self, i: usize) -> Option<(i16, i16)> {
        self.cols.get(i).map(|c| (c.up, c.down))
    }

    /// Whether column `i` passes 0 dBFS, for tests.
    pub fn is_over(&self, i: usize) -> bool {
        self.cols.get(i).is_some_and(|c| c.over)
    }

    /// Adds the waveform into `q`, centred in `body` and cut to `body` and `visible` (screen x).
    /// Runs of equal columns become one quad.
    pub(super) fn paint_into(
        &self,
        q: &mut Quads,
        body: Rect,
        visible: Rangef,
        peak: Color32,
        core: Color32,
        over: Color32,
    ) {
        let mid = (body.center().y).round();
        let (bx0, bx1) = (body.min.x, body.max.x);
        let (lo, hi) = (bx0.max(visible.min), bx1.min(visible.max));
        if lo >= hi {
            return;
        }
        if self.cols.is_empty() {
            q.quad(Rect::from_min_max(pos2(lo, mid), pos2(hi, mid + 1.0)), peak);
            return;
        }
        let first = ((lo - bx0).floor().max(0.0)) as usize;
        let last = (((hi - bx0).ceil()) as usize).min(self.cols.len());
        let mut i = first;
        while i < last {
            let c = self.cols[i];
            let mut j = i + 1;
            while j < last && self.cols[j] == c {
                j += 1;
            }
            let x0 = (bx0 + i as f32).max(lo);
            let x1 = (bx0 + j as f32).min(hi);
            let (up, down) = (f32::from(c.up), f32::from(c.down));
            let col = |q: &mut Quads, u: f32, d: f32, color: Color32| {
                q.quad(
                    Rect::from_min_max(pos2(x0, mid - u), pos2(x1, mid + d.max(1.0 - u.min(1.0)))),
                    color,
                );
            };
            col(q, up, down, peak);
            if c.core_up > 0 || c.core_down > 0 {
                col(q, f32::from(c.core_up), f32::from(c.core_down), core);
            }
            if c.over {
                let tip = metrics::WAVE_OVER_TIP;
                q.quad(
                    Rect::from_min_max(pos2(x0, mid - up), pos2(x1, mid - (up - tip).max(0.0))),
                    over,
                );
                q.quad(
                    Rect::from_min_max(pos2(x0, mid + down - tip), pos2(x1, mid + down)),
                    over,
                );
            }
            i = j;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono file of `frames` frames at 48 kHz with a constant peak of ±`level`.
    fn peaks(frames: usize, level: f32) -> PeakData {
        let samples: Vec<f32> = (0..frames)
            .map(|i| if i % 2 == 0 { level } else { -level })
            .collect();
        PeakData::from_planar(48_000, &[samples])
    }

    fn spec(width: f32, frames: u64) -> WaveSpec {
        WaveSpec {
            width,
            px_per_tick: width / 3840.0,
            body_height: 36.0,
            offset: 0,
            frames,
            gain: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }

    #[test]
    fn full_scale_fills_the_body_without_touching_its_edges() {
        let p = peaks(96_000, 1.0);
        let m = WaveMesh::from_peaks(&p, &spec(72.0, 96_000));
        assert_eq!(m.len(), 72);
        // 36 − 2·2 = 32 → 16 points up and down.
        assert_eq!(m.extent(10), Some((16, 16)));
        assert!(!m.is_over(10));
    }

    #[test]
    fn gain_scales_and_marks_what_passes_zero_dbfs() {
        let p = peaks(96_000, 0.5);
        let mut s = spec(72.0, 96_000);
        s.gain = 2.0;
        let m = WaveMesh::from_peaks(&p, &s);
        assert_eq!(m.extent(10), Some((16, 16)));
        assert!(!m.is_over(10), "exactly 0 dBFS is not over");
        s.gain = 4.0;
        let m = WaveMesh::from_peaks(&p, &s);
        assert!(m.is_over(10));
        assert_eq!(m.extent(10), Some((16, 16)), "capped at the body");
    }

    #[test]
    fn fades_shape_the_envelope() {
        let p = peaks(96_000, 1.0);
        let mut s = spec(72.0, 96_000);
        s.fade_in = 36.0;
        s.fade_out = 36.0;
        let m = WaveMesh::from_peaks(&p, &s);
        let (a, _) = m.extent(0).unwrap();
        let (mid, _) = m.extent(36).unwrap();
        let (end, _) = m.extent(71).unwrap();
        assert!(a <= 1, "starts silent: {a}");
        assert!(end <= 1, "ends silent: {end}");
        assert!(mid >= 14, "the join of the fades is the loudest: {mid}");
    }

    #[test]
    fn the_window_follows_the_offset_and_the_file_end_is_silent() {
        // Loud first half, silent second half.
        let mut samples = vec![1.0f32; 48_000];
        samples.extend(vec![0.0; 48_000]);
        let p = PeakData::from_planar(48_000, &[samples]);
        let mut s = spec(40.0, 48_000);
        s.offset = 24_000;
        let m = WaveMesh::from_peaks(&p, &s);
        assert!(m.extent(2).unwrap().0 > 8, "still in the loud part");
        assert_eq!(m.extent(38).unwrap().0, 0, "behind the loud part");
        // A window that reaches past the end reads silence there.
        s.offset = 90_000;
        let m = WaveMesh::from_peaks(&p, &s);
        assert_eq!(m.extent(30).unwrap(), (0, 0));
    }

    #[test]
    fn a_mesh_is_cut_to_the_zoom_it_was_built_for() {
        let m = WaveMesh::flat(72.0, 0.01, 36.0);
        assert!(m.is_empty());
        assert!(m.fits(0.01, 36.0));
        assert!(!m.fits(0.02, 36.0));
    }

    #[test]
    fn capped_mesh_keeps_source_scale_and_rms_never_exceeds_local_peak() {
        let mut samples = vec![0.0; 131072];
        samples[100000] = 1.0;
        let p = PeakData::from_planar(48000, &[samples]);
        let m = WaveMesh::from_peaks(&p, &spec(65536.0, 131072));
        assert_eq!(m.len(), MAX_COLUMNS);
        assert!(
            m.cols.iter().all(|c| c.up == 0 && c.down == 0),
            "late impulse must not be compressed into the capped first half"
        );
        let p = PeakData::from_planar(
            48000,
            &[vec![1.0; 1000].into_iter().chain(vec![0.0; 1000]).collect()],
        );
        let m = WaveMesh::from_peaks(&p, &spec(40.0, 2000));
        assert!(
            m.cols
                .iter()
                .all(|c| c.core_up <= c.up && c.core_down <= c.down)
        );
    }

    #[test]
    fn equal_columns_merge_when_painting() {
        let p = peaks(96_000, 0.5);
        let m = WaveMesh::from_peaks(&p, &spec(72.0, 96_000));
        let mut q = Quads::new(1.0);
        let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(72.0, 36.0));
        m.paint_into(
            &mut q,
            body,
            Rangef::new(0.0, 72.0),
            Color32::WHITE,
            Color32::GRAY,
            Color32::RED,
        );
        assert!(
            q.mesh.indices.len() <= 6 * 3,
            "{} indices",
            q.mesh.indices.len()
        );
    }
}
