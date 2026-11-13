//! Clips in the arrangement (design system `clipO`, `CV`, `clips`; `ks-view.js` `clipView`):
//! height = track − 6, head 14 (`size.clip-header`) in the track colour `base`, body `body`,
//! content in `ink` (L 0.88), `radius.m`.
//!
//! States ([`ClipState`] and [`ClipModel`]):
//!
//! | State | Look |
//! |---|---|
//! | hover | head `clip-head-hover`, trim handles at both ends, loop handle top right (`text.1`) |
//! | selected | 1 px accent ring, colours stay |
//! | muted | head `dim`, body `surface.2`, content and name `text.3` |
//! | recording | head `state.record`, body `state.record-surface`, name `text.1`, 1 px `text.1` head line on the right |
//! | looped | repetitions in `ink-dim`, joints as 1 px `surface.0` lines, ▾ mark in the head |
//! | trimming | 2 px accent on the dragged edge |
//! | Alt copy | the original as a dashed `text.3` outline ([`paint_clip_ghost`]) |
//!
//! The content preview is a [`PreviewMesh`] built once per (clip, zoom, body height) by the
//! caller and kept in a cache keyed by the clip's `Arc` pointer and the zoom level: the document
//! is immutable, so an edited clip is a new `Arc` and misses the cache by itself.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, Painter, Rect, Role, pos2, vec2};
use komponist_model::{Clip, ClipContent, Meter, Note, StepClip, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::track::{HUES, PRECOMPUTED};
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius, size, space};

use super::mesh::Quads;
use super::{Access, bar_span, metrics};
use crate::icons::{self, Caret};
use crate::{fonts, px};

/// Which end of a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipEdge {
    /// The left edge (start).
    Start,
    /// The right edge (end).
    End,
}

/// Interaction state of a clip, computed by the arrangement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClipState {
    /// The pointer is over the clip: trim and loop handles show.
    pub hovered: bool,
    /// The clip is selected: accent ring.
    pub selected: bool,
    /// An edge is being dragged (trim): 2 px accent on it.
    pub trim: Option<ClipEdge>,
}

impl ClipState {
    /// Nothing going on.
    pub const REST: Self = Self {
        hovered: false,
        selected: false,
        trim: None,
    };
    /// Selected.
    pub const SELECTED: Self = Self {
        selected: true,
        ..Self::REST
    };
    /// Pointer over the clip.
    pub const HOVERED: Self = Self {
        hovered: true,
        ..Self::REST
    };
}

/// What a clip shows.
#[derive(Clone, Copy, Debug)]
pub struct ClipModel<'a> {
    /// Text in the head ("Lead A", "Aufnahme · Take 1"), cut with "…". A recording clip gets
    /// its ● drawn in front; a leading "● " here is dropped.
    pub name: &'a str,
    /// Colours of the track hue.
    pub palette: &'a TrackPalette,
    /// The clip does not play: clip muted (key `0`) or its track muted (not while recording).
    pub muted: bool,
    /// The clip is being recorded: red head, growing.
    pub recording: bool,
    /// Show the loop mark ▾ in the head (the clip loops).
    pub loop_mark: bool,
    /// The content preview; `None` for an empty clip.
    pub preview: Option<&'a PreviewMesh>,
}

impl<'a> ClipModel<'a> {
    /// A plain clip named `name` without preview.
    pub fn new(name: &'a str, palette: &'a TrackPalette) -> Self {
        Self {
            name,
            palette,
            muted: false,
            recording: false,
            loop_mark: false,
            preview: None,
        }
    }
}

