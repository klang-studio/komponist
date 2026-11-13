//! The fader ("Fader und Pegelanzeige"; design system `scaleDb`, `scale`, `fader`, `faders`,
//! `ix.fader`; taper `DBT`, `dbToP`, `pToDb` of `ks-engine.js`).
//!
//! A 4 px track (`size.fader-track`, `surface.0`) and a double-trapezoid thumb: 32 × 44 with
//! waist 22 (`size.fader-thumb`) or compact 24 × 28 with waist 15 (`size.fader-thumb-compact`).
//! The value line across the thumb's waist sits at the level; the dB scale (+6 … −∞) can stand
//! on the left.
//!
//! | Input | What happens (`ix.fader`) |
//! |---|---|
//! | Drag | relative from the thumb, no jump on a click on the track; the pointer is hidden and grabbed; the value line turns accent and the value tooltip shows |
//! | Shift drag | fine (× 0.1); Ctrl coarse (× 10) |
//! | Alt-click | 0.0 dB |
//! | Double-click (thumb, or the value under the fader, [`DbValue`]) | text entry in dB |
//! | ↑ ↓ (focused) | ±1 dB, Shift 0.1 dB, Ctrl 10 dB (keyboard access, not in `ix.fader`) |
//!
//! The level is in dB, `−70` ([`VOLUME_DB_MIN`]) meaning −∞, as stored in the project
//! (`Track::volume_db`). AccessKit: role `Slider`, the caller's German name, value as number and
//! as text ("−3.2 dB").

use egui::{
    Align, Color32, CursorIcon, Id, Painter, Rect, Response, Role, Sense, Shape, Stroke, Ui, Vec2,
    Widget, WidgetInfo, pos2, vec2,
};
use komponist_model::format::{MINUS, fmt_db, parse_db};
use komponist_model::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, line, radius, size, space};

use super::meter;
use super::number_field::{
    accesskit_requests, caption, column, drag_gesture, entry, expose_value, galley, is_still,
    key_steps, paint_entry_ring, sense, snap_to, take_focus_on_press, text_pos, text_width,
    value_label,
};
use crate::drag::{ClickAction, ValueDrag};
use crate::state::State;
use crate::{focus, fonts, px, tooltip};

// ---------------------------------------------------------------------------------------------
// Taper

/// The fader taper (`DBT` in `ks-engine.js`): dB → position from the top, 0 at +6 dB, 1 at the
/// bottom (−70 dB = −∞), linear between the points. The scale labels sit on these points.
pub const DBT: [(f32, f32); 7] = [
    (6.0, 0.0),
    (0.0, 0.14),
    (-6.0, 0.30),
    (-12.0, 0.45),
    (-24.0, 0.65),
    (-48.0, 0.85),
    (-70.0, 1.0),
];

/// Positions at or below this are −∞ (`pToDb`: `p >= .995`).
pub const SILENCE_POSITION: f32 = 0.995;

/// dB → fader position from the top, 0–1 (`dbToP`): ≤ −70 dB (and NaN) → 1, above +6 dB → 0.
pub fn db_to_p(db: f32) -> f32 {
    if db.is_nan() || db <= VOLUME_DB_MIN {
        return 1.0;
    }
    for w in DBT.windows(2) {
        let ((a, pa), (b, pb)) = (w[0], w[1]);
        if db <= a && db >= b {
            return pa + (a - db) / (a - b) * (pb - pa);
        }
    }
    0.0
}

/// Fader position from the top → dB (`pToDb`): `f32::NEG_INFINITY` from [`SILENCE_POSITION`]
/// on, +6 dB at the top; the position is clamped to 0–1.
pub fn p_to_db(p: f32) -> f32 {
    let p = if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) };
    if p >= SILENCE_POSITION {
        return f32::NEG_INFINITY;
    }
    for w in DBT.windows(2) {
        let ((a, pa), (b, pb)) = (w[0], w[1]);
        if p >= pa && p <= pb {
            return a + (p - pa) / (pb - pa) * (b - a);
        }
    }
    VOLUME_DB_MAX
}

