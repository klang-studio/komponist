//! Transport bar (`size.transport` 44; `Screens.dc.html` shell markup and `tp`).
//!
//! Left to right, 16 apart, padding 0 12, on `surface.1`: wordmark "komponist" (`type.track`),
//! project name (`type.ui`, `text.2`) and the unsaved dot; Play / Stop / Record (30 × 28,
//! geometric shapes); Loop, Metronom, Vorzähler 1 (icon toggle buttons, count-in with its bar count); the padded position
//! `fmt_pos` (`type.value.l`) and the time (`type.value.m`, `text.3`); the tempo field (BPM);
//! the meter "4/4". On the right: "Kein Audio-Gerät" (only without device), the master display
//! (oscilloscope or spectrum, #34), CPU load, the master meter, the "⋯" menu. The texts come from `locales/*/transport.ftl` (`docs/I18N.md`); German
//! is quoted here.

use std::sync::Arc;

use egui::{
    Align, CursorIcon, Galley, Layout, Mesh, Painter, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_audio::AudioState;
use komponist_engine::TransportState;
use komponist_i18n::Lang;
use komponist_model::format::{fmt_pos, fmt_time};
use komponist_model::{Project, Tempo, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::state::{self, State};
use komponist_ui::theme::corner_radius;
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::meter::Meter;
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, icons, px};

use crate::actions::{Action, EditAction, ProjectAction, SettingsAction, TransportAction};
use crate::keymap::Keymap;
use crate::master_scope::{MasterDisplay, Picture};
use crate::meters::Meters;
use crate::metrics::transport as m;

/// The DAW's name in the wordmark and window title (ARCHITECTURE §13.6).
pub const WORDMARK: &str = "komponist";

/// What the transport bar shows.
pub struct TransportModel<'a> {
    /// The project (name, tempo, meter, loop, metronome, count-in).
    pub project: &'a Arc<Project>,
    /// Unsaved changes.
    pub dirty: bool,
    /// Transport state.
    pub state: TransportState,
    /// Playhead.
    pub playhead: Tick,
    /// CPU load, 1.0 = the whole buffer period.
    pub cpu: f32,
    /// The audio system's state.
    pub audio: &'a AudioState,
    /// Shortcut labels.
    pub keymap: &'a Keymap,
    /// The language of the bar's texts.
    pub lang: Lang,
    /// What the master display shows (the settings' choice).
    pub master_display: MasterDisplay,
}

impl TransportModel<'_> {
    /// Play and record work (an output runs).
    pub fn audio_ok(&self) -> bool {
        !matches!(self.audio, AudioState::NoDevice { .. })
    }
}

/// Draws the transport bar into `rect`; what the user does goes to `actions`.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    model: &TransportModel<'_>,
    meters: &mut Meters,
    actions: &mut Vec<Action>,
) {
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_1));
    let inner = rect.shrink2(vec2(space::SPACE_3, 0.0));
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = space::SPACE_4;
    let ui = &mut row;

    wordmark(ui, model);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_1;
        transport_buttons(ui, model, actions);
    });
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_1;
        toggles(ui, model, actions);
    });
    separator(ui);
    position(ui, model);
    separator(ui);
    tempo(ui, model, actions);
    meter_box(ui, model);

    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        menu(ui, model, actions);
        ui.add(
            Meter::new(
                &mut meters.master,
                Meters::NO_PEAKS,
                model.lang.transport_master(),
            )
            .header()
            .horizontal()
            .length(m::MASTER_METER_LENGTH),
        );
        cpu(ui, model);
        master_scope(ui, model, meters, actions);
        if !model.audio_ok() {
            no_device_chip(ui, model.lang);
        }
    });
}

/// The y of the first baseline of `galley` placed at `top`.
fn baseline(galley: &Galley, top: f32) -> f32 {
    galley
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first().map(|g| top + r.pos.y + g.pos.y))
        .unwrap_or(top + galley.size().y)
}

