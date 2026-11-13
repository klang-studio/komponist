//! The arithmetic of the piano roll's gestures, pure and tested: snapping of drags, hit tests,
//! box selection, the notes under the playhead, clip extension, labels.
//!
//! Rules from `Designsystem.dc.html` `ix.note` (the design system wins) and the prototype
//! (`Prototyp.dc.html` `prHit`, `prCreate`, `prDown`, `prDbl`, `prVel`, `quantize`):
//!
//! - Drag: time and pitch. With snap the time delta moves in whole grid steps (the notes keep
//!   their offsets to the grid, `Math.round(d16/g)*g`); Shift or snap off: free. The pitch
//!   follows the rows.
//! - Edge drag: the grabbed note's new length snaps to the grid (at least one step); the same
//!   delta goes to all selected notes. Without snap at least [`metrics::FREE_MIN_LENGTH`].
//! - Pencil click: a note on the grid line at or before the pointer, one grid step long
//!   ("Länge = Raster"); dragging pulls its length in grid steps.
//! - Notes that end after the clip make the clip longer, to whole bars (prototype: `c.w =
//!   Math.ceil(end/16)`), unless the clip loops.

use std::collections::BTreeSet;
use std::ops::Range;

use komponist_model::format::note_name;
use komponist_model::{Clip, Grid, Meter, Note, NoteId, Tick};

use super::geometry::Geometry;
use super::metrics;

/// The grid step of a drag: `Some(grid ticks)` with snap, `None` without (snap off or Shift).
pub type Snap = Option<Tick>;

/// The time delta of a move drag over `dx` ticks.
pub fn move_delta(dx: f64, snap: Snap) -> Tick {
    match snap {
        Some(g) if g.0 > 0 => Tick((dx / g.0 as f64).round() as i64 * g.0),
        _ => Tick(dx.round() as i64),
    }
}

/// The pitch delta of a drag `dy` points down at `key_height` (up = higher).
pub fn pitch_delta(dy: f32, key_height: f32) -> i32 {
    (-dy / key_height).round() as i32
}

/// The new length of a note of `length` whose edge was dragged by `dx` ticks.
pub fn resized_length(length: Tick, dx: f64, snap: Snap) -> Tick {
    let raw = length.0 as f64 + dx;
    match snap {
        Some(g) if g.0 > 0 => Tick(((raw / g.0 as f64).round() as i64 * g.0).max(g.0)),
        _ => Tick((raw.round() as i64).max(metrics::FREE_MIN_LENGTH.0)),
    }
}

/// The velocity offset of a Ctrl-drag `dy` points down.
pub fn velocity_offset(dy: f32) -> i16 {
    (-dy / metrics::VELOCITY_DRAG_PX_PER_STEP).round() as i16
}

/// Where a pencil click (or a pointer double-click) at absolute tick `at` puts a note: the grid
/// line at or before it (absolute, so it lands on a visible grid line), clip-local, ≥ 0.
pub fn new_note_start(at: f64, clip_start: Tick, grid: Tick, snap: bool) -> Tick {
    let abs = if snap && grid.0 > 0 {
        Tick((at / grid.0 as f64).floor() as i64 * grid.0)
    } else {
        Tick(at.floor() as i64)
    };
    (abs - clip_start).max(Tick::ZERO)
}

/// The length a pencil drag from `start` to clip-local tick `to` draws: whole grid steps, at
/// least one (`Math.max(g, Math.round(l/g)*g)`); without snap free, at least one grid step too.
pub fn pencil_length(start: Tick, to: f64, grid: Tick, snap: bool) -> Tick {
    let l = to - start.0 as f64;
    let g = grid.0.max(1);
    if snap {
        Tick(((l / g as f64).round() as i64 * g).max(g))
    } else {
        Tick((l.round() as i64).max(g))
    }
}

/// Where the pointer is on a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Index of the note in the take.
    pub index: usize,
    /// On its resize edge.
    pub edge: bool,
}

/// The indices of `notes` (sorted by start) that may overlap the clip-local ticks `t0 .. t1`:
/// a binary search on the start, widened by the longest note.
pub fn time_window(notes: &[Note], t0: Tick, t1: Tick) -> Range<usize> {
    let longest = notes.iter().map(|n| n.length).max().unwrap_or(Tick::ZERO);
    let from = notes.partition_point(|n| n.start + longest <= t0);
    let to = notes.partition_point(|n| n.start < t1);
    from..to.max(from)
}