/// A dB value as the project stores it: −∞ becomes [`VOLUME_DB_MIN`], the rest is clamped to
/// −70 … +6 and rounded to 0.1 dB (what [`fmt_db`] shows).
pub fn stored_db(db: f32) -> f32 {
    if db.is_nan() || db <= VOLUME_DB_MIN {
        VOLUME_DB_MIN
    } else {
        (snap_to(f64::from(db), 0.1) as f32).clamp(VOLUME_DB_MIN, VOLUME_DB_MAX)
    }
}

/// The scale label of a taper point: "+6", "0", "−6" … "−∞".
pub fn scale_label(db: f32) -> String {
    if db <= VOLUME_DB_MIN {
        format!("{MINUS}\u{221e}")
    } else if db > 0.0 {
        format!("+{}", db.round() as i32)
    } else if db == 0.0 {
        "0".to_owned()
    } else {
        format!("{MINUS}{}", (-db).round() as i32)
    }
}

// ---------------------------------------------------------------------------------------------
// Geometry and look

/// Sizes of the design (`fader()`, `faders`, the card's spec line) that `tokens.json` has no
/// token for (contract change requests).
pub mod metrics {
    use komponist_tokens::space;

    /// Track length of the design's samples (`height: 200`); the gallery default.
    pub const TRACK_LENGTH: f32 = 200.0;
    /// Width of the scale column (labels right-aligned in it), `space.4 + space.1`.
    pub const SCALE_WIDTH: f32 = space::SPACE_4 + space::SPACE_1;
    /// Gap between scale column and thumb (`left: 22` − 20).
    pub const SCALE_GAP: f32 = space::SPACE_0_5;
    /// Thickness of the value line ("Wertlinie", `stroke-width: 2`).
    pub const VALUE_LINE: f32 = 2.0;
    /// Waist of the normal thumb ("Taille 22").
    pub const WAIST: f32 = 22.0;
    /// Waist of the compact thumb ("Taille 15").
    pub const WAIST_COMPACT: f32 = 15.0;
    /// Grooves of the normal thumb, per half: (distance from the top or bottom edge, inset from
    /// the side), from the design's SVG.
    pub const GROOVES: [(f32, f32); 2] = [(6.0, 5.0), (11.0, 6.0)];
    /// The one groove per half of the compact thumb ("1 Rille je Hälfte").
    pub const GROOVES_COMPACT: [(f32, f32); 1] = [(7.0, 6.0)];
    /// Inset of the value line from the thumb's sides (normal, compact).
    pub const VALUE_LINE_INSET: [f32; 2] = [4.0, 3.0];
}

/// The two thumb sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FaderSize {
    /// Thumb 32 × 44, waist 22, two grooves per half.
    #[default]
    Normal,
    /// Thumb 24 × 28, waist 15, one groove per half.
    Compact,
}

impl FaderSize {
    /// Thumb width and height.
    pub fn thumb(self) -> Vec2 {
        let [w, h] = match self {
            Self::Normal => size::FADER_THUMB,
            Self::Compact => size::FADER_THUMB_COMPACT,
        };
        vec2(w, h)
    }

    fn waist(self) -> f32 {
        match self {
            Self::Normal => metrics::WAIST,
            Self::Compact => metrics::WAIST_COMPACT,
        }
    }

    fn grooves(self) -> &'static [(f32, f32)] {
        match self {
            Self::Normal => &metrics::GROOVES,
            Self::Compact => &metrics::GROOVES_COMPACT,
        }
    }

    fn value_line_inset(self) -> f32 {
        metrics::VALUE_LINE_INSET[self as usize]
    }
}

/// Colours of a fader in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Thumb fill.
    pub thumb: Color32,
    /// Thumb outline.
    pub edge: Color32,
    /// Grooves.
    pub grip: Color32,
    /// Value line across the waist.
    pub value_line: Color32,
    /// Track.
    pub track: Color32,
    /// Keyboard focus ring around the thumb.
    pub focus_ring: bool,
}