/// The rect of a clip from `x0` to `x1` (screen x of its start and end) in a track lane:
/// 1 px in from both ends, 3 px from top and bottom ("Höhe = Spur − 6"), at least
/// [`metrics::CLIP_MIN_WIDTH`] wide.
pub fn clip_rect(lane: Rect, x0: f32, x1: f32) -> Rect {
    let w = (x1 - x0 - 2.0 * metrics::CLIP_INSET_X).max(metrics::CLIP_MIN_WIDTH);
    Rect::from_min_size(
        pos2(
            x0 + metrics::CLIP_INSET_X,
            lane.min.y + metrics::CLIP_INSET_Y,
        ),
        vec2(w, lane.height() - 2.0 * metrics::CLIP_INSET_Y),
    )
}

/// The body height of a clip of height `clip_height` (below the head): the height a
/// [`PreviewMesh`] is built for.
pub fn body_height(clip_height: f32) -> f32 {
    (clip_height - size::CLIP_HEADER).max(0.0)
}

/// The content marks of a clip, laid out for one zoom and body height, in points relative to the
/// top left of the body.
///
/// Marks are whole points, so they land on pixel edges at 1× and 2×. Marks of one row that touch
/// or overlap are merged; at small zoom many notes fall into the same pixel columns and merge,
/// so the mesh grows with the clip's pixel area, not with its note count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewMesh {
    marks: Vec<Mark>,
    joints: Vec<i32>,
    width: i32,
    px_per_tick: f32,
    body_height: f32,
}

/// One mark: `x0 .. x1`, `y0 .. y1`, in whole points; `repeat` for loop repetitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Mark {
    repeat: bool,
    y0: i32,
    y1: i32,
    x0: i32,
    x1: i32,
}

/// Layout parameters of a [`PreviewMesh`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewSpec {
    /// Clip length: content after it is cut.
    pub length: Tick,
    /// Loop length of a looped MIDI clip ([`Clip::loop_length`]); step clips ignore it.
    pub loop_length: Option<Tick>,
    /// Horizontal zoom in points per tick.
    pub px_per_tick: f32,
    /// Body height in points ([`body_height`]).
    pub body_height: f32,
}

fn round(v: f32) -> i32 {
    v.round() as i32
}

impl PreviewMesh {
    /// The preview of `clip` (active take or step pattern) at `px_per_tick` for a body of
    /// `body_height`.
    pub fn from_clip(clip: &Clip, px_per_tick: f32, body_height: f32) -> Self {
        let spec = PreviewSpec {
            length: clip.length,
            loop_length: clip.loop_length,
            px_per_tick,
            body_height,
        };
        match &clip.content {
            ClipContent::Midi(m) => Self::midi(&m.active().notes, spec),
            ClipContent::Step(s) => Self::steps(s, spec),
        }
    }

    fn empty(spec: &PreviewSpec) -> Self {
        Self {
            marks: Vec::new(),
            joints: Vec::new(),
            width: round(spec.length.0 as f32 * spec.px_per_tick),
            px_per_tick: spec.px_per_tick,
            body_height: spec.body_height,
        }
    }