/// The note under `p` (the one painted last wins, `prHit` searches backwards).
pub fn hit_note(notes: &[Note], window: Range<usize>, g: &Geometry, p: egui::Pos2) -> Option<Hit> {
    let pitch = g.pitch_at(p.y)?;
    let x = p.x;
    window.rev().find_map(|i| {
        let n = &notes[i];
        if n.pitch != pitch {
            return None;
        }
        let x0 = g.x_of_local(n.start);
        let x1 = g.x_of_local(n.start + n.length);
        // At least the drawn width (a very short note is 3 px wide).
        let x1 = x1.max(x0 + komponist_ui::widgets::metrics::timeline::NOTE_MIN_WIDTH);
        if !(x0..x1).contains(&x) {
            return None;
        }
        let edge = x > x1 - metrics::EDGE_GRAB && x1 - x0 > metrics::EDGE_MIN_WIDTH;
        Some(Hit { index: i, edge })
    })
}

/// The notes a box from clip-local ticks `t0 .. t1` over the keys `lo ..= hi` touches
/// (`n.s < sx1 && n.s + n.l > sx0 && p0 <= n.p <= p1`).
pub fn box_notes(notes: &[Note], t0: f64, t1: f64, lo: u8, hi: u8) -> Vec<NoteId> {
    notes
        .iter()
        .filter(|n| {
            (n.start.0 as f64) < t1
                && ((n.start + n.length).0 as f64) > t0
                && (lo..=hi).contains(&n.pitch)
        })
        .map(|n| n.id)
        .collect()
}

/// The clip-local length the editor shows as the clip's content: the loop length of a looped
/// clip, else the clip length.
pub fn content_length(clip: &Clip) -> Tick {
    clip.loop_length.unwrap_or(clip.length).max(Tick(1))
}

/// The clip-local position of the playhead inside `clip` (wrapped into the loop of a looped clip),
/// `None` outside the clip.
pub fn local_playhead(clip: &Clip, playhead: Tick) -> Option<Tick> {
    if playhead < clip.start || playhead >= clip.end() {
        return None;
    }
    let local = playhead - clip.start;
    Some(match clip.loop_length {
        Some(l) if l.0 > 0 => Tick(local.0 % l.0),
        _ => local,
    })
}

/// Whether `note` sounds at the clip-local position `at` (`gpos>=n.s&&gpos<n.s+n.l`).
pub fn sounds_at(note: &Note, at: Tick) -> bool {
    note.start <= at && at < note.start + note.length
}

/// The new clip length if notes ending at clip-local `end` reach past the clip: whole bars
/// (`Math.ceil(end/16)` bars). `None` if they fit or the clip loops.
pub fn extended_length(clip: &Clip, end: Tick, meter: Meter) -> Option<Tick> {
    if clip.loop_length.is_some() || end <= clip.length {
        return None;
    }
    let bar = meter.ticks_per_bar();
    Some(Tick((end.0 + bar - 1) / bar * bar))
}

/// The key at the top edge that shows `notes` when a clip opens, for `rows` visible rows.
///
/// With room for the margins the prototype's rule (`openClip`: `prTop = min(120, max(hi+3,
/// lo+nKeys-2))`); when the range fits only without them, it is centred; when it does not fit at
/// all, its top part shows (one key above the highest note), as the prototype prefers the top.
pub fn fit_top_key(notes: &[Note], rows: usize) -> u8 {
    let (lo, hi) = if notes.is_empty() {
        metrics::FIT_EMPTY
    } else {
        let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(0);
        let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(127);
        (lo, hi)
    };
    let (lo, hi, rows_i) = (i32::from(lo), i32::from(hi), rows as i32);
    let span = hi - lo + 1;
    let top = if span + metrics::FIT_ABOVE + metrics::FIT_BELOW <= rows_i {
        (hi + metrics::FIT_ABOVE).max(lo + rows_i - metrics::FIT_BELOW)
    } else if span <= rows_i {
        hi + (rows_i - span) / 2
    } else {
        hi + 1
    };
    clamp_top_key(top.min(i32::from(metrics::TOP_KEY_MAX)), rows)
}

/// `top` limited so the rows stay within the keys 0 … [`metrics::TOP_KEY_MAX`] (the top edge never
/// shows less than a full screen of keys above key 0).
pub fn clamp_top_key(top: i32, rows: usize) -> u8 {
    let min = (rows as i32 - 1).clamp(0, 127);
    top.clamp(min, i32::from(metrics::TOP_KEY_MAX).max(min)) as u8
}

/// The velocity a press `up` points above the lane's stem base sets (`(H-4-y)/52*127`).
pub fn lane_velocity(up: f32) -> u8 {
    (up / metrics::VELOCITY_MAX_HEIGHT * 127.0)
        .round()
        .clamp(1.0, 127.0) as u8
}