/// The look of a fader in `state` (`fader()`). The design gives the thumb colours as hex values
/// between the tokens (`#34363b`, `#4d5056`, `#5a5d63`, …); they map to the nearest tokens
/// (contract change request): rest `surface.4` / edge `text.4` / grooves `text.4`, hover
/// `surface.5` with grooves `text.3`, drag `line.2` with edge and grooves `text.3` and the value
/// line accent, disabled `surface.3` / `surface.4` with value line `text.4`.
pub fn look(state: State) -> Look {
    let track = color32(color::SURFACE_0);
    if state.disabled {
        return Look {
            thumb: color32(color::SURFACE_3),
            edge: color32(color::SURFACE_4),
            grip: color32(color::SURFACE_4),
            value_line: color32(color::TEXT_4),
            track,
            focus_ring: false,
        };
    }
    let focus_ring = state.focused;
    if state.pressed {
        Look {
            thumb: color32(color::LINE_2),
            edge: color32(color::TEXT_3),
            grip: color32(color::TEXT_3),
            value_line: color32(color::ACCENT),
            track,
            focus_ring,
        }
    } else if state.hovered {
        Look {
            thumb: color32(color::SURFACE_5),
            edge: color32(color::TEXT_4),
            grip: color32(color::TEXT_3),
            value_line: color32(color::TEXT_1),
            track,
            focus_ring,
        }
    } else {
        Look {
            thumb: color32(color::SURFACE_4),
            edge: color32(color::TEXT_4),
            grip: color32(color::TEXT_4),
            value_line: color32(color::TEXT_1),
            track,
            focus_ring,
        }
    }
}

/// Text style of the scale. The design sets it in Mono 9/10, which has no token; `value.s`
/// (Mono 10/12, `text.3`) is the nearest (contract change request).
pub const SCALE_TEXT: TypeStyle = typography::VALUE_S;

/// Where the parts of a fader lie in its rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The track (4 wide).
    pub track: Rect,
    /// Thumb size.
    pub thumb: Vec2,
    /// Left edge of the thumb column.
    pub thumb_left: f32,
    /// Right edge of the scale column, if the scale is shown.
    pub scale_right: Option<f32>,
}

impl Layout {
    /// The layout of a fader of `size` in `rect`, with the scale column on the left if `scale`.
    /// The track runs between the thumb centres at the top and bottom end.
    pub fn new(rect: Rect, size: FaderSize, scale: bool) -> Self {
        let thumb = size.thumb();
        let thumb_left = if scale {
            rect.left() + metrics::SCALE_WIDTH + metrics::SCALE_GAP
        } else {
            rect.left()
        };
        let cx = thumb_left + thumb.x / 2.0;
        let track = Rect::from_x_y_ranges(
            cx - size::FADER_TRACK / 2.0..=cx + size::FADER_TRACK / 2.0,
            rect.top() + thumb.y / 2.0..=rect.bottom() - thumb.y / 2.0,
        );
        Self {
            track,
            thumb,
            thumb_left,
            scale_right: scale.then_some(rect.left() + metrics::SCALE_WIDTH),
        }
    }

    /// The size a fader needs for a track of `length`.
    pub fn desired_size(size: FaderSize, scale: bool, length: f32) -> Vec2 {
        let thumb = size.thumb();
        let scale_w = if scale {
            metrics::SCALE_WIDTH + metrics::SCALE_GAP
        } else {
            0.0
        };
        vec2(scale_w + thumb.x, length + thumb.y)
    }

    /// The y of position `p` (0 top … 1 bottom) on the track.
    pub fn y(&self, p: f32) -> f32 {
        self.track.top() + p.clamp(0.0, 1.0) * self.track.height()
    }

    /// The thumb rect at level `db`.
    pub fn thumb_rect(&self, db: f32) -> Rect {
        let y = self.y(db_to_p(db));
        Rect::from_min_size(pos2(self.thumb_left, y - self.thumb.y / 2.0), self.thumb)
    }
}

/// Paints the scale labels of the taper points right-aligned at `right`, each centred on its
/// position of `layout`.
pub fn paint_scale(painter: &Painter, layout: &Layout, right: f32) {
    let ppp = painter.pixels_per_point();
    for (db, p) in DBT {
        let g = painter.layout_job(fonts::layout_job(
            &scale_label(db),
            &SCALE_TEXT,
            Color32::PLACEHOLDER,
        ));
        let y = layout.y(p) - g.size().y / 2.0;
        let pos = pos2(px::snap(right - g.size().x, ppp), px::snap(y, ppp));
        painter.galley(pos, g, fonts::style_color(&SCALE_TEXT));
    }
}