    /// Note marks (`clipView`): one 2 px line per note over its length minus 1 px, the pitch
    /// range of all notes ± 2 spread over the body height − 10, starting 4 below the head.
    /// A looped clip repeats the content `0 .. loop_length` until `length`; repetitions are
    /// marked for `ink-dim` and get a joint at their start.
    pub fn midi(notes: &[Note], spec: PreviewSpec) -> Self {
        let mut mesh = Self::empty(&spec);
        let (Some(lo), Some(hi)) = (
            notes.iter().map(|n| n.pitch).min(),
            notes.iter().map(|n| n.pitch).max(),
        ) else {
            return mesh;
        };
        let lo = f32::from(lo) - f32::from(metrics::PREVIEW_PITCH_MARGIN);
        let hi = f32::from(hi) + f32::from(metrics::PREVIEW_PITCH_MARGIN);
        let inner = (spec.body_height - metrics::PREVIEW_NOTE_V_PAD).max(0.0);
        let ppt = spec.px_per_tick;
        let length = spec.length.0.max(0);
        let period = spec
            .loop_length
            .map(|l| l.0)
            .filter(|&l| l > 0)
            .unwrap_or(length)
            .max(1);
        let h = round(metrics::PREVIEW_NOTE_HEIGHT).max(1);
        let reps = (length + period - 1) / period;
        mesh.marks.reserve(notes.len() * reps as usize);
        for r in 0..reps {
            let offset = r * period;
            if r > 0 {
                mesh.joints.push(round(offset as f32 * ppt));
            }
            for n in notes {
                if n.start.0 >= period {
                    continue;
                }
                let s = offset + n.start.0;
                if s >= length {
                    continue;
                }
                let e = (offset + (n.start.0 + n.length.0).min(period)).min(length);
                let x0 = round(s as f32 * ppt);
                let x1 = (round(e as f32 * ppt) - 1).max(x0 + 1);
                let y0 = round(
                    metrics::PREVIEW_NOTE_TOP + (hi - f32::from(n.pitch)) / (hi - lo) * inner,
                );
                mesh.marks.push(Mark {
                    repeat: r > 0,
                    y0,
                    y1: y0 + h,
                    x0,
                    x1,
                });
            }
        }
        mesh.finish();
        mesh
    }

    /// Step marks (`clipView`): the pattern repeated over the clip, one block per `on` step,
    /// rows spread over the body height − 6, starting 3 below the head.
    pub fn steps(steps: &StepClip, spec: PreviewSpec) -> Self {
        let mut mesh = Self::empty(&spec);
        let rows = steps.rows.len();
        if rows == 0 {
            return mesh;
        }
        let step_ticks = steps.resolution.ticks().0.max(1);
        let total = (spec.length.0.max(0) + step_ticks - 1) / step_ticks;
        let pattern = usize::from(steps.length_steps).clamp(1, komponist_model::project::MAX_STEPS);
        let sw = step_ticks as f32 * spec.px_per_tick;
        let rh = (spec.body_height - metrics::PREVIEW_STEP_V_PAD).max(0.0) / rows as f32;
        let w = (sw - metrics::PREVIEW_STEP_GAP).max(metrics::PREVIEW_STEP_GAP);
        let h = (rh - 1.0).max(metrics::PREVIEW_NOTE_HEIGHT);
        for k in 0..total {
            let i = k as usize % pattern;
            let x = k as f32 * sw + metrics::PREVIEW_STEP_OFFSET;
            let x0 = round(x);
            let x1 = round(x + w).max(x0 + 1);
            for (r, row) in steps.rows.iter().enumerate() {
                if !row.steps[i].on {
                    continue;
                }
                let y = metrics::PREVIEW_STEP_TOP + r as f32 * rh;
                let y0 = round(y);
                mesh.marks.push(Mark {
                    repeat: false,
                    y0,
                    y1: round(y + h).max(y0 + 1),
                    x0,
                    x1,
                });
            }
        }
        mesh.finish();
        mesh
    }

    /// Cuts marks at the clip end (`m.left < w − 1`, width at most to `w − 1`) and merges marks
    /// of one row that touch or overlap.
    fn finish(&mut self) {
        let end = self.width - 1;
        self.marks.retain(|m| m.x0 < end);
        for m in &mut self.marks {
            m.x1 = m.x1.min(end);
        }
        self.marks.sort_unstable();
        let mut merged: Vec<Mark> = Vec::with_capacity(self.marks.len());
        for m in self.marks.drain(..) {
            match merged.last_mut() {
                Some(last)
                    if last.repeat == m.repeat
                        && last.y0 == m.y0
                        && last.y1 == m.y1
                        && m.x0 <= last.x1 =>
                {
                    last.x1 = last.x1.max(m.x1);
                }
                _ => merged.push(m),
            }
        }
        self.marks = merged;
        self.joints.retain(|&x| x < end);
    }

    /// Number of marks (after merging).
    pub fn len(&self) -> usize {
        self.marks.len()
    }