/// Places galleys left to right on one baseline (the design's `align-items: baseline`), the
/// first one centred vertically in `rect`; returns their rects.
fn paint_on_baseline(ui: &Ui, rect: Rect, parts: &[(Arc<Galley>, [u8; 4], f32)]) -> Vec<Rect> {
    let ppp = ui.pixels_per_point();
    let Some((first, _, _)) = parts.first() else {
        return Vec::new();
    };
    let top = rect.center().y - first.size().y / 2.0;
    let base = baseline(first, top);
    let mut x = rect.left();
    let mut out = Vec::with_capacity(parts.len());
    for (galley, color, gap_after) in parts {
        let y = base - (baseline(galley, 0.0));
        let pos = pos2(px::snap(x, ppp), px::snap(y, ppp));
        ui.painter()
            .galley(pos, Arc::clone(galley), color32(*color));
        out.push(Rect::from_min_size(pos, galley.size()));
        x += galley.size().x + gap_after;
    }
    out
}

/// A live readout for AccessKit (`Role::Status`): `name` with the current `value`
/// ("Position" = "10.2.120").
fn readout(ui: &Ui, rect: Rect, name: &str, value: &str) -> Response {
    let id = ui.scope_id().with(("komponist_readout", name));
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Status, true, name);
        info.current_text_value = Some(value.to_owned());
        info
    });
    response
}

