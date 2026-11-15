//! `pad_grid` (n × m cells, `W_pads`): every pad is one cell (56 × 64, gap 4, `radius.m`) in
//! `track.body` with `track.ink` text: a 3 px bar in `track.base` at the top, name and note at the
//! bottom. A playing pad is bright (`oklch(0.92 0.06 hue)`) with `surface.0` text, the selected
//! one has a 1 px accent outline. A click selects the pad and plays it ([`PadGridOutput::clicked`]);
//! a sample file dropped on a pad goes to the callback of [`PadGrid::on_drop`].
//!
//! AccessKit: one node per pad, role `Button`, named „Pad Kick“, selected state, the note as
//! description.

use egui::{
    Color32, CursorIcon, DroppedFileHandle, Painter, Rect, Response, Role, Sense, Ui, WidgetInfo,
    pos2, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::oklch_to_srgb;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, device, radius};

use super::{cell_height, cell_width, elided_galley};
use crate::Lang;
use crate::state::{self, State};
use crate::widgets::metrics::{self as wm, device::pad_grid as metrics};
use crate::widgets::{accesskit, sense};
use crate::{focus, px};

/// One pad: its name ("Kick") and the note it plays ("C1").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pad<'a> {
    /// Name.
    pub name: &'a str,
    /// Note name, may be empty.
    pub note: &'a str,
}

impl<'a> Pad<'a> {
    /// A pad.
    pub const fn new(name: &'a str, note: &'a str) -> Self {
        Self { name, note }
    }

    /// The accessible name: „Pad Kick“.
    pub fn accessible_name(&self, t: Lang) -> String {
        t.ui_pad_name(self.name)
    }
}

/// The colours of a pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadLook {
    /// Ground.
    pub fill: Color32,
    /// Name and note.
    pub text: Color32,
    /// The bar at the top.
    pub bar: Color32,
}

/// The colour of a playing pad: `oklch(0.92 0.06 hue)` (`widget.pad_grid.playing`).
pub fn playing_color(palette: &TrackPalette) -> Color32 {
    let (l, c) = metrics::PLAYING_LC;
    color32(oklch_to_srgb(l, c, f64::from(palette.hue)))
}

/// The look of a pad of a track palette, `playing` or not.
pub fn look(palette: &TrackPalette, playing: bool) -> PadLook {
    PadLook {
        fill: if playing {
            playing_color(palette)
        } else {
            color32(palette.body)
        },
        text: color32(if playing {
            color::SURFACE_0
        } else {
            palette.ink
        }),
        bar: color32(palette.base),
    }
}

/// The rectangle of pad `index` in a grid of `cols` columns whose top left is `origin`.
pub fn pad_rect(origin: egui::Pos2, cols: u8, index: usize) -> Rect {
    let [w, h] = device::grid::CELL;
    let gap = device::grid::GAP;
    let (c, r) = (
        index % usize::from(cols.max(1)),
        index / usize::from(cols.max(1)),
    );
    Rect::from_min_size(
        pos2(
            origin.x + c as f32 * (w + gap),
            origin.y + r as f32 * (h + gap),
        ),
        vec2(w, h),
    )
}

/// Paints one pad.
pub fn paint_pad(painter: &Painter, rect: Rect, pad: &Pad<'_>, look: &PadLook) {
    let ppp = painter.pixels_per_point();
    px::fill(painter, rect, radius::M, look.fill);
    let [pad_y, pad_x] = metrics::PADDING;
    let inner = rect.shrink2(vec2(pad_x, pad_y));
    let bar = Rect::from_min_size(inner.min, vec2(inner.width(), metrics::BAR_H));
    px::fill(
        painter,
        px::snap_rect(bar, ppp),
        metrics::BAR_RADIUS,
        look.bar,
    );
    let name_style = typography::SMALL;
    let note_style = typography::MONO_XXS;
    let name = elided_galley(
        painter,
        pad.name,
        &name_style,
        Some(wm::SEMIBOLD),
        inner.width(),
    );
    let note = elided_galley(painter, pad.note, &note_style, None, inner.width());
    let note_h = if pad.note.is_empty() {
        0.0
    } else {
        note_style.line_height
    };
    let gap = if pad.note.is_empty() {
        0.0
    } else {
        metrics::TEXT_GAP
    };
    let name_y = inner.bottom() - note_h - gap - name_style.line_height;
    painter.galley(
        pos2(px::snap(inner.left(), ppp), px::snap(name_y, ppp)),
        name,
        look.text,
    );
    if !pad.note.is_empty() {
        let col = look.text.gamma_multiply(metrics::NOTE_OPACITY);
        let y = inner.bottom() - note_style.line_height;
        painter.galley(
            pos2(px::snap(inner.left(), ppp), px::snap(y, ppp)),
            note,
            col,
        );
    }
}