    /// Whether there are no marks.
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// The zoom it was built for.
    pub fn px_per_tick(&self) -> f32 {
        self.px_per_tick
    }

    /// The body height it was built for.
    pub fn body_height(&self) -> f32 {
        self.body_height
    }

    /// Whether it was built for this zoom and body height (else rebuild).
    pub fn fits(&self, px_per_tick: f32, body_height: f32) -> bool {
        self.px_per_tick == px_per_tick && self.body_height == body_height
    }

    /// Adds the marks into `body` (snapped), cut to it: first pass in `ink`, repetitions in
    /// `repeat`, joints in `joint`.
    fn paint_into(&self, q: &mut Quads, body: Rect, ink: Color32, repeat: Color32, joint: Color32) {
        let (bw, bh) = (body.width(), body.height());
        q.reserve(self.marks.len() + self.joints.len());
        for m in &self.marks {
            let (x0, x1) = (m.x0 as f32, (m.x1 as f32).min(bw));
            let (y0, y1) = (m.y0 as f32, (m.y1 as f32).min(bh));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let c = if m.repeat { repeat } else { ink };
            q.quad(
                Rect::from_min_max(body.min + vec2(x0, y0), body.min + vec2(x1, y1)),
                c,
            );
        }
        let w = px::line_width(q.pixels_per_point());
        for &x in &self.joints {
            let x = x as f32;
            if x < bw {
                q.quad(
                    Rect::from_min_max(
                        pos2(body.min.x + x, body.min.y),
                        pos2(body.min.x + x + w, body.max.y),
                    ),
                    joint,
                );
            }
        }
    }
}

/// The colours of a clip in its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipColors {
    /// Head fill.
    pub head: Color32,
    /// Body fill.
    pub body: Color32,
    /// Content of the first pass.
    pub ink: Color32,
    /// Content of loop repetitions.
    pub ink_repeat: Color32,
    /// Name in the head.
    pub name: Color32,
}

/// The ink of a recording clip. Contract change pending: `state.record-ink` (#f2a6a8 in the
/// design); until then the `ink` of the red track hue (#ffc3bc), the closest token.
fn record_ink() -> Color32 {
    let red = HUES
        .iter()
        .find(|h| h.name == "red")
        .map_or(25, |h| h.degrees);
    let ink = PRECOMPUTED
        .iter()
        .find(|p| p.hue == red)
        .map_or(color::STATE_RECORD, |p| p.ink);
    color32(ink)
}

/// The colours of a clip (`clipO`: `head`, `bodyC`, `inkC`).
pub fn clip_colors(model: &ClipModel<'_>, state: ClipState) -> ClipColors {
    let p = model.palette;
    if model.recording {
        ClipColors {
            head: color32(color::STATE_RECORD),
            body: color32(color::STATE_RECORD_SURFACE),
            ink: record_ink(),
            ink_repeat: color32(p.ink_dim),
            name: color32(color::TEXT_1),
        }
    } else if model.muted {
        ClipColors {
            head: color32(p.dim),
            body: color32(color::SURFACE_2),
            ink: color32(color::TEXT_3),
            ink_repeat: color32(p.ink_dim),
            name: color32(color::TEXT_3),
        }
    } else {
        ClipColors {
            head: color32(if state.hovered {
                p.clip_head_hover
            } else {
                p.base
            }),
            body: color32(p.body),
            ink: color32(p.ink),
            ink_repeat: color32(p.ink_dim),
            name: color32(color::SURFACE_0),
        }
    }
}