/// A static text for AccessKit (`Role::Label`, the text as its value).
fn text_node(ui: &Ui, rect: Rect, key: &str, text: &str) -> Response {
    let id = ui.scope_id().with(("komponist_text", key));
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

fn wordmark(ui: &mut Ui, model: &TransportModel<'_>) {
    let (rect, _) = ui.allocate_exact_size(vec2(m::WORDMARK_WIDTH, size::CONTROL), Sense::hover());
    let mark = w::styled_galley(ui, WORDMARK, &typography::TRACK);
    let name = w::styled_galley(ui, &model.project.name, &typography::UI);
    let rects = paint_on_baseline(
        ui,
        rect,
        &[
            (mark, color::TEXT_1, m::WORDMARK_GAP),
            (name, color::TEXT_2, m::WORDMARK_GAP),
        ],
    );
    text_node(ui, rects[0], "wordmark", WORDMARK);
    let project = if model.dirty {
        model.lang.transport_project_unsaved(&model.project.name)
    } else {
        model.project.name.clone()
    };
    text_node(ui, rects[1], "project", &project);
    if model.dirty {
        let x = rects[1].right() + m::WORDMARK_GAP + m::DIRTY_DOT / 2.0;
        let c = pos2(x, rect.center().y);
        ui.painter()
            .circle_filled(c, m::DIRTY_DOT / 2.0, color32(color::TEXT_3));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Play,
    Stop,
    Record,
}

/// Play, Stop or Record: 30 × 28, `radius.m`, the shape centred (`tp` `playS`, `recS`).
fn shape_button(
    ui: &mut Ui,
    shape: Shape,
    model: &TransportModel<'_>,
    name: &str,
    shortcut: Option<String>,
) -> Response {
    let enabled = shape == Shape::Stop || model.audio_ok();
    let (rect, response) = ui.allocate_exact_size(
        vec2(m::BUTTON[0], m::BUTTON[1]),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let mut st = State::of(ui, &response);
    st.disabled |= !enabled;
    let playing = model.state != TransportState::Stopped;
    let recording = model.state == TransportState::Recording;
    let count_in = model.state == TransportState::CountIn;
    let (fill, ink, on) = match shape {
        Shape::Play => (
            state::surface_for(if playing { 4 } else { 3 }, st),
            if st.disabled {
                color32(color::TEXT_4)
            } else if playing {
                color32(color::STATE_PLAY)
            } else {
                state::text_for(color::TEXT_2, st)
            },
            playing,
        ),
        Shape::Stop => (
            state::surface_for(3, st),
            state::text_for(color::TEXT_2, st),
            false,
        ),
        Shape::Record => (
            if st.disabled {
                color32(color::SURFACE_2)
            } else if recording {
                color32(color::STATE_RECORD)
            } else if count_in {
                color32(color::STATE_RECORD_SURFACE)
            } else {
                state::surface_for(3, st)
            },
            if st.disabled {
                color32(color::TEXT_4)
            } else if recording {
                color32(color::TEXT_1)
            } else {
                color32(color::STATE_RECORD)
            },
            recording || count_in,
        ),
    };
    let painter = ui.painter();
    px::fill(painter, rect, radius::M, fill);
    let c = rect.center();
    match shape {
        Shape::Play => {
            let [tw, th] = m::PLAY_TRIANGLE;
            let r = Rect::from_center_size(
                pos2(c.x + m::PLAY_TRIANGLE_OFFSET / 2.0, c.y),
                vec2(tw, th),
            );
            icons::paint_play(painter, r, ink);
        }
        Shape::Stop => {
            icons::paint_stop(
                painter,
                Rect::from_center_size(c, vec2(m::STOP_SQUARE, m::STOP_SQUARE)),
                ink,
            );
        }
        Shape::Record => {
            icons::paint_record(
                painter,
                Rect::from_center_size(c, vec2(m::RECORD_DOT, m::RECORD_DOT)),
                ink,
            );
        }
    }
    if st.focused && !st.disabled {
        focus::paint_ring(painter, rect, radius::M);
    }
    response.widget_info(|| {
        if shape == Shape::Stop {
            WidgetInfo::labeled(Role::Button, enabled, name)
        } else {
            WidgetInfo::selected(Role::Button, enabled, on, name)
        }
    });
    let tip = match &shortcut {
        Some(sc) => Tip::new(name).shortcut(sc),
        None => Tip::new(name),
    };
    tip.attach(&response);
    response
}

fn transport_buttons(ui: &mut Ui, model: &TransportModel<'_>, actions: &mut Vec<Action>) {
    let t = model.lang;
    let label = |a: TransportAction| model.keymap.label_for(&Action::Transport(a), t);
    if shape_button(
        ui,
        Shape::Play,
        model,
        t.transport_play(),
        label(TransportAction::TogglePlay),
    )
    .clicked()
    {
        actions.push(Action::Transport(TransportAction::TogglePlay));
    }
    if shape_button(ui, Shape::Stop, model, t.transport_stop(), None).clicked() {
        actions.push(Action::Transport(TransportAction::Stop));
    }
    if shape_button(
        ui,
        Shape::Record,
        model,
        t.transport_record(),
        label(TransportAction::ToggleRecord),
    )
    .clicked()
    {
        actions.push(Action::Transport(TransportAction::ToggleRecord));
    }
}

fn toggles(ui: &mut Ui, model: &TransportModel<'_>, actions: &mut Vec<Action>) {
    let p = model.project;
    let t = model.lang;
    // Icon toggles: the German label stays the AccessKit name, the tooltip carries it with the
    // shortcut. The count-in shows its bar count beside the icon only while it is on; the room
    // for the digit stays reserved while it is off, so the bar does not jump (owner, 2026-11-15).
    let bars = p.count_in_bars;
    let bars_text = bars.max(1).to_string();
    // Room for the widest count: the edit's limit, or a longer value a project carries.
    let room_text = "8".repeat(
        bars.max(komponist_model::edits::MAX_COUNT_IN_BARS)
            .to_string()
            .len(),
    );
    let count_in_tip = if bars > 0 {
        t.transport_count_in_tip(i64::from(bars))
    } else {
        t.transport_count_in_off_tip().to_owned()
    };
    let items = [
        (
            Icon::Repeat,
            t.transport_loop(),
            p.loop_on,
            TransportAction::ToggleLoop,
            None,
        ),
        (
            Icon::Metronome,
            t.transport_metronome(),
            p.metronome,
            TransportAction::ToggleMetronome,
            None,
        ),
        (
            Icon::Timer,
            t.transport_count_in(),
            bars > 0,
            TransportAction::ToggleCountIn,
            Some(bars_text.as_str()),
        ),
    ];
    for (icon, name, on, action, badge) in items {
        let action = Action::Transport(action);
        let shortcut = model.keymap.label_for(&action, t);
        let mut button = Button::icon(icon, name).on(on);
        if let Some(badge) = badge {
            button = button.badge(badge).badge_room(&room_text).badge_shown(on);
        }
        if badge.is_some() {
            let mut tip = Tip::new(&count_in_tip);
            if let Some(sc) = shortcut.as_deref() {
                tip = tip.shortcut(sc);
            }
            button = button.tip(tip);
        } else if let Some(sc) = shortcut.as_deref() {
            button = button.shortcut(sc);
        }
        if ui.add(button).clicked() {
            actions.push(action);
        }
    }
}

fn separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(komponist_tokens::line::WIDTH, m::SEPARATOR_HEIGHT),
        Sense::hover(),
    );
    px::vline(
        ui.painter(),
        rect.left(),
        rect.y_range(),
        color32(color::LINE_1),
    );
}

/// Position and time: `fmt_pos` padded ("10.2.120", stable width), time `fmt_time`.
pub fn position_texts(project: &Project, playhead: Tick) -> (String, String) {
    let map = project.tempo_map();
    (
        fmt_pos(playhead, project.meter),
        fmt_time(map.tick_to_seconds(playhead)),
    )
}

fn position(ui: &mut Ui, model: &TransportModel<'_>) {
    let (rect, _) = ui.allocate_exact_size(vec2(m::POSITION_WIDTH, size::CONTROL), Sense::hover());
    let (pos, time) = position_texts(model.project, model.playhead);
    let recording = matches!(
        model.state,
        TransportState::Recording | TransportState::CountIn
    );
    let pos_g = w::styled_galley(ui, &pos, &typography::VALUE_L);
    let time_g = w::styled_galley(ui, &time, &typography::VALUE_M);
    let rects = paint_on_baseline(
        ui,
        rect,
        &[
            (
                pos_g,
                if recording {
                    color::STATE_RECORD
                } else {
                    color::TEXT_1
                },
                m::POSITION_GAP,
            ),
            (time_g, color::TEXT_3, 0.0),
        ],
    );
    readout(ui, rects[0], model.lang.transport_position(), &pos);
    readout(ui, rects[1], model.lang.transport_time(), &time);
}

fn tempo(ui: &mut Ui, model: &TransportModel<'_>, actions: &mut Vec<Action>) {
    let mut bpm = model.project.tempo.bpm;
    let response = ui.add(
        NumberField::new(&mut bpm, model.lang.transport_tempo())
            .unit("BPM")
            .range(Tempo::MIN_BPM..=Tempo::MAX_BPM)
            .step(0.5)
            .decimals(2)
            .default_value(Tempo::DEFAULT.bpm)
            .width(m::TEMPO_WIDTH),
    );
    if response.drag_started() {
        actions.push(Action::Edit(EditAction::BeginGesture {
            label: "undo-tempo".to_owned(),
        }));
    }
    if response.changed() {
        actions.push(Action::Edit(EditAction::SetProjectProp(
            komponist_model::edits::ProjectProp::Tempo(Tempo::new(bpm)),
        )));
    }
    if response.drag_stopped() {
        actions.push(Action::Edit(EditAction::EndGesture));
    }
}

fn meter_box(ui: &mut Ui, model: &TransportModel<'_>) {
    let meter = model.project.meter;
    let text = format!("{}/{}", meter.num, meter.den);
    let galley = w::styled_galley(ui, &text, &typography::VALUE_M);
    let width = galley.size().x.ceil() + 2.0 * space::SPACE_2;
    let (rect, _) = ui.allocate_exact_size(vec2(width, size::CONTROL), Sense::hover());
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_2));
    let pos = w::text_pos_centered(rect, &galley, ui.pixels_per_point());
    ui.painter().galley(pos, galley, color32(color::TEXT_1));
    readout(ui, rect, model.lang.transport_meter(), &text);
}