/// What a [`PadGrid`] did in this pass.
pub struct PadGridOutput {
    /// The whole grid.
    pub response: Response,
    /// The pad that was clicked: select it and play it.
    pub clicked: Option<usize>,
    /// The response of every pad, in order (for a context menu on a pad).
    pub pads: Vec<Response>,
}

/// What is called with a pad index and a file dropped on it.
type DropCallback<'a> = Box<dyn FnMut(usize, &DroppedFileHandle) + 'a>;

/// A grid of pads: `PadGrid::new(&pads, 4, &palette).selected(Some(0)).show(ui)`.
pub struct PadGrid<'a> {
    pads: &'a [Pad<'a>],
    cols: u8,
    palette: &'a TrackPalette,
    selected: Option<usize>,
    playing: &'a [usize],
    on_drop: Option<DropCallback<'a>>,
}

impl<'a> PadGrid<'a> {
    /// `pads` in `cols` columns in the colours of `palette`.
    pub fn new(pads: &'a [Pad<'a>], cols: u8, palette: &'a TrackPalette) -> Self {
        Self {
            pads,
            cols: cols.max(1),
            palette,
            selected: None,
            playing: &[],
            on_drop: None,
        }
    }

    /// The selected pad (accent outline).
    #[must_use]
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// The pads that sound right now (bright).
    #[must_use]
    pub fn playing(mut self, playing: &'a [usize]) -> Self {
        self.playing = playing;
        self
    }

    /// Called with the pad index and the file when a sample file is dropped on a pad.
    #[must_use]
    pub fn on_drop(mut self, f: impl FnMut(usize, &DroppedFileHandle) + 'a) -> Self {
        self.on_drop = Some(Box::new(f));
        self
    }

    /// Rows of the grid.
    pub fn rows(&self) -> u8 {
        self.pads.len().div_ceil(usize::from(self.cols)).max(1) as u8
    }

    /// Draws the grid and handles clicks and drops.
    pub fn show(mut self, ui: &mut Ui) -> PadGridOutput {
        let size = vec2(cell_width(self.cols), cell_height(self.rows()));
        let (area, response) = ui.allocate_exact_size(size, Sense::hover());
        let still = crate::widgets::is_still(ui);
        let (hovered_files, dropped, pointer) = ui.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw.dropped_files.clone(),
                i.pointer.latest_pos(),
            )
        });
        let mut clicked = None;
        let t = crate::lang(ui.ctx());
        let mut responses = Vec::with_capacity(self.pads.len());
        for (i, pad) in self.pads.iter().enumerate() {
            let rect = pad_rect(area.min, self.cols, i);
            let r = ui.interact(
                rect,
                response.id.with(("pad", i)),
                sense(ui, Sense::click()),
            );
            let st = State::of(ui, &r);
            let on = self.selected == Some(i);
            let name = pad.accessible_name(t);
            r.widget_info(|| WidgetInfo::selected(Role::Button, !st.disabled, on, &name));
            accesskit(&r, |n| {
                n.set_selected(on);
                if !pad.note.is_empty() {
                    n.set_description(t.ui_pad_note(pad.note));
                }
            });
            if !still && r.clicked() {
                r.request_focus();
                clicked = Some(i);
            }
            responses.push(r.clone());
            if !still && r.hovered() {
                ui.set_cursor_icon(CursorIcon::PointingHand);
            }
            let playing = self.playing.contains(&i);
            let pl = look(self.palette, playing);
            paint_pad(ui.painter(), rect, pad, &pl);
            if on {
                state::paint_select_ring(ui.painter(), rect, radius::M);
            }
            if st.focused && !st.disabled {
                focus::paint_ring(ui.painter(), rect, radius::M);
            }
            let over = pointer.is_some_and(|p| rect.contains(p));
            if hovered_files && over {
                // The drop target: the accent ring marks the pad the file would land on.
                state::paint_select_ring(ui.painter(), rect, radius::M);
            }
            if over
                && !dropped.is_empty()
                && let Some(cb) = self.on_drop.as_mut()
            {
                for file in &dropped {
                    cb(i, file);
                }
            }
        }
        PadGridOutput {
            response,
            clicked,
            pads: responses,
        }
    }
}