/// The notes a press in the velocity lane at clip-local tick `at` changes (`prVel`): the note
/// whose start is nearest, if within `max_dist` ticks, and every note starting with it — only the
/// selected ones if it is selected.
pub fn lane_targets(
    notes: &[Note],
    selection: &BTreeSet<NoteId>,
    at: f64,
    max_dist: f64,
) -> Vec<NoteId> {
    let best = notes.iter().min_by(|a, b| {
        let da = (a.start.0 as f64 - at).abs();
        let db = (b.start.0 as f64 - at).abs();
        da.total_cmp(&db)
    });
    let Some(best) = best.filter(|n| (n.start.0 as f64 - at).abs() <= max_dist) else {
        return Vec::new();
    };
    // `prVel` takes only the selected notes if the nearest one is selected; of notes starting
    // together any selected one counts.
    let only_selected = notes
        .iter()
        .any(|n| n.start == best.start && selection.contains(&n.id));
    notes
        .iter()
        .filter(|n| n.start == best.start && (!only_selected || selection.contains(&n.id)))
        .map(|n| n.id)
        .collect()
}

/// The length as the toolbar shows it: a note value ("1/8", "3/16", "1/1") when it is one of a
/// straight grid, else bars, beats and ticks ([`komponist_model::format::fmt_length`]).
pub fn fmt_note_length(length: Tick, meter: Meter) -> String {
    let whole = 4 * komponist_model::PPQ;
    let (mut num, mut den) = (length.0, whole);
    let gcd = gcd(num.abs(), den);
    if gcd > 0 {
        num /= gcd;
        den /= gcd;
    }
    if num > 0 && den.count_ones() == 1 && den <= 128 {
        format!("{num}/{den}")
    } else {
        komponist_model::format::fmt_length(length, meter)
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The grids of the "Raster" dropdown, in its order.
pub const GRIDS: [Grid; 9] = [
    Grid::Bar,
    Grid::Half,
    Grid::Quarter,
    Grid::Eighth,
    Grid::Sixteenth,
    Grid::ThirtySecond,
    Grid::QuarterTriplet,
    Grid::EighthTriplet,
    Grid::SixteenthTriplet,
];

/// The label of a grid as the design writes it ("1 Takt" in the arrangement header, "1/16",
/// "1/16 Triole" in the design system's dropdown).
pub fn grid_label(grid: Grid) -> &'static str {
    match grid {
        Grid::Bar => "1 Takt",
        Grid::Half => "1/2",
        Grid::Quarter => "1/4",
        Grid::Eighth => "1/8",
        Grid::Sixteenth => "1/16",
        Grid::ThirtySecond => "1/32",
        Grid::QuarterTriplet => "1/4 Triole",
        Grid::EighthTriplet => "1/8 Triole",
        Grid::SixteenthTriplet => "1/16 Triole",
    }
}

/// The selection info of the toolbar: "2 Noten" / "1 Note" selected, else all notes of the clip
/// (`selT`).
pub fn selection_text(selected: usize, total: usize) -> String {
    let n = if selected > 0 { selected } else { total };
    if n == 1 {
        "1 Note".to_owned()
    } else {
        format!("{n} Noten")
    }
}

/// The note name of a key ("C4"), for keys and the "Note" field.
pub fn key_name(pitch: u8) -> String {
    note_name(pitch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2, vec2};
    use komponist_model::ClipContent;
    use komponist_model::fixtures::demo_project;

    use crate::ui_state::PianoRollView;

    const M: Meter = Meter::FOUR_FOUR;
    const G16: Tick = Tick(240);

    fn note(id: u64, start: i64, length: i64, pitch: u8) -> Note {
        Note {
            id: NoteId(id),
            start: Tick(start),
            length: Tick(length),
            pitch,
            velocity: 100,
        }
    }

    fn geometry() -> Geometry {
        let view = PianoRollView {
            px_per_bar: 320.0,
            top_key: 74,
            key_height: 12.0,
            ..PianoRollView::default()
        };
        let body = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 300.0));
        Geometry::new(body, &view, Tick(3840), M)
    }

    #[test]
    fn moves_snap_in_grid_steps() {
        assert_eq!(move_delta(130.0, Some(G16)), Tick(240));
        assert_eq!(move_delta(110.0, Some(G16)), Tick(0));
        assert_eq!(move_delta(-130.0, Some(G16)), Tick(-240));
        // Shift / snap off: free.
        assert_eq!(move_delta(110.4, None), Tick(110));
        assert_eq!(pitch_delta(-12.0, 12.0), 1);
        assert_eq!(pitch_delta(17.0, 12.0), -1);
        assert_eq!(pitch_delta(5.0, 12.0), 0);
    }

    #[test]
    fn resizing_snaps_the_length() {
        assert_eq!(resized_length(Tick(480), 100.0, Some(G16)), Tick(480));
        assert_eq!(resized_length(Tick(480), 130.0, Some(G16)), Tick(720));
        // Never shorter than one grid step.
        assert_eq!(resized_length(Tick(480), -2000.0, Some(G16)), G16);
        // Without snap down to 1/8 of a 1/16.
        assert_eq!(resized_length(Tick(480), 17.0, None), Tick(497));
        assert_eq!(resized_length(Tick(480), -2000.0, None), Tick(30));
    }

    #[test]
    fn velocity_drags_in_steps_of_one_and_a_half_points() {
        assert_eq!(velocity_offset(-15.0), 10);
        assert_eq!(velocity_offset(3.0), -2);
        assert_eq!(lane_velocity(52.0), 127);
        assert_eq!(lane_velocity(26.0), 64);
        assert_eq!(lane_velocity(-5.0), 1);
    }

    #[test]
    fn pencil_places_on_the_grid_line_before_the_pointer() {
        // Clip at bar 2; pointer at absolute 3840 + 250 → local 240.
        assert_eq!(new_note_start(4090.0, Tick(3840), G16, true), Tick(240));
        assert_eq!(new_note_start(4090.0, Tick(3840), G16, false), Tick(250));
        // Left of the clip: clamped to its start.
        assert_eq!(new_note_start(100.0, Tick(3840), G16, true), Tick(0));
        // Length = grid, drag pulls whole steps.
        assert_eq!(pencil_length(Tick(240), 250.0, G16, true), G16);
        assert_eq!(pencil_length(Tick(240), 900.0, G16, true), Tick(720));
        assert_eq!(pencil_length(Tick(240), 1000.0, G16, false), Tick(760));
    }

    #[test]
    fn hit_test_finds_the_last_painted_note_and_its_edge() {
        let g = geometry();
        let notes = [
            note(1, 0, 480, 72),
            note(2, 240, 960, 72),
            note(3, 0, 60, 70),
        ];
        let all = 0..notes.len();
        let y = g.row_top(72) + 5.0;
        // x of local 300: both 1 and 2 cover it, 2 is painted later.
        let x = g.x_of_local(Tick(300));
        assert_eq!(
            hit_note(&notes, all.clone(), &g, pos2(x, y)),
            Some(Hit {
                index: 1,
                edge: false
            })
        );
        // The last 6 px of note 2 are its edge.
        let end = g.x_of_local(Tick(1200));
        assert!(
            hit_note(&notes, all.clone(), &g, pos2(end - 2.0, y))
                .unwrap()
                .edge
        );
        assert!(hit_note(&notes, all.clone(), &g, pos2(end + 1.0, y)).is_none());
        // A 5 px note has no edge.
        let y70 = g.row_top(70) + 5.0;
        let h = hit_note(&notes, all.clone(), &g, pos2(g.x_of_local(Tick(50)), y70)).unwrap();
        assert_eq!((h.index, h.edge), (2, false));
        // Another row.
        assert!(hit_note(&notes, all, &g, pos2(x, g.row_top(71) + 5.0)).is_none());
    }

    #[test]
    fn time_window_culls_by_start_and_longest_note() {
        let notes = [
            note(1, 0, 4000, 60),
            note(2, 1000, 100, 60),
            note(3, 5000, 100, 60),
            note(4, 9000, 100, 60),
        ];
        assert_eq!(time_window(&notes, Tick(3000), Tick(6000)), 0..3);
        assert_eq!(time_window(&notes, Tick(9500), Tick(12000)), 3..4);
        assert_eq!(time_window(&notes, Tick(20_000), Tick(30_000)), 4..4);
        assert_eq!(time_window(&[], Tick(0), Tick(10)), 0..0);
    }

    #[test]
    fn box_selection_touches_time_and_keys() {
        let notes = [
            note(1, 0, 480, 72),
            note(2, 480, 480, 70),
            note(3, 960, 480, 60),
        ];
        assert_eq!(
            box_notes(&notes, 400.0, 600.0, 70, 72),
            [NoteId(1), NoteId(2)]
        );
        assert_eq!(box_notes(&notes, 480.0, 600.0, 70, 72), [NoteId(2)]);
        assert!(box_notes(&notes, 0.0, 2000.0, 61, 69).is_empty());
    }

    #[test]
    fn playhead_inside_clips_and_loops() {
        let p = demo_project();
        let clip = &p.tracks[2].clips[0];
        assert_eq!(clip.start, Tick(8 * 3840));
        assert_eq!(
            local_playhead(clip, Tick(36_000)),
            Some(Tick(36_000 - 30_720))
        );
        assert_eq!(local_playhead(clip, Tick(0)), None);
        assert_eq!(local_playhead(clip, clip.end()), None);
        let looped = Clip {
            loop_length: Some(Tick(3840)),
            ..(**clip).clone()
        };
        assert_eq!(
            local_playhead(&looped, Tick(30_720 + 3840 + 10)),
            Some(Tick(10))
        );
        assert_eq!(content_length(&looped), Tick(3840));
        assert_eq!(content_length(clip), clip.length);
        let n = note(1, 100, 200, 60);
        assert!(sounds_at(&n, Tick(100)) && sounds_at(&n, Tick(299)));
        assert!(!sounds_at(&n, Tick(300)) && !sounds_at(&n, Tick(99)));
    }

    #[test]
    fn notes_past_the_end_lengthen_the_clip_to_bars() {
        let p = demo_project();
        let clip = &p.tracks[2].clips[0];
        assert_eq!(clip.length, Tick(2 * 3840));
        assert_eq!(extended_length(clip, Tick(2 * 3840), M), None);
        assert_eq!(
            extended_length(clip, Tick(2 * 3840 + 1), M),
            Some(Tick(3 * 3840))
        );
        let looped = Clip {
            loop_length: Some(Tick(3840)),
            ..(**clip).clone()
        };
        assert_eq!(extended_length(&looped, Tick(5 * 3840), M), None);
    }

    #[test]
    fn opening_fits_the_notes() {
        let p = demo_project();
        let ClipContent::Midi(m) = &p.tracks[2].clips[0].content else {
            panic!("midi");
        };
        let notes = &m.active().notes;
        let hi = notes.iter().map(|n| n.pitch).max().unwrap();
        let lo = notes.iter().map(|n| n.pitch).min().unwrap();
        let visible = |top: u8, rows: i32| {
            i32::from(top) >= i32::from(hi) && i32::from(top) - rows < i32::from(lo)
        };
        // Lead A spans G3 … C5 (18 keys). 30 rows: the prototype's margins.
        assert_eq!((lo, hi), (55, 72));
        assert_eq!(fit_top_key(notes, 30), 55 + 30 - 2);
        // 20 rows (S01): all visible, centred.
        let top = fit_top_key(notes, 20);
        assert_eq!(top, 73);
        assert!(visible(top, 20));
        // 15 rows: the top part.
        assert_eq!(fit_top_key(notes, 15), 73);
        // An empty clip shows C4 … C5 with the margins (`hi = 72`, `lo = 60`).
        assert_eq!(fit_top_key(&[], 18), 60 + 18 - 2);
        assert_eq!(clamp_top_key(500, 15), metrics::TOP_KEY_MAX);
        assert_eq!(clamp_top_key(-3, 15), 14);
    }

    #[test]
    fn lane_takes_the_nearest_start() {
        let notes = [
            note(1, 0, 480, 72),
            note(2, 0, 480, 60),
            note(3, 960, 480, 62),
        ];
        let none = BTreeSet::new();
        assert_eq!(
            lane_targets(&notes, &none, 30.0, 100.0),
            [NoteId(1), NoteId(2)]
        );
        assert_eq!(lane_targets(&notes, &none, 900.0, 100.0), [NoteId(3)]);
        assert!(lane_targets(&notes, &none, 500.0, 100.0).is_empty());
        let sel: BTreeSet<NoteId> = [NoteId(2)].into();
        assert_eq!(lane_targets(&notes, &sel, 30.0, 100.0), [NoteId(2)]);
    }

    #[test]
    fn labels() {
        assert_eq!(fmt_note_length(Tick(480), M), "1/8");
        assert_eq!(fmt_note_length(Tick(720), M), "3/16");
        assert_eq!(fmt_note_length(Tick(3840), M), "1/1");
        assert_eq!(fmt_note_length(Tick(320), M), "0.0.80");
        assert_eq!(selection_text(2, 17), "2 Noten");
        assert_eq!(selection_text(1, 17), "1 Note");
        assert_eq!(selection_text(0, 17), "17 Noten");
        assert_eq!(grid_label(Grid::Sixteenth), "1/16");
        assert_eq!(GRIDS.len(), 9);
        assert_eq!(key_name(69), "A4");
    }
}