/// CPU load as shown: "18 %" ("18%" in English).
pub fn cpu_text(load: f32, t: Lang) -> String {
    let pct = if load.is_finite() {
        (load * 100.0).round().max(0.0)
    } else {
        0.0
    };
    t.transport_cpu_value(format!("{pct:.0}"))
}

fn cpu(ui: &mut Ui, model: &TransportModel<'_>) {
    let (load, t) = (model.cpu, model.lang);
    // Right-to-left layout: value, bar, label.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_2;
        let text = cpu_text(load, t);
        let value = w::styled_galley(ui, &text, &typography::VALUE_S);
        let (vr, _) =
            ui.allocate_exact_size(vec2(m::CPU_VALUE_WIDTH, size::CONTROL), Sense::hover());
        let pos = w::text_pos(vr, &value, vr.left(), ui.pixels_per_point());
        ui.painter().galley(pos, value, color32(color::TEXT_3));

        let [bw, bh] = m::CPU_BAR;
        let (br, _) = ui.allocate_exact_size(vec2(bw, size::CONTROL), Sense::hover());
        let bar = Rect::from_center_size(br.center(), vec2(bw, bh));
        px::fill(ui.painter(), bar, radius::S, color32(color::SURFACE_3));
        let f = load.clamp(0.0, 1.0);
        if f > 0.0 {
            let fill = Rect::from_min_size(bar.min, vec2(bw * f, bh));
            px::fill(ui.painter(), fill, 0.0, color32(color::TEXT_2));
        }

        let label = w::styled_galley(ui, t.transport_cpu(), &typography::LABEL);
        let (lr, _) =
            ui.allocate_exact_size(vec2(label.size().x.ceil(), size::CONTROL), Sense::hover());
        let pos = w::text_pos(lr, &label, lr.left(), ui.pixels_per_point());
        ui.painter()
            .galley(pos, label, fonts::style_color(&typography::LABEL));
        readout(ui, lr.union(vr), t.transport_cpu(), &text);
    });
}

