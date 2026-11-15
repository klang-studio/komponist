//! Transport bar (`size.transport` 44; `Screens.dc.html` shell markup and `tp`).
//!
//! Left to right, 16 apart, padding 0 12, on `surface.1`: wordmark "komponist" (`type.track`),
//! project name (`type.ui`, `text.2`) and the unsaved dot; Play / Stop / Record (30 × 28,
//! geometric shapes); Loop, Metronom, Vorzähler 1 (toggle buttons); the padded position
//! `fmt_pos` (`type.value.l`) and the time (`type.value.m`, `text.3`); the tempo field (BPM);
//! the meter "4/4". On the right: "Kein Audio-Gerät" (only without device), CPU load, the master
//! meter, the "⋯" menu. The texts come from `locales/*/transport.ftl` (`docs/I18N.md`); German
//! is quoted here.

use std::sync::Arc;

use egui::{
    Align, Galley, Layout, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_audio::AudioState;
use komponist_engine::TransportState;
use komponist_i18n::Lang;
use komponist_model::format::{fmt_pos, fmt_time};
use komponist_model::{Project, Tempo, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::meter::Meter;
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, icons, px};

use crate::actions::{Action, EditAction, ProjectAction, TransportAction};
use crate::keymap::Keymap;
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
    let items = [
        (t.transport_loop(), p.loop_on, TransportAction::ToggleLoop),
        (
            t.transport_metronome(),
            p.metronome,
            TransportAction::ToggleMetronome,
        ),
        (
            t.transport_count_in(),
            p.count_in_bars > 0,
            TransportAction::ToggleCountIn,
        ),
    ];
    for (name, on, action) in items {
        let action = Action::Transport(action);
        let shortcut = model.keymap.label_for(&action, t);
        let mut button = Button::new(name).on(on);
        if let Some(sc) = shortcut.as_deref() {
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
