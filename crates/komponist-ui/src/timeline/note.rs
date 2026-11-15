//! Notes of the piano roll (design system `noteBox`, `nS`, `notes`): a rect one key row high minus
//! 2 px ("Höhe = Tastenhöhe − 2"), `radius.s`, brightness = velocity.
//!
//! - Colour: `TrackPalette::note(velocity)`, hover `note_hover` (L + 0.08), playing
//!   `TrackPalette::play` with a 1 px `text.1` ring.
//! - Selected: 1 px accent ring, the note keeps its colour. Below
//!   [`metrics::NOTE_RING_MIN_HEIGHT`] (zoomed out to key height 4) a ring would cover the
//!   neighbours, so selection fills the note with accent instead ("Auswahl = Akzentfläche") and a
//!   playing note just takes the playing colour.
//! - Muted: no fill, a 1 px inset contour in `note-muted`. Drag original: no fill, dashed
//!   `text.3` contour.
//! - From 28 × 12 on, the note shows its name ("mit Name ab h 12, w 28").
//!
//! [`paint_note`] paints one note. For many, [`NoteBatch`] collects them into one mesh (fills
//! first, then rings on top, then names), which is what the piano roll uses for its 10 000 notes.

use egui::{Color32, Painter, Rect, Role, pos2, vec2};
use komponist_model::format::{fmt_pos, note_name};
use komponist_model::{Meter, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius};

use super::mesh::Quads;
use super::{Access, TimeAxis};
use crate::Lang;
use crate::widgets::metrics::timeline as metrics;
use crate::{fonts, px};

/// What is going on with a note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoteState {
    /// The pointer is over the note.
    pub hovered: bool,
    /// The note is selected.
    pub selected: bool,
    /// The note sounds right now (playback, or held from the keyboard).
    pub playing: bool,
    /// The note does not play (muted clip or track): contour only.
    pub muted: bool,
    /// The original left behind while dragging a copy (dashed contour).
    pub ghost: bool,
}

impl NoteState {
    /// Nothing going on.
    pub const REST: Self = Self {
        hovered: false,
        selected: false,
        playing: false,
        muted: false,
        ghost: false,
    };
}

/// The rect of a note from `start` for `length` in a key row whose top is `row_top`, with the
/// design's inset ([`metrics::NOTE_INSET`]) and at least [`metrics::NOTE_MIN_WIDTH`] wide.
pub fn note_rect(
    axis: &TimeAxis,
    start: Tick,
    length: Tick,
    row_top: f32,
    key_height: f32,
) -> Rect {
    let inset = metrics::NOTE_INSET;
    let x = axis.x(start) + inset;
    let w = (axis.width(length) - 2.0 * inset).max(metrics::NOTE_MIN_WIDTH);
    let h = (key_height - 2.0 * inset).max(1.0);
    Rect::from_min_size(pos2(x, row_top + inset), vec2(w, h))
}

/// Colours and parts of a note at its size, from the design's note states and zoom levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteLook {
    /// Fill, `None` for muted notes and drag originals.
    pub fill: Option<Color32>,
    /// 1 px ring outside the note (selection: accent, playing: `text.1`).
    pub ring: Option<Color32>,
    /// 1 px contour inside the note (muted).
    pub contour: Option<Color32>,
    /// Dashed 1 px contour inside the note (drag original).
    pub dashed: Option<Color32>,
    /// Corner radius: `radius.s`, at most [`metrics::NOTE_RADIUS_PER_HEIGHT`] of the height.
    pub radius: f32,
    /// The note shows its name.
    pub label: bool,
}

/// The look of a note of `velocity` in `palette` with size `size` (points) in `state`.
pub fn note_look(
    palette: &TrackPalette,
    velocity: u8,
    state: NoteState,
    size: egui::Vec2,
) -> NoteLook {
    let rings = size.y >= metrics::NOTE_RING_MIN_HEIGHT;
    let radius = radius::S.min(size.y * metrics::NOTE_RADIUS_PER_HEIGHT);
    let label = size.x >= metrics::NOTE_LABEL_MIN[0] && size.y >= metrics::NOTE_LABEL_MIN[1];
    if state.ghost {
        return NoteLook {
            fill: None,
            ring: None,
            contour: None,
            dashed: Some(color32(color::TEXT_3)),
            radius,
            label: false,
        };
    }
    let base = if state.playing {
        palette.play
    } else if state.hovered {
        palette.note_hover(velocity)
    } else {
        palette.note(velocity)
    };
    let ring = if state.selected {
        Some(color32(color::ACCENT))
    } else if state.playing {
        Some(color32(color::TEXT_1))
    } else {
        None
    };
    if state.muted {
        let contour = color32(palette.note_muted);
        return if rings {
            NoteLook {
                fill: None,
                ring: state.selected.then(|| color32(color::ACCENT)),
                contour: Some(contour),
                dashed: None,
                radius,
                label: false,
            }
        } else {
            NoteLook {
                fill: Some(if state.selected {
                    color32(color::ACCENT)
                } else {
                    contour
                }),
                ring: None,
                contour: None,
                dashed: None,
                radius,
                label: false,
            }
        };
    }
    if rings {
        NoteLook {
            fill: Some(color32(base)),
            ring,
            contour: None,
            dashed: None,
            radius,
            label,
        }
    } else {
        let fill = if state.selected {
            color32(color::ACCENT)
        } else {
            color32(base)
        };
        NoteLook {
            fill: Some(fill),
            ring: None,
            contour: None,
            dashed: None,
            radius,
            label: false,
        }
    }
}

