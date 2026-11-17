//! The waveform of an audio clip in the arrangement (audio proposal `SPEC.md` §3.1, D1 A): a
//! mirrored envelope, the peak envelope in `track.wave-peak` and a lighter core in `track.wave-rms`.
//!
//! A [`WaveMesh`] is built once per (clip `Arc` pointer, zoom, body height) from the file's peak
//! pyramid ([`PeakData::query`]) and kept by the caller, exactly like the [`super::PreviewMesh`] of
//! a MIDI clip: it holds subpixel envelope samples along the clip width, so drawing costs the visible
//! width, not the file's length, and an edited clip (a new `Arc`) rebuilds by itself. The clip
//! gain and the fades are applied in the mesh, so the waveform shows them at once and parts above
//! 0 dBFS get a `meter.clip` tip.
//!
//! The inner envelope uses the measured RMS energy from the peak pyramid.

use egui::{Color32, Rangef, Rect, pos2};
use komponist_audiofile::PeakData;

use super::mesh::Quads;
use crate::widgets::metrics::timeline as metrics;

/// Most columns one mesh holds. Wider clips cover their full source window with coarser
/// columns, keeping memory bounded without hiding the end of a long recording.
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

/// One envelope sample: fractional-point peak/RMS extents above and below the centre line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Col {
    up: f32,
    down: f32,
    core_up: f32,
    core_down: f32,
    over: bool,
}