/// Paints the thumb of `size` into `rect` (its size): two trapezoids meeting at the waist,
/// grooves, the value line across the waist.
pub fn paint_thumb(painter: &Painter, rect: Rect, size: FaderSize, look: &Look) {
    let ppp = painter.pixels_per_point();
    let rect = px::snap_rect(rect, ppp);
    let (w, h) = (rect.width(), rect.height());
    let o = rect.min;
    let p = |x: f32, y: f32| pos2(o.x + x, o.y + y);
    let inset = (w - size.waist()) / 2.0;
    // The design's SVG: 1 px outline on half-pixel coordinates, the halves 2 px apart.
    let half = 0.5 * line::WIDTH;
    let mid = h / 2.0;
    let edge = Stroke::new(line::WIDTH, look.edge);
    let top = vec![
        p(half, half),
        p(w - half, half),
        p(w - inset - half, mid - line::WIDTH),
        p(inset + half, mid - line::WIDTH),
    ];
    let bottom = vec![
        p(inset + half, mid + line::WIDTH),
        p(w - inset - half, mid + line::WIDTH),
        p(w - half, h - half),
        p(half, h - half),
    ];
    painter.add(Shape::convex_polygon(top, look.thumb, edge));
    painter.add(Shape::convex_polygon(bottom, look.thumb, edge));
    for &(dy, dx) in size.grooves() {
        let x = o.x + dx..=o.x + w - dx;
        px::hline(painter, x.clone(), o.y + dy - half, look.grip);
        px::hline(painter, x, o.y + h - dy - half, look.grip);
    }
    let li = size.value_line_inset();
    let line_rect = Rect::from_min_max(
        p(li, mid - metrics::VALUE_LINE / 2.0),
        p(w - li, mid + metrics::VALUE_LINE / 2.0),
    );
    px::fill(painter, line_rect, 0.0, look.value_line);
}

/// Paints a whole fader at level `db` into `rect`: scale (if `scale`), track, thumb, focus ring.
pub fn paint(painter: &Painter, rect: Rect, size: FaderSize, scale: bool, db: f32, look: &Look) {
    let layout = Layout::new(rect, size, scale);
    if let Some(right) = layout.scale_right {
        paint_scale(painter, &layout, right);
    }
    px::fill(painter, layout.track, radius::S, look.track);
    let thumb = layout.thumb_rect(db);
    paint_thumb(painter, thumb, size, look);
    if look.focus_ring {
        focus::paint_ring(painter, thumb, 0.0);
    }
}

// ---------------------------------------------------------------------------------------------
// The fader widget

/// A fader for a level in dB: `ui.add(Fader::new(&mut track.volume_db, "Lautstärke Lead"))`.
///
/// The response is `changed()` when the level changed; `drag_started()` / `drag_stopped()` mark
/// a gesture (fast-path commands while it lasts, one undo step at the end).
pub struct Fader<'a> {
    db: &'a mut f32,
    name: String,
    size: FaderSize,
    scale: bool,
    length: f32,
}

impl<'a> Fader<'a> {
    /// A normal fader for `db` (−70 = −∞ … +6) with the German accessible `name`, without scale,
    /// track [`metrics::TRACK_LENGTH`] long.
    pub fn new(db: &'a mut f32, name: impl Into<String>) -> Self {
        Self {
            db,
            name: name.into(),
            size: FaderSize::Normal,
            scale: false,
            length: metrics::TRACK_LENGTH,
        }
    }

    /// The thumb size.
    #[must_use]
    pub fn size(mut self, size: FaderSize) -> Self {
        self.size = size;
        self
    }

    /// The compact thumb (24 × 28).
    #[must_use]
    pub fn compact(self) -> Self {
        self.size(FaderSize::Compact)
    }

    /// Shows the dB scale on the left.
    #[must_use]
    pub fn scale(mut self, scale: bool) -> Self {
        self.scale = scale;
        self
    }

    /// Track length in points (the fader is one thumb height taller).
    #[must_use]
    pub fn track_length(mut self, length: f32) -> Self {
        self.length = length.max(1.0);
        self
    }

    /// The size of the fader in points.
    pub fn desired_size(&self) -> Vec2 {
        Layout::desired_size(self.size, self.scale, self.length)
    }
}