/// Paints a clip at `rect` (see [`clip_rect`]).
pub fn paint_clip(painter: &Painter, rect: Rect, model: &ClipModel<'_>, state: ClipState) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    let c = clip_colors(model, state);
    let k = radius::M;
    let head_h = size::CLIP_HEADER.min(r.height());
    let head = Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + head_h));
    let body = Rect::from_min_max(pos2(r.min.x, head.max.y), r.max);

    let mut q = Quads::new(ppp);
    q.rounded(r, k, c.body);
    q.rounded_corners(head, k, [true, true, false, false], c.head);
    if let Some(preview) = model.preview {
        preview.paint_into(&mut q, body, c.ink, c.ink_repeat, color32(color::SURFACE_0));
    }
    q.paint(painter);

    paint_name(painter, head, model, c.name);

    let mut q = Quads::new(ppp);
    let w = px::line_width(ppp);
    if state.hovered && !model.recording {
        // Contract change pending: `track.$formula.clip-handle` = oklch(0.40 0.07 h); `dim`
        // (oklch 0.42 0.03 h) is the closest token.
        let handle = color32(model.palette.dim);
        let hw = metrics::CLIP_HANDLE_WIDTH.min(body.width() / 2.0);
        q.rounded_corners(
            Rect::from_min_max(body.min, pos2(body.min.x + hw, body.max.y)),
            k,
            [false, false, false, true],
            handle,
        );
        q.rounded_corners(
            Rect::from_min_max(pos2(body.max.x - hw, body.min.y), body.max),
            k,
            [false, false, true, false],
            handle,
        );
        let lw = metrics::CLIP_LOOP_HANDLE_WIDTH.min(head.width());
        q.rounded_corners(
            Rect::from_min_max(pos2(head.max.x - lw, head.min.y), head.max),
            k,
            [false, true, false, false],
            color32(color::TEXT_1),
        );
    }
    if model.recording {
        q.quad(
            Rect::from_min_max(pos2(r.max.x - w, r.min.y + w), pos2(r.max.x, r.max.y - w)),
            color32(color::TEXT_1),
        );
    }
    if let Some(edge) = state.trim {
        let tw = metrics::CLIP_TRIM_EDGE;
        let e = match edge {
            ClipEdge::Start => Rect::from_min_max(r.min, pos2(r.min.x + tw, r.max.y)),
            ClipEdge::End => Rect::from_min_max(pos2(r.max.x - tw, r.min.y), r.max),
        };
        q.rect(e, color32(color::ACCENT));
    }
    if state.selected {
        q.ring(r, k, 0.0, color32(color::ACCENT));
    }
    q.paint(painter);

    if model.loop_mark {
        let [mw, mh] = metrics::CLIP_LOOP_MARK;
        let inset = space::SPACE_1;
        let mark = Rect::from_min_size(pos2(r.max.x - inset - mw, r.min.y + inset), vec2(mw, mh));
        if mark.min.x > r.min.x {
            icons::paint_caret(painter, mark, Caret::Down, color32(color::SURFACE_0));
        }
    }
}