/// The laid-out waveform of one clip at one zoom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaveMesh {
    cols: Vec<Col>,
    width: i32,
    column_width: f32,
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
            column_width: 1.0,
            px_per_tick,
            body_height,
        }
    }

    /// The waveform of `peaks` for `spec`.
    pub fn from_peaks(peaks: &PeakData, spec: &WaveSpec) -> Self {
        Self::from_channels(peaks, spec, 0..peaks.channels().min(2))
    }

    /// The waveform of one source channel, for separate stereo-editor lanes.
    pub fn from_channel(peaks: &PeakData, spec: &WaveSpec, channel: usize) -> Self {
        Self::from_channels(
            peaks,
            spec,
            channel..channel.saturating_add(1).min(peaks.channels()),
        )
    }

    fn from_channels(peaks: &PeakData, spec: &WaveSpec, channels: std::ops::Range<usize>) -> Self {
        let mut mesh = Self::flat(spec.width, spec.px_per_tick, spec.body_height);
        let columns = ((mesh.width.max(0) as f32 * metrics::WAVE_SAMPLES_PER_POINT).ceil()
            as usize)
            .min(MAX_COLUMNS);
        if columns == 0 || spec.frames == 0 {
            return mesh;
        }
        mesh.column_width = mesh.width as f32 / columns as f32;
        let end = spec.offset.saturating_add(spec.frames);
        let mut peak = vec![(0.0f32, 0.0f32); columns];
        let mut rms = vec![0.0f32; columns];
        for channel in channels {
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
        let scale = |v: f32| v * half;
        mesh.cols = peak
            .iter()
            .enumerate()
            .map(|(i, &(lo, hi))| {
                let x = (i as f32 + 0.5) * mesh.column_width;
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
        mesh
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
        self.cols
            .get(i)
            .map(|c| (c.up.round() as i16, c.down.round() as i16))
    }

    /// Whether column `i` passes 0 dBFS, for tests.
    pub fn is_over(&self, i: usize) -> bool {
        self.cols.get(i).is_some_and(|c| c.over)
    }

    /// Paints a cached waveform in a source-editor lane, clipped to the viewport.
    pub fn paint(
        &self,
        painter: &egui::Painter,
        body: Rect,
        peak: Color32,
        core: Color32,
        over: Color32,
    ) {
        let mut q = Quads::new(painter.pixels_per_point());
        self.paint_into(
            &mut q,
            body,
            painter.clip_rect().x_range(),
            peak,
            core,
            over,
        );
        q.paint(painter);
    }

    /// Interpolates adjacent measured samples without overshoot or temporal smoothing.
    fn at(&self, x: f32) -> Col {
        let position = (x / self.column_width - 0.5).clamp(0.0, (self.cols.len() - 1) as f32);
        let i = position.floor() as usize;
        let j = (i + 1).min(self.cols.len() - 1);
        let t = position - i as f32;
        let (a, b) = (self.cols[i], self.cols[j]);
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Col {
            up: mix(a.up, b.up),
            down: mix(a.down, b.down),
            core_up: mix(a.core_up, b.core_up),
            core_down: mix(a.core_down, b.core_down),
            over: a.over || b.over,
        }
    }

    /// Continuous peak/RMS ribbons in one mesh, with a one-device-pixel alpha feather on
    /// each envelope boundary. Samples remain fractional points at every display scale.
    pub(super) fn paint_into(
        &self,
        q: &mut Quads,
        body: Rect,
        visible: Rangef,
        peak: Color32,
        core: Color32,
        over: Color32,
    ) {
        let mid = body.center().y;
        let (lo, hi) = (body.left().max(visible.min), body.right().min(visible.max));
        if lo >= hi {
            return;
        }
        if self.cols.is_empty() {
            q.quad(
                Rect::from_min_max(pos2(lo, mid), pos2(hi, mid + 1.0 / q.pixels_per_point())),
                peak,
            );
            return;
        }
        let start = (((lo - body.left()) / self.column_width - 0.5)
            .ceil()
            .max(0.0)) as usize;
        let end = (((hi - body.left()) / self.column_width - 0.5)
            .floor()
            .max(0.0) as usize)
            .min(self.cols.len() - 1);
        let mut points: Vec<(f32, Col)> =
            Vec::with_capacity(end.saturating_sub(start).saturating_add(3));
        let mut push = |x: f32, c: Col| {
            let n = points.len();
            // Constant material needs only its end points, keeping long flat clips cheap.
            if n >= 2 && points[n - 1].1 == c && points[n - 2].1 == c {
                points[n - 1] = (x, c);
            } else {
                points.push((x, c));
            }
        };
        push(lo, self.at(lo - body.left()));
        for i in start..=end {
            let x = body.left() + (i as f32 + 0.5) * self.column_width;
            if x > lo && x < hi {
                push(x, self.cols[i]);
            }
        }
        push(hi, self.at(hi - body.left()));
        let feather = 1.0 / q.pixels_per_point();
        ribbon(q, &points, mid, feather, peak, false);
        if points
            .iter()
            .any(|(_, c)| c.core_up > 0.0 || c.core_down > 0.0)
        {
            ribbon(q, &points, mid, feather, core, true);
        }
        for pair in points.windows(2) {
            let [(x0, a), (x1, b)] = pair else {
                unreachable!()
            };
            if a.over || b.over {
                let up = a.up.max(b.up);
                let down = a.down.max(b.down);
                let tip = metrics::WAVE_OVER_TIP;
                q.quad(
                    Rect::from_min_max(pos2(*x0, mid - up), pos2(*x1, mid - (up - tip).max(0.0))),
                    over,
                );
                q.quad(
                    Rect::from_min_max(
                        pos2(*x0, mid + (down - tip).max(0.0)),
                        pos2(*x1, mid + down),
                    ),
                    over,
                );
            }
        }
    }
}

/// One strip per envelope: outer transparent edges, inner opaque fill; neighbouring samples
/// share vertices. Peak samples are never averaged, so short transients retain their height.
fn ribbon(
    q: &mut Quads,
    points: &[(f32, Col)],
    mid: f32,
    feather: f32,
    color: Color32,
    core: bool,
) {
    let base = q.mesh.vertices.len() as u32;
    q.mesh.reserve_vertices(points.len() * 4);
    q.mesh.reserve_triangles(points.len().saturating_sub(1) * 6);
    let contour = |(x, c): (f32, Col)| {
        let (up, down) = if core {
            (c.core_up, c.core_down)
        } else {
            (c.up, c.down)
        };
        (
            pos2(x, mid - up),
            pos2(x, (mid + down).max(mid - up + feather)),
        )
    };
    let lo = points.first().unwrap().0;
    let hi = points.last().unwrap().0;
    for (i, &point) in points.iter().enumerate() {
        let (top, bottom) = contour(point);
        let (prev_top, prev_bottom) = contour(points[i.saturating_sub(1)]);
        let (next_top, next_bottom) = contour(points[(i + 1).min(points.len() - 1)]);
        let normal = |a: egui::Pos2, b: egui::Pos2, top: bool| {
            let d = b - a;
            let n = if top {
                egui::vec2(d.y, -d.x)
            } else {
                egui::vec2(-d.y, d.x)
            };
            n.normalized() * feather
        };
        let mut top_outer = top + normal(prev_top, next_top, true);
        let mut bottom_outer = bottom + normal(prev_bottom, next_bottom, false);
        top_outer.x = top_outer.x.clamp(lo, hi);
        bottom_outer.x = bottom_outer.x.clamp(lo, hi);
        for (pos, tint) in [
            (top_outer, Color32::TRANSPARENT),
            (top, color),
            (bottom, color),
            (bottom_outer, Color32::TRANSPARENT),
        ] {
            q.mesh.colored_vertex(pos, tint);
        }
    }
    for i in 0..points.len().saturating_sub(1) {
        let a = base + i as u32 * 4;
        let b = a + 4;
        for j in 0..3 {
            q.mesh.add_triangle(a + j, a + j + 1, b + j);
            q.mesh.add_triangle(a + j + 1, b + j + 1, b + j);
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
        assert_eq!(m.len(), 144);
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
        let (mid, _) = m.extent(72).unwrap();
        let (end, _) = m.extent(143).unwrap();
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
        assert_eq!(m.extent(76).unwrap().0, 0, "behind the loud part");
        // A window that reaches past the end reads silence there.
        s.offset = 90_000;
        let m = WaveMesh::from_peaks(&p, &s);
        assert_eq!(m.extent(60).unwrap(), (0, 0));
    }

    #[test]
    fn a_mesh_is_cut_to_the_zoom_it_was_built_for() {
        let m = WaveMesh::flat(72.0, 0.01, 36.0);
        assert!(m.is_empty());
        assert!(m.fits(0.01, 36.0));
        assert!(!m.fits(0.02, 36.0));
    }

    #[test]
    fn capped_mesh_covers_the_whole_source_at_its_timeline_position() {
        let mut samples = vec![0.0; 131072];
        samples[100000] = 1.0;
        let p = PeakData::from_planar(48000, &[samples]);
        let m = WaveMesh::from_peaks(&p, &spec(65536.0, 131072));
        assert_eq!(m.len(), MAX_COLUMNS);
        assert_eq!(m.column_width, 2.0);
        assert!(
            m.cols[..MAX_COLUMNS / 2].iter().all(|c| c.up == 0.0),
            "late transient must not move into the first half"
        );
        let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(65536.0, 36.0));
        let visible = Rangef::new(49980.0, 50080.0);
        let mut q = Quads::new(1.0);
        m.paint_into(
            &mut q,
            body,
            visible,
            Color32::WHITE,
            Color32::GRAY,
            Color32::RED,
        );
        assert!(
            q.mesh
                .vertices
                .iter()
                .any(|v| v.pos.y < 18.0 && (v.pos.x - 50000.0).abs() < 80.0),
            "second-half transient must be painted at its full-clip location"
        );
        assert!(
            q.mesh.vertices.iter().all(|v| visible.contains(v.pos.x)),
            "coarse columns remain clipped to the viewport"
        );
        let mut tail = Quads::new(1.0);
        m.paint_into(
            &mut tail,
            body,
            Rangef::new(60000.0, 60010.0),
            Color32::WHITE,
            Color32::GRAY,
            Color32::RED,
        );
        assert!(
            !tail.mesh.vertices.is_empty(),
            "even the silent tail must retain its center line"
        );
        assert_eq!(
            tail.mesh
                .vertices
                .iter()
                .map(|v| v.pos.x)
                .fold(f32::NEG_INFINITY, f32::max),
            60010.0
        );
    }

    #[test]
    fn coarse_columns_apply_fades_across_the_entire_clip() {
        let p = peaks(131072, 1.0);
        let mut s = spec(65536.0, 131072);
        s.fade_in = s.width;
        let m = WaveMesh::from_peaks(&p, &s);
        assert_eq!(m.extent(MAX_COLUMNS / 4).unwrap().0, 4);
        assert_eq!(m.extent(3 * MAX_COLUMNS / 4).unwrap().0, 12);
        assert_eq!(m.extent(MAX_COLUMNS - 1).unwrap().0, 16);
        s.fade_in = 0.0;
        s.fade_out = s.width;
        let m = WaveMesh::from_peaks(&p, &s);
        assert_eq!(m.extent(MAX_COLUMNS / 4).unwrap().0, 12);
        assert_eq!(m.extent(3 * MAX_COLUMNS / 4).unwrap().0, 4);
        assert_eq!(m.extent(MAX_COLUMNS - 1).unwrap().0, 0);
    }

    #[test]
    fn rms_never_exceeds_the_local_peak() {
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
    fn fractional_envelopes_use_retina_density_and_physical_pixel_feathers() {
        let p = peaks(96000, 0.03125);
        let m = WaveMesh::from_peaks(&p, &spec(72.0, 96000));
        assert_eq!(m.cols.len(), 144);
        assert_eq!(m.column_width, 0.5);
        assert_eq!(
            m.cols[10].up, 0.5,
            "half-point amplitudes must not be rounded away"
        );
        let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(72.0, 36.0));
        for ppp in [1.0, 2.0] {
            let mut q = Quads::new(ppp);
            m.paint_into(
                &mut q,
                body,
                body.x_range(),
                Color32::WHITE,
                Color32::GRAY,
                Color32::RED,
            );
            let v = &q.mesh.vertices;
            assert_eq!(v[0].color.a(), 0);
            assert_eq!(v[1].color.a(), 255);
            assert!((v[1].pos.y - v[0].pos.y - 1.0 / ppp).abs() < 0.0001);
            assert_eq!(v[1].pos.y, 17.5);
            assert!(v.iter().any(|v| v.pos.y.fract() != 0.0));
        }
    }

    #[test]
    fn channel_meshes_preserve_stereo_difference() {
        let p = PeakData::from_planar(48000, &[vec![0.8; 48000], vec![0.2; 48000]]);
        let s = spec(72.0, 48000);
        let left = WaveMesh::from_channel(&p, &s, 0);
        let right = WaveMesh::from_channel(&p, &s, 1);
        assert!(left.cols[10].up > right.cols[10].up * 3.0);
        assert!(right.cols.iter().all(|c| c.core_up <= c.up));
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
            q.mesh.indices.len() <= 6 * 6,
            "{} indices",
            q.mesh.indices.len()
        );
    }
}