/// The text of a level with unit, for tooltip and AccessKit: "−3.2 dB", "−∞ dB".
pub fn db_text(db: f32) -> String {
    format!("{} dB", fmt_db(db))
}

/// Parses dB text entry: [`parse_db`], −∞ as [`VOLUME_DB_MIN`], clamped and rounded
/// ([`stored_db`]).
pub fn parse_level(text: &str) -> Option<f32> {
    parse_db(text).map(stored_db)
}

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, mut response) =
            ui.allocate_exact_size(self.desired_size(), sense(ui, Sense::click_and_drag()));
        let id = response.id;
        let ctx = ui.ctx().clone();
        let layout = Layout::new(rect, self.size, self.scale);
        let old = *self.db;
        let mut db = old;
        let live = !is_still(ui) && response.enabled();

        if live && !entry::is_open(&ctx, id) {
            take_focus_on_press(&response);
            match ClickAction::of(&response) {
                ClickAction::Reset => db = 0.0,
                ClickAction::TextEntry => entry::open(&ctx, id, fmt_db(db)),
                ClickAction::None => {}
            }
            let len = f64::from(layout.track.height());
            let up = f64::from(1.0 - db_to_p(db));
            if let Some(u) = drag_gesture(&response, || ValueDrag::span(up, 0.0, 1.0, len)) {
                db = p_to_db(1.0 - u as f32);
            }
            let steps = key_steps(ui, &response);
            if steps != 0.0 {
                db = db.max(VOLUME_DB_MIN) + steps as f32;
            }
            if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                entry::open(&ctx, id, fmt_db(db));
            }
            let (ak_steps, ak_set) = accesskit_requests(ui, id);
            db += ak_steps as f32;
            if let Some(v) = ak_set {
                db = v as f32;
            }
            if response.hovered() || response.dragged() {
                ui.set_cursor_icon(CursorIcon::ResizeVertical);
            }
        }
        let shown = stored_db(db);

        let st = State::of(ui, &response);
        let lk = look(st);
        paint(ui.painter(), rect, self.size, self.scale, shown, &lk);
        let thumb = layout.thumb_rect(shown);
        if live && entry::is_open(&ctx, id) {
            if let entry::Outcome::Commit(t) =
                entry::show_floating(&ctx, id, thumb, &self.name, thumb.width())
                && let Some(v) = parse_level(&t)
            {
                db = v;
            }
        } else if live && response.dragged() {
            tooltip::show_value(&ctx, id, thumb, tooltip::Placement::Above, &db_text(shown));
        }

        let db = stored_db(db);
        if db != old {
            *self.db = db;
            response.mark_changed();
        }
        expose_value(
            &response,
            Role::Slider,
            &self.name,
            f64::from(*self.db),
            db_text(*self.db),
            f64::from(VOLUME_DB_MIN)..=f64::from(VOLUME_DB_MAX),
            0.1,
        );
        response
    }
}

// ---------------------------------------------------------------------------------------------
// The value under the fader

/// Text style of the level value. The design sets it in Mono 11, which has no token; `value.m`
/// (Mono 12) is the nearest (contract change request).
pub const VALUE_TEXT: TypeStyle = typography::VALUE_M;

/// The level under a fader ("0.0", "−3.2", "−∞"): double-click opens text entry in dB in place,
/// Alt-click sets 0.0 dB. A plain click is reported (`clicked()`), so the caller can reset the
/// clip indicator ("bleibt rot bis Klick auf Anzeige oder Wert").
pub struct DbValue<'a> {
    db: &'a mut f32,
    name: String,
    color: Color32,
    width: Option<f32>,
}

impl<'a> DbValue<'a> {
    /// The value of `db` with the German accessible `name` ("Lautstärke Lead, Wert").
    pub fn new(db: &'a mut f32, name: impl Into<String>) -> Self {
        Self {
            db,
            name: name.into(),
            color: color32(color::TEXT_1),
            width: None,
        }
    }

    /// Text colour (default `text.1`).
    #[must_use]
    pub fn color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// Width in points; default: wide enough for "−48.0", so the layout does not jump.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
}