/// The master display (#34; master-scope proposal, variant A „Fenster“, internal notes): a
/// `size.control` high box on `surface.0` with a 1 px `line.1` frame (`line.2` on hover) and
/// `radius.m`, the picture [`m::SCOPE_INSET`] inside. A click, or Space / Enter while it has
/// focus, switches between oscilloscope and spectrum. AccessKit: a button named after the picture
/// („Master-Anzeige: Oszilloskop“) whose description, the tooltip, says how to switch; the curve
/// itself is decorative.
fn master_scope(
    ui: &mut Ui,
    model: &TransportModel<'_>,
    meters: &mut Meters,
    actions: &mut Vec<Action>,
) {
    let (t, mode) = (model.lang, model.master_display);
    let (rect, response) =
        ui.allocate_exact_size(vec2(m::SCOPE_WIDTH, size::CONTROL), Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let st = State::of(ui, &response);
    let now = ui.input(|i| i.time);
    let picture = meters.master_scope.update(now, mode);
    paint_master_scope(ui.painter(), rect, mode, picture, st);
    let (name, tip) = match mode {
        MasterDisplay::Oscilloscope => (
            t.transport_scope_oscilloscope(),
            t.transport_scope_oscilloscope_tip(),
        ),
        MasterDisplay::Spectrum => (
            t.transport_scope_spectrum(),
            t.transport_scope_spectrum_tip(),
        ),
    };
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    Tip::new(tip).attach(&response);
    if response.clicked() {
        actions.push(Action::Settings(SettingsAction::ToggleMasterDisplay));
    }
}

/// Paints the master display into `rect` (see [`master_scope`]). Oscilloscope: the centre line
/// in `line.1` and the curve, 1 px `accent.default`. Spectrum: the grid at
/// [`m::SCOPE_GRID_HZ`] in `line.1`, covered by the fill in `accent.surface` under a 1 px
/// `accent.default` edge. Silence: one flat `text.4` line, in the middle (oscilloscope) or on the
/// floor (spectrum).
fn paint_master_scope(
    painter: &Painter,
    rect: Rect,
    mode: MasterDisplay,
    picture: Picture<'_>,
    st: State,
) {
    let ppp = painter.pixels_per_point();
    let lw = px::line_width(ppp);
    px::fill(painter, rect, radius::M, color32(color::SURFACE_0));
    let inner = px::snap_rect(rect.shrink(m::SCOPE_INSET), ppp);
    let clip = painter.with_clip_rect(inner);
    let middle = px::snap(inner.center().y - lw / 2.0, ppp);
    // The curve's 1 px stroke stays inside the picture at full scale.
    let (top, bottom) = (inner.top() + lw / 2.0, inner.bottom() - lw / 2.0);
    let x_at = |f: f32| inner.left() + f * inner.width();
    match mode {
        MasterDisplay::Oscilloscope => match picture {
            Picture::Wave(values) => {
                px::hline(&clip, inner.x_range(), middle, color32(color::LINE_1));
                let half = (bottom - top) / 2.0;
                let centre = middle + lw / 2.0;
                let last = (values.len().max(2) - 1) as f32;
                let points = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| pos2(x_at(i as f32 / last), centre - v * half))
                    .collect();
                clip.add(egui::Shape::line(
                    points,
                    Stroke::new(lw, color32(color::ACCENT)),
                ));
            }
            _ => px::hline(&clip, inner.x_range(), middle, color32(color::TEXT_4)),
        },
        MasterDisplay::Spectrum => {
            let span = (crate::spectrum::F_MAX / crate::spectrum::F_MIN).ln();
            for hz in m::SCOPE_GRID_HZ {
                let x = px::snap(x_at((hz / crate::spectrum::F_MIN).ln() / span), ppp);
                px::vline(&clip, x, inner.y_range(), color32(color::LINE_1));
            }
            match picture {
                Picture::Spectrum(values) => {
                    // One point per device pixel column, the highest value it covers: the
                    // 256 points are about three per pixel at 1×, which would zigzag.
                    let columns = ((inner.width() * ppp).round() as usize).clamp(2, values.len());
                    let points: Vec<_> = (0..columns)
                        .map(|c| {
                            let from = c * values.len() / columns;
                            let to = ((c + 1) * values.len() / columns).max(from + 1);
                            let v = values[from..to].iter().fold(0.0_f32, |m, v| m.max(*v));
                            // At the centre frequency of the points it covers.
                            let f = (from + to - 1) as f32 / 2.0 / (values.len() - 1) as f32;
                            pos2(x_at(f), bottom - v * (bottom - top))
                        })
                        .collect();
                    // The fill as a strip of quads down to the floor (the curve is not convex).
                    let fill = color32(color::ACCENT_SURFACE);
                    let mut mesh = Mesh::default();
                    for (i, p) in points.iter().enumerate() {
                        mesh.colored_vertex(*p, fill);
                        mesh.colored_vertex(pos2(p.x, inner.bottom()), fill);
                        if i > 0 {
                            let k = 2 * i as u32;
                            mesh.add_triangle(k - 2, k - 1, k);
                            mesh.add_triangle(k - 1, k + 1, k);
                        }
                    }
                    clip.add(mesh);
                    clip.add(egui::Shape::line(
                        points,
                        Stroke::new(lw, color32(color::ACCENT)),
                    ));
                }
                _ => px::hline(
                    &clip,
                    inner.x_range(),
                    inner.bottom() - lw,
                    color32(color::TEXT_4),
                ),
            }
        }
    }
    let frame = if st.hovered {
        color::LINE_2
    } else {
        color::LINE_1
    };
    painter.rect_stroke(
        px::snap_rect(rect, ppp),
        corner_radius(radius::M),
        Stroke::new(lw, color32(frame)),
        StrokeKind::Inside,
    );
    if st.focused {
        focus::paint_ring(painter, rect, radius::M);
    }
}