/// Paints one note of `pitch` and `velocity` at `rect` (see [`note_rect`]).
pub fn paint_note(
    painter: &Painter,
    rect: Rect,
    palette: &TrackPalette,
    pitch: u8,
    velocity: u8,
    state: NoteState,
) {
    let mut batch = NoteBatch::new(painter.pixels_per_point());
    batch.add(rect, palette, pitch, velocity, state);
    batch.paint(painter);
}

/// Many notes painted as one mesh: fills in the order added, then all rings and contours on
/// top, then the names. Build one per frame for the visible notes.
pub struct NoteBatch {
    fills: Quads,
    rings: Quads,
    labels: Vec<(Rect, u8)>,
    names: bool,
}

impl NoteBatch {
    /// An empty batch for `pixels_per_point`.
    pub fn new(pixels_per_point: f32) -> Self {
        Self {
            fills: Quads::new(pixels_per_point),
            rings: Quads::new(pixels_per_point),
            labels: Vec::new(),
            names: true,
        }
    }

    /// Whether notes large enough show their name (default on).
    pub fn show_names(&mut self, on: bool) {
        self.names = on;
    }

    /// An empty batch with room for `notes` plain notes.
    pub fn with_capacity(pixels_per_point: f32, notes: usize) -> Self {
        let mut b = Self::new(pixels_per_point);
        // A note fill is 12 vertices and 10 triangles (`Quads::soft_rounded`).
        b.fills.reserve_exact(12 * notes, 10 * notes);
        b
    }

    /// Adds a note (see [`paint_note`]).
    pub fn add(
        &mut self,
        rect: Rect,
        palette: &TrackPalette,
        pitch: u8,
        velocity: u8,
        state: NoteState,
    ) {
        let look = note_look(palette, velocity, state, rect.size());
        self.add_look(rect, &look, pitch);
    }

    /// Adds a note with a precomputed look (for callers that cache [`note_look`]).
    pub fn add_look(&mut self, rect: Rect, look: &NoteLook, pitch: u8) {
        if let Some(fill) = look.fill {
            self.fills.soft_rounded(rect, look.radius, fill);
        }
        if let Some(c) = look.contour {
            self.rings.inner_ring(rect, look.radius, c);
        }
        if let Some(c) = look.dashed {
            self.rings.dashed_border(rect, c);
        }
        if let Some(c) = look.ring {
            self.rings.ring(rect, look.radius, 0.0, c);
        }
        if look.label && self.names {
            self.labels.push((rect, pitch));
        }
    }

    /// Number of vertices so far (for benches and tests).
    pub fn vertex_count(&self) -> usize {
        self.fills.mesh.vertices.len() + self.rings.mesh.vertices.len()
    }

    /// The batch as one mesh, without the names (for benches and custom painting).
    pub fn into_mesh(self) -> egui::Mesh {
        let mut q = self.fills;
        q.append(self.rings);
        q.mesh
    }

    /// Paints everything.
    pub fn paint(self, painter: &Painter) {
        let Self {
            mut fills,
            rings,
            labels,
            ..
        } = self;
        fills.append(rings);
        fills.paint(painter);
        for (rect, pitch) in labels {
            paint_label(painter, rect, pitch);
        }
    }
}

/// The note name in a note: `value.s` in `surface.0`, [`metrics::NOTE_LABEL_PAD`] from the left,
/// centred vertically, clipped to the note.
///
/// The design sets it in Mono 9; there is no 9 px type token, so it uses `value.s` (Mono 10).
fn paint_label(painter: &Painter, rect: Rect, pitch: u8) {
    let ppp = painter.pixels_per_point();
    let style = typography::VALUE_S;
    let galley = painter.layout_job(fonts::layout_job(
        &note_name(pitch),
        &style,
        color32(color::SURFACE_0),
    ));
    let y = rect.center().y - galley.size().y / 2.0;
    let pos = pos2(
        px::snap(rect.min.x + metrics::NOTE_LABEL_PAD, ppp),
        px::snap(y, ppp),
    );
    painter
        .with_clip_rect(rect.intersect(painter.clip_rect()))
        .galley(pos, galley, Color32::PLACEHOLDER);
}