impl Widget for DbValue<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let widest = format!("{MINUS}48.0");
        let w = self
            .width
            .unwrap_or_else(|| galley(ui, &widest, &VALUE_TEXT).size().x + 2.0 * space::SPACE_1);
        let size = vec2(w, VALUE_TEXT.line_height);
        let (rect, mut response) = ui.allocate_exact_size(size, sense(ui, Sense::click()));
        let id = response.id;
        let ctx = ui.ctx().clone();
        let old = *self.db;
        let mut db = old;
        let live = !is_still(ui) && response.enabled();
        if live && !entry::is_open(&ctx, id) {
            match ClickAction::of(&response) {
                ClickAction::Reset => db = 0.0,
                ClickAction::TextEntry => entry::open(&ctx, id, fmt_db(db)),
                ClickAction::None => {}
            }
            if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                entry::open(&ctx, id, fmt_db(db));
            }
        }
        let painter = ui.painter();
        let ppp = ui.pixels_per_point();
        if live && entry::is_open(&ctx, id) {
            px::fill(painter, rect, radius::S, color32(color::SURFACE_0));
            paint_entry_ring(painter, rect, radius::S);
            let inner = rect.shrink2(vec2(space::SPACE_1, 0.0));
            if let entry::Outcome::Commit(t) = entry::show(ui, id, inner, &self.name)
                && let Some(v) = parse_level(&t)
            {
                db = v;
            }
        } else {
            let st = State::of(ui, &response);
            let color = if st.disabled {
                color32(color::TEXT_4)
            } else {
                self.color
            };
            let g = galley(ui, &fmt_db(*self.db), &VALUE_TEXT);
            let pos = text_pos(rect, &g, rect.center().x - g.size().x / 2.0, ppp);
            painter.galley(pos, g, color);
            if st.focused {
                focus::paint_ring(painter, rect, radius::S);
            }
        }
        let db = stored_db(db);
        if db != old {
            *self.db = db;
            response.mark_changed();
        }
        let text = db_text(*self.db);
        let enabled = response.enabled();
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::SpinButton, enabled, &self.name);
            info.value = Some(f64::from(*self.db));
            info.current_text_value = Some(text.clone());
            info
        });
        response
    }
}

// ---------------------------------------------------------------------------------------------
// Gallery

/// The samples of the design (`faders`): label, state, level.
const SAMPLES: [(&str, State, f32); 4] = [
    ("standard", State::REST, 0.0),
    ("hover", State::HOVERED, -3.2),
    ("ziehen", State::PRESSED, -3.2),
    ("deaktiviert", State::DISABLED, -6.0),
];

/// One still fader with scale, for the gallery: `state` at `db`. Not interactive; AccessKit sees
/// an image named `name`.
pub fn still(ui: &mut Ui, name: &str, state: State, db: f32, size: FaderSize) -> Response {
    let desired = Layout::desired_size(size, true, metrics::TRACK_LENGTH);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Image, false, name));
    paint(ui.painter(), rect, size, true, db, &look(state));
    response
}

/// The card content "Fader und Pegelanzeige": the four fader samples, a 1 px divider, the four
/// meter samples ([`meter::gallery_samples`]) in one row if the card is wide enough (else the
/// meters in a row of their own), then a live fader driving a live meter.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    // `gap: 28px` of the design's row: space.6 + space.1.
    let gap = space::SPACE_6 + space::SPACE_1;
    let fader = Layout::desired_size(FaderSize::Normal, true, metrics::TRACK_LENGTH);
    let widths: Vec<f32> = SAMPLES
        .iter()
        .map(|(label, _, db)| {
            fader
                .x
                .max(text_width(ui, label, &typography::VALUE_S))
                .max(text_width(ui, &fmt_db(*db), &VALUE_TEXT))
        })
        .collect();
    let faders_w: f32 = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
    let meters_w = meter::gallery_width(ui, gap);
    let one_row = faders_w + 2.0 * gap + line::WIDTH + meters_w <= ui.available_width();
    // Columns align at their bottom edge, as the design's `align-items: flex-end`.
    let meter_top = (fader.y - meter::metrics::LENGTH - space::SPACE_2).max(0.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for ((label, st, db), w) in SAMPLES.into_iter().zip(widths) {
            column(ui, w, Align::Center, space::SPACE_2, |ui| {
                still(ui, &format!("Fader · {label}"), st, db, FaderSize::Normal);
                let c = if st.disabled {
                    color::TEXT_4
                } else {
                    color::TEXT_1
                };
                value_label(ui, &fmt_db(db), c);
                caption(ui, label);
            });
        }
        if one_row {
            divider(ui);
            meter::gallery_samples(ui, meter_top);
        }
    });
    if !one_row {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            meter::gallery_samples(ui, 0.0);
        });
    }
    live_row(ui);
}