/// The clip name in the head: Sans 10/600 (the face and size of `type.label`, without its caps and
/// tracking), line height = head height, 5 px padding, cut with "…". A recording clip shows a
/// ● in front, drawn as a circle.
fn paint_name(painter: &Painter, head: Rect, model: &ClipModel<'_>, color: Color32) {
    let ppp = painter.pixels_per_point();
    let pad = metrics::CLIP_HEAD_PAD_X;
    let mut x = head.min.x + pad;
    let painter = painter.with_clip_rect(head.intersect(painter.clip_rect()));
    if model.recording {
        let d = metrics::CLIP_RECORD_DOT;
        let dot = Rect::from_min_size(pos2(x, head.center().y - d / 2.0), vec2(d, d));
        icons::paint_record(&painter, dot, color);
        x += d + space::SPACE_1;
    }
    let name = clip_name(model.name);
    let width = head.max.x - pad - x;
    if width <= 0.0 || name.is_empty() {
        return;
    }
    let style = typography::LABEL;
    let mut job = LayoutJob::single_section(
        name.to_owned(),
        TextFormat {
            font_id: fonts::font_id(&style),
            line_height: Some(head.height()),
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(pos2(px::snap(x, ppp), head.min.y), galley, color);
}

/// The clip name without a leading ● (drawn as a circle while recording).
fn clip_name(name: &str) -> &str {
    super::strip_symbol(name, '●')
}

/// Paints the original of a clip being Alt-dragged as a copy: a dashed 1 px `text.3` outline.
///
/// The design rounds the dashed border with `radius.m`; the dashes here have square corners.
pub fn paint_clip_ghost(painter: &Painter, rect: Rect) {
    let mut q = Quads::new(painter.pixels_per_point());
    q.dashed_border(rect, color32(color::TEXT_3));
    q.paint(painter);
}

/// Accessible name and role of a clip: `Clip Beat A, Takt 1–4` (a leading ● of a recording clip's
/// name is dropped; muted clips add `, stumm`), role grid cell, selectable.
pub fn clip_access(
    model: &ClipModel<'_>,
    start: Tick,
    end: Tick,
    meter: Meter,
    state: ClipState,
) -> Access {
    let mut s = format!(
        "Clip {}, {}",
        clip_name(model.name),
        bar_span(start, end, meter)
    );
    if model.muted {
        s.push_str(", stumm");
    }
    Access::selectable(Role::GridCell, s, state.selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{NoteId, Step, StepResolution, StepRow};

    fn note(start: i64, len: i64, pitch: u8) -> Note {
        Note {
            id: NoteId(0),
            start: Tick(start),
            length: Tick(len),
            pitch,
            velocity: 100,
        }
    }

    fn spec(length: i64, loop_length: Option<i64>, ppt: f32) -> PreviewSpec {
        PreviewSpec {
            length: Tick(length),
            loop_length: loop_length.map(Tick),
            px_per_tick: ppt,
            body_height: 36.0,
        }
    }

    #[test]
    fn midi_marks_follow_clip_view() {
        // 72 px per bar: 3840 ticks → 72 px.
        let ppt = 72.0 / 3840.0;
        let notes = [note(0, 480, 60), note(480, 480, 64), note(960, 1920, 62)];
        let m = PreviewMesh::midi(&notes, spec(3840, None, ppt));
        assert_eq!(m.len(), 3);
        // Pitch range 58..66: highest note at 4 + 2/8·26 = 10.5 → 11 (rounded half away).
        let top = m.marks.iter().map(|k| k.y0).min().unwrap();
        assert_eq!(top, 11);
        let first = m.marks.iter().find(|k| k.x0 == 0).unwrap();
        assert_eq!((first.x0, first.x1), (0, 8), "9 px minus the 1 px gap");
        assert!(m.joints.is_empty());
    }

    #[test]
    fn loops_repeat_with_joints() {
        let ppt = 72.0 / 3840.0;
        let notes = [note(0, 960, 60), note(3000, 2000, 62)];
        // Two-bar clip looping one bar 2.5 times.
        let m = PreviewMesh::midi(&notes, spec(3840 * 5 / 2, Some(3840), ppt));
        assert_eq!(m.joints, vec![72, 144]);
        let repeats = m.marks.iter().filter(|k| k.repeat).count();
        let firsts = m.marks.iter().filter(|k| !k.repeat).count();
        assert_eq!(firsts, 2);
        assert_eq!(
            repeats, 3,
            "second note cut at the loop end; half a pass at the end"
        );
        // The note reaching past the loop end is cut there.
        let cut = m.marks.iter().find(|k| !k.repeat && k.x0 > 0).unwrap();
        assert_eq!(cut.x1, 71);
    }

    #[test]
    fn small_zoom_merges_per_pixel_column() {
        // 10 000 sixteenths on one pitch over 625 bars at 2 px per bar.
        let notes: Vec<Note> = (0..10_000).map(|i| note(i * 240, 120, 60)).collect();
        let ppt = 2.0 / 3840.0;
        let m = PreviewMesh::midi(&notes, spec(10_000 * 240, None, ppt));
        assert!(m.len() < 10, "{} marks", m.len());
        // At 72 px per bar every sixteenth stays visible (4.5 px apart).
        let m = PreviewMesh::midi(&notes[..64], spec(64 * 240, None, 72.0 / 3840.0));
        assert_eq!(m.len(), 64);
    }

    #[test]
    fn steps_repeat_the_pattern() {
        let mut row = StepRow {
            name: "Kick".into(),
            note: 36,
            mute: false,
            solo: false,
            steps: [Step::default(); 32],
        };
        row.steps[0].on = true;
        row.steps[4].on = true;
        let clip = StepClip {
            rows: vec![
                row.clone(),
                StepRow {
                    steps: [Step::default(); 32],
                    ..row
                },
            ],
            length_steps: 8,
            resolution: StepResolution::Sixteenth,
            swing: 0.0,
        };
        // Two bars at 72 px per bar: 32 steps of 4.5 px, pattern of 8 → 8 hits.
        let m = PreviewMesh::steps(&clip, spec(2 * 3840, None, 72.0 / 3840.0));
        assert_eq!(m.len(), 8);
        let first = m.marks[0];
        assert_eq!((first.x0, first.x1, first.y0), (1, 4, 3));
    }

    #[test]
    fn access_names() {
        let p = TrackPalette::new(45);
        let m = ClipModel::new("Beat A", &p);
        let a = clip_access(
            &m,
            Tick(0),
            Tick(4 * 3840),
            Meter::FOUR_FOUR,
            ClipState::SELECTED,
        );
        assert_eq!(a.name, "Clip Beat A, Takt 1\u{2013}4");
        assert_eq!(a.selected, Some(true));
        let mut rec = ClipModel::new("● Aufnahme · Take 1", &p);
        rec.recording = true;
        let a = clip_access(
            &rec,
            Tick(12 * 3840),
            Tick(13 * 3840 + 100),
            Meter::FOUR_FOUR,
            ClipState::REST,
        );
        assert_eq!(a.name, "Clip Aufnahme · Take 1, Takt 13\u{2013}14");
        let mut muted = ClipModel::new("Arp", &p);
        muted.muted = true;
        let a = clip_access(
            &muted,
            Tick(0),
            Tick(3840),
            Meter::FOUR_FOUR,
            ClipState::REST,
        );
        assert_eq!(a.name, "Clip Arp, Takt 1, stumm");
    }

    #[test]
    fn colours_per_state() {
        let p = TrackPalette::new(150);
        let mut m = ClipModel::new("Lead A", &p);
        let c = clip_colors(&m, ClipState::REST);
        assert_eq!(
            (c.head, c.body, c.ink),
            (color32(p.base), color32(p.body), color32(p.ink))
        );
        assert_eq!(
            clip_colors(&m, ClipState::HOVERED).head,
            color32(p.clip_head_hover)
        );
        m.muted = true;
        let c = clip_colors(&m, ClipState::REST);
        assert_eq!(
            (c.head, c.body, c.name),
            (
                color32(p.dim),
                color32(color::SURFACE_2),
                color32(color::TEXT_3)
            )
        );
        m.recording = true;
        let c = clip_colors(&m, ClipState::REST);
        assert_eq!(c.head, color32(color::STATE_RECORD));
        assert_eq!(c.body, color32(color::STATE_RECORD_SURFACE));
    }

    #[test]
    fn rect_in_lane() {
        let lane = Rect::from_min_size(pos2(0.0, 100.0), vec2(1000.0, 56.0));
        let r = clip_rect(lane, 72.0, 144.0);
        assert_eq!(r, Rect::from_min_size(pos2(73.0, 103.0), vec2(70.0, 50.0)));
        assert_eq!(body_height(r.height()), 36.0);
        assert_eq!(clip_rect(lane, 10.0, 11.0).width(), 4.0);
    }
}