fn no_device_chip(ui: &mut Ui, t: Lang) {
    let font = w::font_at(&typography::SMALL, wm::MEDIUM);
    let text = w::galley(
        ui,
        t.transport_no_device(),
        font,
        typography::SMALL.line_height,
    );
    let width = wm::BUTTON_PADDING_X * 2.0 + m::ERROR_DOT + space::SPACE_2 + text.size().x.ceil();
    let (rect, _) = ui.allocate_exact_size(vec2(width, size::CONTROL), Sense::hover());
    let painter = ui.painter();
    px::fill(
        painter,
        rect,
        radius::M,
        color32(color::STATE_ERROR_SURFACE),
    );
    let dot = pos2(
        rect.left() + wm::BUTTON_PADDING_X + m::ERROR_DOT / 2.0,
        rect.center().y,
    );
    painter.circle_filled(dot, m::ERROR_DOT / 2.0, color32(color::STATE_ERROR));
    let x = rect.left() + wm::BUTTON_PADDING_X + m::ERROR_DOT + space::SPACE_2;
    painter.galley(
        w::text_pos(rect, &text, x, ui.pixels_per_point()),
        text,
        color32(color::TEXT_1),
    );
    text_node(ui, rect, "no_device", t.transport_no_device());
}

/// The "⋯" menu (prototype `menu`): new, open, save, export, start screen.
fn menu(ui: &mut Ui, model: &TransportModel<'_>, actions: &mut Vec<Action>) {
    let response = ui.add(Button::icon(Icon::Ellipsis, model.lang.transport_menu()));
    // The menu opens one frame after the click: a press and release delivered in one frame
    // (egui-mcp, fast touchpads) would otherwise count as a press outside the new menu and close
    // it at once (the context menu exempts only its own right-click).
    let pending = response.id.with("komponist_menu_pending");
    let ctx = ui.ctx().clone();
    if ctx
        .data_mut(|d| d.remove_temp::<bool>(pending))
        .unwrap_or(false)
    {
        ContextMenu::<Action>::open_at(&ctx, response.id, response.rect.left_bottom());
    }
    if response.clicked() {
        ctx.data_mut(|d| d.insert_temp(pending, true));
        ctx.request_repaint();
    }
    let t = model.lang;
    let p = |a| Action::Project(a);
    let entries_spec = [
        (t.transport_menu_new(), p(ProjectAction::New)),
        (t.transport_menu_open(), p(ProjectAction::Open)),
        (t.transport_menu_save(), p(ProjectAction::Save)),
        (t.transport_menu_save_as(), p(ProjectAction::SaveAs)),
        (t.transport_menu_collect(), p(ProjectAction::CollectAndSave)),
        (
            t.transport_menu_export(),
            p(ProjectAction::OpenExportDialog),
        ),
        (t.transport_menu_start(), p(ProjectAction::ShowStartScreen)),
    ];
    let labels: Vec<Option<String>> = entries_spec
        .iter()
        .map(|(_, a)| model.keymap.label_for(a, t))
        .collect();
    let entries: Vec<Entry<'_, Action>> = entries_spec
        .iter()
        .zip(&labels)
        .map(|((name, action), label)| {
            let e = Entry::action(name, action.clone());
            match label {
                Some(l) => e.shortcut(l),
                None => e,
            }
        })
        .collect();
    if let Some(action) = ContextMenu::new(t.transport_menu(), &entries).show_on(&response) {
        actions.push(action);
    }
}