/// The design's 1 px divider between faders and meters, as high as a fader sample column.
fn divider(ui: &mut Ui) {
    let h = Layout::desired_size(FaderSize::Normal, true, metrics::TRACK_LENGTH).y
        + 2.0 * space::SPACE_2
        + VALUE_TEXT.line_height
        + typography::VALUE_S.line_height;
    let (rect, _) = ui.allocate_exact_size(vec2(line::WIDTH, h), Sense::hover());
    px::vline(
        ui.painter(),
        rect.left(),
        rect.y_range(),
        color32(color::LINE_1),
    );
}

/// The live values of the gallery card, kept in egui memory.
#[derive(Clone, Debug)]
struct Live {
    db: f32,
    compact: f32,
    meter: meter::MeterState,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            db: -3.2,
            compact: 0.0,
            meter: meter::MeterState::new(),
        }
    }
}

/// The level of the test tone the live fader drives into the live meter (−3 dBFS): the meter
/// shows the fader's level 3 dB lower, so the fader above +3 dB clips.
const TEST_TONE_DB: f32 = -3.0;

fn live_row(ui: &mut Ui) {
    let id = Id::unique("komponist_gallery_fader_live");
    let mut live: Live = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_6;
        let fader = Layout::desired_size(FaderSize::Normal, true, metrics::TRACK_LENGTH);
        column(ui, fader.x, Align::Center, space::SPACE_2, |ui| {
            ui.add(Fader::new(&mut live.db, "Lautstärke").scale(true));
            if ui
                .add(DbValue::new(&mut live.db, "Lautstärke, Wert"))
                .clicked()
            {
                live.meter.reset();
            }
            caption(ui, "live");
        });
        let w = text_width(ui, "Testton \u{2212}3 dBFS", &typography::VALUE_S);
        column(ui, w, Align::Center, space::SPACE_2, |ui| {
            let gain = klang_dsp::units::db_to_gain(TEST_TONE_DB + live.db);
            let peak = if live.db <= VOLUME_DB_MIN { 0.0 } else { gain };
            // The bars span the fader's track, so the meter reads on the fader's scale.
            let led = meter::metrics::LED_HEIGHT + meter::metrics::LED_GAP;
            let thumb = FaderSize::Normal.thumb().y;
            ui.add_space(thumb / 2.0 - led);
            ui.add(
                meter::Meter::new(&mut live.meter, [peak, peak * 0.94], "Pegel Testton")
                    .length(metrics::TRACK_LENGTH + led),
            );
            ui.add_space(thumb / 2.0);
            let c = if live.meter.clipped() {
                color::METER_CLIP
            } else {
                color::TEXT_1
            };
            value_label(ui, &fmt_db(live.meter.peak_db()), c);
            caption(ui, "Testton \u{2212}3 dBFS");
        });
        let w = FaderSize::Compact
            .thumb()
            .x
            .max(text_width(ui, "kompakt", &typography::VALUE_S));
        column(ui, w, Align::Center, space::SPACE_2, |ui| {
            ui.add(
                Fader::new(&mut live.compact, "Lautstärke kompakt")
                    .compact()
                    .track_length(metrics::TRACK_LENGTH / 2.0),
            );
            ui.add(DbValue::new(&mut live.compact, "Lautstärke kompakt, Wert"));
            caption(ui, "kompakt");
        });
    });
    ui.data_mut(|d| d.insert_temp(id, live));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn taper_matches_the_prototype() {
        for (db, p) in DBT {
            assert!(near(db_to_p(db), p), "{db} dB");
        }
        assert!(near(db_to_p(-3.0), 0.22));
        assert!(near(db_to_p(-36.0), 0.75));
        assert_eq!(db_to_p(12.0), 0.0);
        assert_eq!(db_to_p(f32::NEG_INFINITY), 1.0);
        assert_eq!(db_to_p(-80.0), 1.0);
        assert_eq!(db_to_p(f32::NAN), 1.0);
        assert!(near(p_to_db(0.0), 6.0));
        assert!(near(p_to_db(0.14), 0.0));
        assert!(near(p_to_db(0.375), -9.0));
        assert_eq!(p_to_db(0.995), f32::NEG_INFINITY);
        assert_eq!(p_to_db(1.0), f32::NEG_INFINITY);
        assert!(near(p_to_db(-1.0), 6.0));
    }

    #[test]
    fn taper_round_trips() {
        // p_to_db(db_to_p(x)) = x over the whole audible range, in 0.05 dB steps.
        let mut db = 6.0f32;
        while db > -69.0 {
            let back = p_to_db(db_to_p(db));
            assert!((back - db).abs() < 1e-3, "{db} → {back}");
            db -= 0.05;
        }
        // And the other way round over the positions above silence.
        for i in 0..=995 {
            let p = i as f32 / 1000.0;
            if p >= SILENCE_POSITION {
                break;
            }
            assert!((db_to_p(p_to_db(p)) - p).abs() < 1e-4, "{p}");
        }
        // −∞ ↔ the bottom.
        assert_eq!(p_to_db(db_to_p(f32::NEG_INFINITY)), f32::NEG_INFINITY);
        assert_eq!(db_to_p(p_to_db(1.0)), 1.0);
        assert_eq!(stored_db(p_to_db(db_to_p(VOLUME_DB_MIN))), VOLUME_DB_MIN);
    }

    #[test]
    fn stored_levels() {
        assert_eq!(stored_db(f32::NEG_INFINITY), VOLUME_DB_MIN);
        assert_eq!(stored_db(-3.2345), -3.2);
        assert_eq!(stored_db(9.0), VOLUME_DB_MAX);
        assert_eq!(parse_level("-inf"), Some(VOLUME_DB_MIN));
        assert_eq!(parse_level("−3,25 dB"), Some(-3.3));
        assert_eq!(parse_level("+12"), Some(VOLUME_DB_MAX));
        assert_eq!(parse_level("laut"), None);
        assert_eq!(db_text(-3.2), "\u{2212}3.2 dB");
        assert_eq!(db_text(VOLUME_DB_MIN), "\u{2212}\u{221e} dB");
    }

    #[test]
    fn scale_labels() {
        let labels: Vec<String> = DBT.iter().map(|(db, _)| scale_label(*db)).collect();
        assert_eq!(labels, ["+6", "0", "−6", "−12", "−24", "−48", "−∞"]);
    }

    #[test]
    fn layout_follows_the_design() {
        // The design's sample: 54 × 244, track at x 36–40 from y 22, 200 long; thumb at x 22.
        let size = Layout::desired_size(FaderSize::Normal, true, 200.0);
        assert_eq!(size, vec2(54.0, 244.0));
        let l = Layout::new(
            Rect::from_min_size(egui::Pos2::ZERO, size),
            FaderSize::Normal,
            true,
        );
        assert_eq!(
            l.track,
            Rect::from_min_max(pos2(36.0, 22.0), pos2(40.0, 222.0))
        );
        assert_eq!(l.thumb_left, 22.0);
        assert_eq!(l.scale_right, Some(20.0));
        // Thumb top = p · 200 (`top: p*200`).
        assert!((l.thumb_rect(0.0).top() - 28.0).abs() < 1e-4);
        assert!((l.thumb_rect(-6.0).top() - 60.0).abs() < 1e-4);
        let c = Layout::desired_size(FaderSize::Compact, false, 100.0);
        assert_eq!(c, vec2(24.0, 128.0));
    }

    #[test]
    fn looks_follow_the_design() {
        let rest = look(State::REST);
        assert_eq!(rest.thumb, color32(color::SURFACE_4));
        assert_eq!(rest.value_line, color32(color::TEXT_1));
        assert_eq!(look(State::HOVERED).thumb, color32(color::SURFACE_5));
        assert_eq!(look(State::PRESSED).value_line, color32(color::ACCENT));
        let d = look(State::DISABLED);
        assert_eq!(
            (d.thumb, d.value_line),
            (color32(color::SURFACE_3), color32(color::TEXT_4))
        );
        assert!(look(State::FOCUSED).focus_ring);
        assert!(!rest.focus_ring);
    }
}