/// Accessible name and role of a note: `Note C4, 9.1.000` (pitch name, absolute position in
/// `Takt.Schlag.Tick`), role grid cell, selectable.
pub fn note_access(t: Lang, pitch: u8, start: Tick, meter: Meter, state: NoteState) -> Access {
    let (pitch, pos) = (note_name(pitch), fmt_pos(start, meter));
    let name = if state.muted {
        t.a11y_note_muted(pitch, pos)
    } else {
        t.a11y_note(pitch, pos)
    };
    Access::selectable(Role::GridCell, name, state.selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> TrackPalette {
        TrackPalette::new(150)
    }

    #[test]
    fn rect_from_time_and_key_row() {
        let axis = TimeAxis::per_step(0.0, 20.0);
        let r = note_rect(&axis, Tick(240), Tick(480), 24.0, 12.0);
        assert_eq!(r, Rect::from_min_size(pos2(21.0, 25.0), vec2(38.0, 10.0)));
        let tiny = note_rect(&axis, Tick(0), Tick(1), 0.0, 4.0);
        assert_eq!(tiny.size(), vec2(3.0, 2.0));
    }

    #[test]
    fn looks_follow_the_note_states() {
        let p = palette();
        let big = vec2(80.0, 12.0);
        let rest = note_look(&p, 90, NoteState::REST, big);
        assert_eq!(rest.fill, Some(color32(p.note(90))));
        assert_eq!(rest.ring, None);
        assert!(rest.label);
        assert_eq!(rest.radius, radius::S);

        let hover = note_look(
            &p,
            90,
            NoteState {
                hovered: true,
                ..NoteState::REST
            },
            big,
        );
        assert_eq!(hover.fill, Some(color32(p.note_hover(90))));

        let sel = note_look(
            &p,
            90,
            NoteState {
                selected: true,
                ..NoteState::REST
            },
            big,
        );
        assert_eq!(sel.fill, Some(color32(p.note(90))), "keeps its colour");
        assert_eq!(sel.ring, Some(color32(color::ACCENT)));

        let play = note_look(
            &p,
            90,
            NoteState {
                playing: true,
                ..NoteState::REST
            },
            big,
        );
        assert_eq!(play.fill, Some(color32(p.play)));
        assert_eq!(play.ring, Some(color32(color::TEXT_1)));

        let muted = note_look(
            &p,
            90,
            NoteState {
                muted: true,
                ..NoteState::REST
            },
            big,
        );
        assert_eq!(muted.fill, None);
        assert_eq!(muted.contour, Some(color32(p.note_muted)));

        let ghost = note_look(
            &p,
            90,
            NoteState {
                ghost: true,
                ..NoteState::REST
            },
            big,
        );
        assert_eq!(ghost.dashed, Some(color32(color::TEXT_3)));
        assert_eq!(ghost.fill, None);
    }

    #[test]
    fn zoom_levels() {
        let p = palette();
        let sel = NoteState {
            selected: true,
            ..NoteState::REST
        };
        // Key 8: note 6 high, ring, no name.
        let k8 = note_look(&p, 110, sel, vec2(50.0, 6.0));
        assert_eq!(k8.ring, Some(color32(color::ACCENT)));
        assert!(!k8.label);
        assert_eq!(k8.radius, 2.0);
        // Key 4: selection is an accent fill.
        let k4 = note_look(&p, 110, sel, vec2(40.0, 3.0));
        assert_eq!(k4.fill, Some(color32(color::ACCENT)));
        assert_eq!(k4.ring, None);
        assert_eq!(k4.radius, 1.0);
        // The name needs 28 × 12.
        assert!(!note_look(&p, 110, NoteState::REST, vec2(27.0, 12.0)).label);
        assert!(!note_look(&p, 110, NoteState::REST, vec2(80.0, 11.0)).label);
    }

    #[test]
    fn access_names() {
        let a = note_access(
            Lang::De,
            60,
            Tick(8 * 3840),
            Meter::FOUR_FOUR,
            NoteState::REST,
        );
        assert_eq!(a.name, "Note C4, 9.1.000");
        assert_eq!(a.role, Role::GridCell);
        assert_eq!(a.selected, Some(false));
        let a = note_access(
            Lang::De,
            61,
            Tick(960 + 480),
            Meter::FOUR_FOUR,
            NoteState {
                selected: true,
                muted: true,
                ..NoteState::REST
            },
        );
        assert_eq!(a.name, "Note C#4, 1.2.120, stumm");
        assert_eq!(a.selected, Some(true));
    }

    #[test]
    fn batch_puts_rings_after_fills() {
        let p = palette();
        let mut b = NoteBatch::new(1.0);
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 10.0));
        b.add(
            r,
            &p,
            60,
            100,
            NoteState {
                selected: true,
                ..NoteState::REST
            },
        );
        b.add(r.translate(vec2(50.0, 0.0)), &p, 62, 100, NoteState::REST);
        let mesh = b.into_mesh();
        let last = mesh.vertices.last().unwrap().color;
        let accent = color32(color::ACCENT);
        assert!(last == accent || last == Color32::TRANSPARENT);
        assert!(
            mesh.vertices[..4]
                .iter()
                .all(|v| v.color == color32(p.note(100)))
        );
    }
}
