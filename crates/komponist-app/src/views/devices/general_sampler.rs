//! The Sample and Modulation pages of the Sampler.

use std::ops::Range;
use std::sync::Arc;

use egui::{Align, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_audiofile::PeakData;
use komponist_general_sampler::{
    MAX_FADE_MS, MAX_SLICES, Settings, Slice, SliceOptions, read_settings,
};
use komponist_i18n::Lang;
use komponist_model::format::note_name;
use komponist_model::params::ParamId;
use komponist_sampler::{PadStatus, PadView, SamplerHandle, SyncStatus};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, TrackPalette, color, device, radius, size, space, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::device::choice::DropdownCell;
use komponist_ui::widgets::device::graph::{
    AreaAction, Axis, AxisKind, Bind, Curve, CurveFn, DataKind, Graph, GraphEvent, GraphOutput,
    GraphSpec, Grid, Handle, HandleLook, Layer, Look, Marker, MarkerHead, Pred, Region, SlotInfo,
    Tick,
};
use komponist_ui::widgets::device::number::Number;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::format_number;
use komponist_ui::{fonts, state};

use super::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, Page, WidgetKind,
};
use super::model::{Card, Param};
use super::{cells, metrics};
use crate::actions::{Action, EditAction, PluginAction};
use crate::views::mixer::{Touch, gesture_actions, gesture_touch};

/// Snapshot of the Sampler's linked source and edits, copied on the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Editor {
    source: Option<PadView>,
    peaks: Option<Arc<PeakData>>,
    playhead: Option<f32>,
    transients: Vec<f64>,
    settings: Settings,
    sync_status: Option<SyncStatus>,
}

impl Editor {
    pub(super) fn new(device_state: Option<&[u8]>, handle: Option<&SamplerHandle>) -> Self {
        Self {
            source: handle.map(|handle| handle.view(0)),
            peaks: handle.and_then(SamplerHandle::waveform_peaks),
            playhead: handle.and_then(|handle| handle.playhead(0)),
            transients: handle.map(SamplerHandle::transients).unwrap_or_default(),
            settings: read_settings(device_state),
            sync_status: handle.and_then(SamplerHandle::sync_status),
        }
    }

    fn length(&self) -> Option<f64> {
        self.source.as_ref().and_then(|source| {
            (source.status == PadStatus::Ready && source.length_s > 0.0).then_some(source.length_s)
        })
    }

    fn slice_spacing(&self) -> f64 {
        1.0 / f64::from(
            self.peaks
                .as_ref()
                .map_or(48_000, |peaks| peaks.sample_rate()),
        )
    }
}

pub(super) fn layout(t: Lang) -> DeviceLayout {
    let half = |col, row, width, part, widget, id| {
        Cell::half(
            col,
            row,
            width,
            part,
            widget,
            CellParam::Id(ParamId::new(id)),
        )
    };
    let field = |col, row, width, part, id| half(col, row, width, part, WidgetKind::Number, id);
    let choice = |col, row, width, part, id| half(col, row, width, part, WidgetKind::Dropdown, id);
    let knob = |col, id| Cell::knob(col, 2, ParamId::new(id));
    let page = |name: &str, cells| Page {
        name: name.to_owned(),
        cols: metrics::SAMPLER_COLUMNS,
        cells,
        dividers: vec![metrics::SAMPLER_WAVE_COLUMNS - 1],
    };
    use Half::{Bottom, Top};
    DeviceLayout {
        pages: vec![
            page(
                t.devices_sampler_sample(),
                vec![
                    half(10, 0, 4, Top, WidgetKind::Segment, "mode"),
                    choice(10, 0, 2, Bottom, "direction"),
                    field(12, 0, 2, Bottom, "root"),
                    half(10, 1, 2, Top, WidgetKind::ToggleButton, "sync"),
                    field(12, 1, 2, Top, "source_bpm"),
                    knob(10, "coarse"),
                    knob(11, "fine"),
                    knob(12, "gain"),
                    knob(13, "pan"),
                    field(0, 2, 1, Top, "start"),
                    field(1, 2, 1, Top, "end"),
                    field(2, 2, 2, Top, "fade_in"),
                    field(4, 2, 2, Top, "fade_out"),
                ],
            ),
            page(
                t.devices_sampler_modulation(),
                vec![
                    Cell::graph(
                        0,
                        0,
                        5,
                        2,
                        GraphBinding::new(GraphConfig::Envelope)
                            .titled(t.devices_sampler_amp_envelope())
                            .bind("attack", "amp_attack")
                            .bind("decay", "amp_decay")
                            .bind("sustain", "amp_sustain")
                            .bind("release", "amp_release"),
                    ),
                    knob(0, "amp_attack"),
                    knob(1, "amp_decay"),
                    knob(2, "amp_sustain"),
                    knob(3, "amp_release"),
                    knob(5, "filter_env"),
                    knob(6, "filter_decay"),
                    knob(7, "resonance"),
                    choice(10, 0, 2, Top, "filter"),
                    field(12, 0, 2, Top, "cutoff"),
                    choice(10, 0, 2, Bottom, "lfo_shape"),
                    field(12, 0, 2, Bottom, "lfo_rate"),
                    choice(10, 1, 2, Bottom, "lfo_target"),
                    field(12, 1, 2, Bottom, "lfo_depth"),
                    choice(10, 2, 2, Bottom, "velocity_target"),
                    field(12, 2, 2, Bottom, "velocity_depth"),
                ],
            ),
        ],
    }
}

#[derive(Clone, Default)]
struct Viewport {
    start: f64,
    end: f64,
}

impl Viewport {
    fn fit(length: f64) -> Self {
        Self {
            start: 0.0,
            end: length,
        }
    }

    fn width(&self) -> f64 {
        self.end - self.start
    }

    fn centre(&self) -> f64 {
        (self.start + self.end) * 0.5
    }

    fn contains(&self, time: f64) -> bool {
        (self.start..=self.end).contains(&time)
    }

    fn set(&mut self, centre: f64, width: f64, length: f64) {
        self.set_anchored(centre, 0.5, width, length);
    }

    fn set_anchored(&mut self, anchor: f64, fraction: f64, width: f64, length: f64) {
        let width = width.clamp(metrics::SAMPLER_MIN_ZOOM.min(length), length);
        self.start = (anchor - width * fraction).clamp(0.0, length - width);
        self.end = self.start + width;
    }
}

const WAVEFORM_X_PAD: f32 = space::SPACE_1_5;

fn waveform_rect(body_origin: egui::Pos2) -> Rect {
    let pad = device::grid::PADDING;
    let left = body_origin + vec2(pad, pad);
    let width = super::layout::device_width(metrics::SAMPLER_WAVE_COLUMNS) - 2.0 * pad;
    Rect::from_min_max(
        pos2(left.x, left.y + size::CONTROL_COMPACT + space::SPACE_2),
        pos2(
            left.x + width,
            left.y + 2.0 * metrics::SAMPLER_ROW - device::grid::GAP,
        ),
    )
}

/// Give the waveform priority only when it can act on the wheel input.
pub(super) fn takes_wheel(ui: &Ui, card: &Card, body_origin: egui::Pos2, clip: Rect) -> bool {
    let Some(editor) = &card.general_sampler else {
        return false;
    };
    let Some(length) = editor.length() else {
        return false;
    };
    let rect = waveform_rect(body_origin);
    if card.collapsed
        || card.page != 0
        || card.state != super::model::CardState::Ok
        || card.warning.is_some()
        || !ui.rect_contains_pointer(rect.intersect(clip))
    {
        return false;
    }
    let view = view_state(ui, card, editor);
    wheel_viewport(ui, rect, &view.viewport, length).is_some()
}

fn wheel_viewport(ui: &Ui, graph: Rect, viewport: &Viewport, length: f64) -> Option<Viewport> {
    let (delta, zoom, pointer) =
        ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
    let plot = graph.shrink2(vec2(WAVEFORM_X_PAD, 0.0));
    let width = viewport.width();
    let mut next = viewport.clone();
    if zoom != 1.0 {
        let x = pointer.map_or(plot.center().x, |p| p.x);
        let fraction = f64::from(((x - plot.left()) / plot.width()).clamp(0.0, 1.0));
        let anchor = viewport.start + fraction * width;
        next.set_anchored(anchor, fraction, width / f64::from(zoom), length);
        return Some(next);
    }
    // Like the audio clip editor, both wheel axes pan along the waveform's time axis.
    let pixels = delta.x + delta.y;
    if (pixels > 0.0 && viewport.start > 0.0) || (pixels < 0.0 && viewport.end < length) {
        let seconds = f64::from(pixels / plot.width()) * width;
        next.set(viewport.centre() - seconds, width, length);
        return (next.start != viewport.start).then_some(next);
    }
    None
}

#[derive(Clone, Default)]
struct ViewState {
    link: Option<komponist_sampler::SampleLink>,
    length: f64,
    viewport: Viewport,
    click_preview_disabled: bool,
    selected_time: Option<f64>,
    selected_index: Option<usize>,
    pending_slices: Option<PendingSlices>,
}

#[derive(Clone)]
struct PendingSlices {
    baseline: Settings,
    preview: Settings,
}

fn view_id(card: &Card) -> egui::Id {
    egui::Id::unique(("general-sampler-view", card.device))
}

fn view_state(ui: &Ui, card: &Card, editor: &Editor) -> ViewState {
    let view: ViewState = ui
        .data(|data| data.get_temp(view_id(card)))
        .unwrap_or_default();
    let link = editor
        .source
        .as_ref()
        .and_then(|sample| sample.link.clone());
    let length = editor.length().unwrap_or(0.0);
    if view.link == link && view.length == length {
        view
    } else {
        ViewState {
            link,
            length,
            viewport: Viewport::fit(length),
            click_preview_disabled: view.click_preview_disabled,
            ..Default::default()
        }
    }
}

pub(super) fn show(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    palette: &TrackPalette,
    body: Rect,
) {
    let Some(editor) = &card.general_sampler else {
        return;
    };
    let page = card.page.min(1);
    let lang = komponist_ui::lang(ui.ctx());
    let pad = device::grid::PADDING;
    let left = body.left_top() + vec2(pad, pad);
    let wave_width = super::layout::device_width(metrics::SAMPLER_WAVE_COLUMNS) - 2.0 * pad;
    let right_x = left.x + wave_width + device::grid::GAP;
    let label = |ui: &mut Ui, y, title| {
        let rect = Rect::from_min_size(
            pos2(right_x, left.y + y),
            vec2(body.right() - right_x - pad, size::CONTROL_COMPACT),
        );
        heading(ui, rect, title);
    };
    if page == 1 {
        label(ui, metrics::SAMPLER_ROW, lang.devices_sampler_lfo());
        label(
            ui,
            2.0 * metrics::SAMPLER_ROW,
            lang.devices_sampler_velocity(),
        );
        filter_graph(ui, card, palette, left, wave_width);
        return;
    }
    if value(card, "sync", 0.0) >= 0.5 {
        let sync = match editor.sync_status {
            Some(SyncStatus::Preparing) => Some(lang.devices_sampler_sync_preparing()),
            Some(SyncStatus::Unavailable) => Some(lang.devices_sampler_sync_unavailable()),
            _ => None,
        };
        if let Some(sync) = sync {
            label(
                ui,
                metrics::SAMPLER_ROW + device::grid::HALF_OFFSET_BOTTOM,
                sync,
            );
        }
    }

    let mut view = view_state(ui, card, editor);
    let toolbar_rect = Rect::from_min_size(left, vec2(wave_width, size::CONTROL_COMPACT));
    let graph_rect = waveform_rect(body.min);
    if view.length > 0.0
        && ui.rect_contains_pointer(graph_rect)
        && let Some(viewport) = wheel_viewport(ui, graph_rect, &view.viewport, view.length)
    {
        view.viewport = viewport;
        ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
    }
    toolbar(ui, out, card, editor, &mut view, toolbar_rect);
    sample_drop(ui, out, card, toolbar_rect.union(graph_rect));
    let tools = Rect::from_min_size(
        pos2(
            left.x,
            left.y + 2.0 * metrics::SAMPLER_ROW + device::grid::HALF_OFFSET_BOTTOM,
        ),
        vec2(wave_width, size::CONTROL_COMPACT),
    );
    playback_wave(
        ui, out, card, editor, palette, graph_rect, tools, body, &mut view,
    );
    ui.ctx()
        .data_mut(|data| data.insert_temp(view_id(card), view));
}

fn heading(ui: &mut Ui, rect: Rect, text: &str) {
    let response = ui.allocate_rect(rect, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        fonts::font_id(&typography::SMALL),
        color32(color::TEXT_2),
    );
}

fn toolbar(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    editor: &Editor,
    view: &mut ViewState,
    rect: Rect,
) {
    let t = komponist_ui::lang(ui.ctx());
    let ready = editor.length();
    let source = editor.source.as_ref();
    let display = match source.map(|sample| &sample.status) {
        Some(PadStatus::Ready) => source.map_or("", |sample| &sample.name),
        Some(PadStatus::Loading) => t.devices_pad_loading(),
        Some(PadStatus::Missing) => t.devices_pad_missing(),
        Some(PadStatus::Failed(_)) => t.devices_pad_failed(),
        _ => t.devices_load_sample(),
    };
    let pick = || PluginAction::PickSample {
        device: card.device,
        pad: 0,
    };
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    ui.spacing_mut().item_spacing.x = device::grid::GAP;
    let style = typography::SMALL;
    let displayed = crate::views::elided_galley(
        &ui,
        display,
        komponist_ui::widgets::font_at(&style, komponist_ui::widgets::metrics::MEDIUM),
        style.line_height,
        metrics::SAMPLER_FILE_WIDTH - 2.0 * space::SPACE_2,
    );
    let name = displayed.rows.first().map(|row| row.text());
    let file = ui.add(
        Button::new(name.as_deref().unwrap_or(display))
            .name(display)
            .small()
            .width(metrics::SAMPLER_FILE_WIDTH),
    );
    if let Some(sample) = source {
        let detail = match &sample.status {
            PadStatus::Failed(reason) => reason,
            _ => &sample.name,
        };
        if !detail.is_empty() {
            file.clone().on_hover_text(detail);
        }
    }
    if file.clicked() {
        if source.is_some_and(|sample| matches!(sample.status, PadStatus::Ready)) {
            ContextMenu::<FileAction>::open_at(ui.ctx(), file.id, file.rect.left_bottom());
        } else {
            out.push(Action::Plugin(pick()));
        }
    }
    let load_label = if source.is_some_and(|sample| sample.status == PadStatus::Missing) {
        t.devices_search()
    } else {
        t.devices_load_sample()
    };
    let clear = Entry::action(t.devices_remove_sample(), FileAction::Clear);
    let clear = if source.is_some_and(|sample| sample.link.is_some()) {
        clear
    } else {
        clear.disabled()
    };
    let entries = [Entry::action(load_label, FileAction::Pick), clear];
    match ContextMenu::new(t.devices_sampler_sample_menu(), &entries).show_on(&file) {
        Some(FileAction::Pick) => out.push(Action::Plugin(pick())),
        Some(FileAction::Clear) => out.push(Action::Plugin(PluginAction::ClearSample {
            device: card.device,
            pad: 0,
        })),
        None => {}
    }

    if let Some(length) = ready {
        let time = format_number(length, 2);
        let time = match t {
            Lang::De => time.replace('.', ","),
            Lang::En => time,
        };
        let duration = t.devices_sampler_length(time);
        let length_text = editor.peaks.as_ref().map_or_else(
            || duration.clone(),
            |peaks| {
                let rate = crate::project_io::fmt_bpm(f64::from(peaks.sample_rate()) / 1000.0);
                let channels = if peaks.channels() == 1 {
                    t.audio_channel_mono()
                } else {
                    t.audio_channel_stereo()
                };
                t.audio_file_metadata(rate, channels, &duration)
            },
        );
        let height = size::CONTROL_COMPACT;
        let size = vec2(metrics::SAMPLER_FILE_INFO_WIDTH, height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &length_text));
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            length_text,
            fonts::font_id(&typography::SMALL),
            color32(color::TEXT_2),
        );
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let viewport = &mut view.viewport;
        let narrowed = ready.is_some_and(|length| viewport.width() < length);
        let fit = ui
            .add_enabled(
                narrowed,
                Button::icon(Icon::Maximize2, t.devices_sampler_fit()).small(),
            )
            .clicked();
        let zoom_in = ui
            .add_enabled(
                ready.is_some(),
                Button::icon(Icon::Plus, t.audio_zoom_in()).small(),
            )
            .clicked();
        let zoom_out = ui
            .add_enabled(
                narrowed,
                Button::icon(Icon::Minus, t.audio_zoom_out()).small(),
            )
            .clicked();
        let later = ui
            .add_enabled(
                narrowed,
                Button::icon(Icon::ChevronRight, t.devices_sampler_later()).small(),
            )
            .clicked();
        let earlier = ui
            .add_enabled(
                narrowed,
                Button::icon(Icon::ChevronLeft, t.devices_sampler_earlier()).small(),
            )
            .clicked();
        if ui
            .add(
                Button::icon(Icon::Volume2, t.devices_sampler_click_preview())
                    .small()
                    .on(!view.click_preview_disabled),
            )
            .clicked()
        {
            view.click_preview_disabled = !view.click_preview_disabled;
        }
        if let Some(length) = ready {
            let width = viewport.width();
            let centre = viewport.centre();
            if fit {
                *viewport = Viewport::fit(length);
            } else if zoom_in || zoom_out {
                let factor = if zoom_in { 0.5 } else { 2.0 };
                let anchor = editor
                    .playhead
                    .map(|p| f64::from(p) * length)
                    .filter(|&p| viewport.contains(p))
                    .unwrap_or(centre);
                viewport.set(anchor, width * factor, length);
            } else if earlier || later {
                let shift = if earlier { -0.5 } else { 0.5 };
                viewport.set(centre + shift * width, width, length);
            }
        }
    });
}

#[derive(Clone, Copy)]
enum FileAction {
    Pick,
    Clear,
}

fn sample_drop(ui: &mut Ui, out: &mut Vec<Action>, card: &Card, rect: Rect) {
    let (over, hovered, path) = ui.input(|input| {
        (
            input
                .pointer
                .latest_pos()
                .is_some_and(|p| rect.intersect(ui.clip_rect()).contains(p)),
            !input.raw.hovered_files.is_empty(),
            input
                .raw
                .dropped_files
                .first()
                .map(|file| file.path().to_path_buf()),
        )
    });
    if over {
        if hovered {
            state::paint_select_ring(ui.painter(), rect, radius::M);
        }
        if let Some(path) = path {
            out.push(Action::Plugin(PluginAction::LoadSample {
                device: card.device,
                pad: 0,
                path,
            }));
        }
    }
}

fn waveform_spec(t: Lang, view: &Viewport) -> GraphSpec {
    let width = view.width();
    let ticks = (1..metrics::SAMPLER_TICK_COUNT)
        .map(|i| {
            let time = view.start + width * f64::from(i) / f64::from(metrics::SAMPLER_TICK_COUNT);
            Tick::major(time, format_number(time, if width >= 1.0 { 1 } else { 3 }))
        })
        .collect();
    let mut spec = GraphSpec::new(
        "",
        Axis::new(AxisKind::Time, view.start, view.end)
            .padding([WAVEFORM_X_PAD; 2])
            .grid(Grid::Ticks(ticks))
            .labelled(),
        Axis::new(AxisKind::Lin, -1.0, 1.0)
            .padding([space::SPACE_3, space::SPACE_2])
            .grid(Grid::Ticks(vec![Tick::minor(0.0)])),
    );
    spec.name = t.ui_graph_waveform().to_owned();
    spec.layers = vec![
        Layer::Data(DataKind::Sample),
        Layer::Marker(Marker::FullHeightPlayhead),
    ];
    spec
}

fn waveform_data(editor: &Editor, view: &Viewport, columns: usize) -> Vec<f32> {
    if let Some(peaks) = &editor.peaks {
        let rate = f64::from(peaks.sample_rate());
        let start = (view.start * rate).round() as u64;
        let end = (view.end * rate).round() as u64;
        let mut amplitudes = vec![0.0_f32; columns];
        for channel in 0..peaks.channels().min(2) {
            let query = peaks.query(channel, start, end.max(start + 1), columns);
            for (amplitude, peak) in amplitudes.iter_mut().zip(query.columns) {
                *amplitude = amplitude.max(peak.min.abs().max(peak.max.abs()));
            }
        }
        return amplitudes;
    }
    let Some(sample) = &editor.source else {
        return Vec::new();
    };
    if sample.peaks.is_empty() || sample.length_s <= 0.0 {
        return Vec::new();
    }
    let count = sample.peaks.len() as f64;
    let start = (view.start / sample.length_s * count).floor() as usize;
    let end = (view.end / sample.length_s * count).ceil() as usize;
    sample
        .peaks
        .get(start..end.min(sample.peaks.len()))
        .unwrap_or_default()
        .to_vec()
}

#[allow(clippy::too_many_arguments)]
fn graph(
    ui: &mut Ui,
    rect: Rect,
    spec: &GraphSpec,
    values: &mut [f64],
    infos: &[SlotInfo<'_>],
    palette: &TrackPalette,
    look: Look,
    data: &[f32],
    playhead: Option<f64>,
    key: &'static str,
) -> GraphOutput {
    let id = ui.scope_id().with(key);
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    Graph::new(spec, values, infos, rect.size())
        .track(color32(palette.base))
        .look(look)
        .data(data)
        .playhead(playhead)
        .id(id)
        .show(&mut child)
}

fn param<'a>(card: &'a Card, key: &str) -> Option<&'a Param> {
    card.param(&ParamId::new(key))
}

fn preview_on_press(
    ui: &Ui,
    out: &mut Vec<Action>,
    card: &Card,
    graph: &GraphOutput,
    slice_interval: Option<&Range<f64>>,
    enabled: bool,
) -> Option<f64> {
    let primary = egui::PointerButton::Primary;
    let response = &graph.response;
    if !ui.input(|input| input.pointer.button_pressed(primary))
        || !(response.is_pointer_button_down_on() || response.clicked_by(primary))
    {
        return None;
    }
    let slice_time = if let Some(interval) = slice_interval {
        Some(graph.pointer_x.filter(|time| interval.contains(time))?)
    } else {
        None
    };
    if enabled {
        out.push(Action::Plugin(PluginAction::PreviewSampler {
            device: card.device,
            slice_time,
        }));
    }
    slice_time
}

fn value(card: &Card, key: &str, fallback: f64) -> f64 {
    param(card, key).map_or(fallback, |parameter| parameter.value)
}

/// Playback positions are normalized on disk and in automation, and displayed as source time.
pub(super) fn sample_cell(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    rect: Rect,
) -> bool {
    let CellParam::Id(id) = &cell.param else {
        return false;
    };
    if matches!(id.as_str(), "fade_in" | "fade_out" | "direction")
        && value(card, "mode", 1.0) == 2.0
    {
        return true;
    }
    if cell.widget != WidgetKind::Number || !matches!(id.as_str(), "start" | "end") {
        return false;
    }
    let Some(length) = card.general_sampler.as_ref().and_then(Editor::length) else {
        return false;
    };
    let Some(p) = card.param(id) else {
        return false;
    };
    let ms = length * 1000.0;
    let mut time = p.value * ms;
    let entry = format_number(time, 0);
    let field = Number::new(&mut time, p.name())
        .range(0.0..=ms)
        .default_value(p.info.default * ms)
        .unit("ms")
        .cols(cell.cols)
        .format(|time| format_number(time, 0));
    let response = ui.put(rect, field);
    cells::touch(
        ui,
        out,
        card,
        p,
        &response,
        Some(entry),
        false,
        response.changed().then_some(time / ms),
    );
    true
}

fn edit_look(card: &Card, ready: bool) -> Look {
    if !ready || card.macro_mapping {
        Look::Disabled
    } else {
        Look::Normal
    }
}

fn visible_at(slot: usize, view: &Viewport, scale: f64) -> Pred {
    let range = view.start..=view.end;
    Pred::new(move |slots| range.contains(&(slots[slot] * scale)))
}

fn marker(
    label: &str,
    slot: usize,
    scale: f64,
    view: &Viewport,
    head: MarkerHead,
    t: Lang,
) -> Handle {
    Handle::dot(label)
        .look(HandleLook::Marker(head))
        .x(Bind::axis(slot, t.ui_graph_position()).scaled(scale))
        .visible_if(visible_at(slot, view, scale))
}

// The first four slots store trim and fade settings. Slice positions follow in seconds.
const FIRST_SLICE_SLOT: usize = 4;
const FIRST_SLICE_HANDLE: usize = 2;
const SAMPLE_WAVE: &str = "sample-wave";

#[allow(clippy::too_many_arguments)]
fn playback_wave(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    editor: &Editor,
    palette: &TrackPalette,
    rect: Rect,
    tools: Rect,
    body: Rect,
    view: &mut ViewState,
) {
    let t = komponist_ui::lang(ui.ctx());
    let length = editor.length().unwrap_or(1.0);
    let layout_view = if view.viewport.width() > 0.0 {
        view.viewport.clone()
    } else {
        Viewport::fit(length)
    };
    let scale = length;
    let mut spec = waveform_spec(t, &layout_view);
    let keys = ["start", "end"];
    let params = keys.map(|key| param(card, key));
    spec.slots = keys.map(str::to_owned).to_vec();
    let mut values = vec![0.0, 1.0];
    let mut infos: Vec<_> = params
        .iter()
        .zip(&mut values)
        .map(|(parameter, value)| {
            if let Some(p) = parameter {
                *value = p.value;
                let info = SlotInfo::from_param(&p.info, false)
                    .format(move |fraction| format_number(fraction * scale * 1000.0, 0) + " ms");
                let driven = card
                    .macros
                    .get(&p.info.id)
                    .is_some_and(|control| control.driver.is_some());
                if driven { info.locked() } else { info }
            } else {
                SlotInfo::new(0.0, 1.0, *value).locked()
            }
        })
        .collect();
    for key in ["fade_in", "fade_out"] {
        values.push(value(card, key, 0.0) / 1000.0);
        infos.push(SlotInfo::new(0.0, length, 0.0).locked());
        spec.slots.push(key.to_owned());
    }
    spec.handles = vec![
        marker(
            t.ui_graph_sample_start(),
            0,
            scale,
            &layout_view,
            MarkerHead::Right,
            t,
        )
        .between(None, Some(1)),
        marker(
            t.ui_graph_sample_end(),
            1,
            scale,
            &layout_view,
            MarkerHead::Left,
            t,
        )
        .between(Some(0), None),
    ];
    spec.layers.push(Layer::Region(Region::Outside {
        start: 0,
        end: 1,
        scale,
    }));
    let slicing = value(card, "mode", 1.0) == 2.0;
    let pending = view.pending_slices.take();
    let cancelled = pending
        .as_ref()
        .is_some_and(|edit| !slicing || edit.baseline != editor.settings);
    if cancelled {
        ui.ctx().stop_dragging();
    }
    let mut settings = if slicing {
        pending
            .filter(|_| !cancelled)
            .map_or_else(|| editor.settings.clone(), |edit| edit.preview)
    } else {
        Settings::default()
    };
    let interval = (values[0] * scale)..(values[1].max(values[0]) * scale);
    let mut selected = view.selected_time.and_then(|time| {
        settings
            .slices
            .iter()
            .position(|slice| (slice.time - time).abs() <= 4.0 * f64::EPSILON * time.abs().max(1.0))
    });
    selected = active_boundary(&settings.slices, selected, &interval);
    if slicing && view.selected_index != selected {
        Graph::select_handle(
            ui.ctx(),
            ui.scope_id().with(SAMPLE_WAVE),
            selected.map(|i| FIRST_SLICE_HANDLE + i),
        );
    } else if !slicing && view.selected_time.take().is_some() {
        view.selected_index = None;
        Graph::select_handle(ui.ctx(), ui.scope_id().with(SAMPLE_WAVE), None);
    }
    let root = value(card, "root", 60.0) as usize;
    let mut bounds = [0, 1];
    let mut scales = [scale; 2];
    let mut reverse = value(card, "direction", 0.0) == 1.0;
    if slicing {
        if let Some(index) = selected {
            bounds[0] = FIRST_SLICE_SLOT + index;
            scales[0] = 1.0;
        }
        let start = selected.map_or(interval.start, |i| settings.slices[i].time);
        if let Some(index) = settings
            .slices
            .iter()
            .position(|slice| slice.time > start && slice.time < interval.end)
        {
            bounds[1] = FIRST_SLICE_SLOT + index;
            scales[1] = 1.0;
        }
        if editor.length().is_some() {
            spec.layers
                .push(Layer::Region(Region::Inside { bounds, scales }));
        }
        let options = settings.options(selection_settings(&settings, selected, &interval));
        let (slice_reverse, fades) = effective_options(card, options);
        reverse = slice_reverse;
        values[2] = fades[0] / 1000.0;
        values[3] = fades[1] / 1000.0;
        spec.on_double_click = AreaAction::NewPoint;
        spec.deletable = true;
        let mut active_count = 0;
        let separation = editor.slice_spacing();
        for (i, slice) in settings.slices.iter().enumerate() {
            let seconds = slice.time;
            let label = slice_note(t, i, seconds, &interval, root, &mut active_count);
            let low = i
                .checked_sub(1)
                .map_or(separation, |prev| settings.slices[prev].time + separation)
                .min(seconds);
            let high = settings
                .slices
                .get(i + 1)
                .map_or(length, |next| next.time - separation)
                .max(seconds);
            let slot = FIRST_SLICE_SLOT + i;
            values.push(seconds);
            infos.push(
                SlotInfo::new(low, high, seconds)
                    .format(|seconds| format_number(seconds * 1000.0, 0) + " ms"),
            );
            spec.slots.push(i.to_string());
            spec.handles
                .push(marker(&label, slot, 1.0, &layout_view, MarkerHead::Down, t));
        }
    }
    spec.layers.push(Layer::Region(Region::Fades {
        bounds,
        scales,
        durations: if reverse { [3, 2] } else { [2, 3] },
    }));
    let data = waveform_data(editor, &layout_view, rect.width().ceil() as usize);
    let playhead = editor
        .playhead
        .map(|p| f64::from(p) * length)
        .filter(|&p| layout_view.contains(p));
    let shown = graph(
        ui,
        rect,
        &spec,
        &mut values,
        &infos,
        palette,
        edit_look(card, editor.length().is_some()),
        &data,
        playhead,
        SAMPLE_WAVE,
    );
    let previous_selection = selected;
    let preview_time = preview_on_press(
        ui,
        out,
        card,
        &shown,
        slicing.then_some(&interval),
        !view.click_preview_disabled,
    );
    if let Some(time) = preview_time {
        selected = settings
            .slices
            .iter()
            .rposition(|slice| slice.time > interval.start && slice.time <= time);
    }
    trim_actions(ui, out, card, &params, &shown);
    if slicing {
        edit_slices(
            ui,
            out,
            card,
            editor,
            view,
            &mut selected,
            cancelled,
            tools,
            &shown,
            &values,
            &interval,
            root,
            &layout_view,
            playhead,
            &mut settings,
        );
        if previous_selection != selected || preview_time.is_some() {
            Graph::select_handle(
                ui.ctx(),
                ui.scope_id().with(SAMPLE_WAVE),
                selected.map(|i| FIRST_SLICE_HANDLE + i),
            );
            ui.ctx().request_repaint();
        }
        view.selected_index = selected;
        view.selected_time = selected.map(|index| settings.slices[index].time);
        let selected = selection_settings(&settings, selected, &interval);
        slice_controls(
            ui,
            out,
            card,
            body,
            editor.length().is_some(),
            selected,
            settings,
        );
    }
}

fn trim_actions(
    ui: &Ui,
    out: &mut Vec<Action>,
    card: &Card,
    params: &[Option<&Param>; 2],
    shown: &GraphOutput,
) {
    let slot = shown
        .selected_slot
        .or_else(|| shown.changed.first().map(|change| change.0));
    let parameter = slot.and_then(|i| params.get(i).copied().flatten());
    let Some(p) = parameter else {
        return;
    };
    let changed = shown.changed.iter().any(|(slot, _)| *slot < params.len());
    gesture_touch(
        ui.ctx(),
        shown.response.id,
        Touch {
            drag_started: shown.touch.drag_started,
            dragged: shown.touch.dragging,
            drag_stopped: shown.touch.drag_stopped,
            wheel: shown.touch.wheel,
            hovered: shown.response.hovered(),
        },
        changed,
        || cells::begin_param(card, p.wire),
        out,
        |out| {
            for &(slot, change) in &shown.changed {
                if let Some(parameter) = params.get(slot).copied().flatten() {
                    cells::set(out, card, parameter, change);
                }
            }
        },
    );
    if shown.touch.clicked || shown.touch.dragging || shown.touch.drag_stopped || changed {
        out.push(cells::touch_action(card, p, shown.touch.dragging));
    }
}

fn slice_note(
    t: Lang,
    index: usize,
    position: f64,
    interval: &Range<f64>,
    root: usize,
    active_count: &mut usize,
) -> String {
    if position > interval.start && position < interval.end {
        *active_count += 1;
        if root + *active_count < 128 {
            return note_name((root + *active_count) as u8);
        }
    }
    t.devices_sampler_slice(index + 1)
}

fn active_boundary(slices: &[Slice], index: Option<usize>, interval: &Range<f64>) -> Option<usize> {
    index.filter(|&index| {
        slices
            .get(index)
            .is_some_and(|slice| interval.start < slice.time && slice.time < interval.end)
    })
}

// If trim starts inside a slice, the audible remainder keeps that slice's options.
fn selection_settings(
    settings: &Settings,
    index: Option<usize>,
    interval: &Range<f64>,
) -> Option<usize> {
    active_boundary(&settings.slices, index, interval).or_else(|| {
        settings
            .slices
            .iter()
            .rposition(|slice| slice.time <= interval.start)
    })
}

fn effective_options(card: &Card, options: SliceOptions) -> (bool, [f64; 2]) {
    (
        options
            .reverse
            .unwrap_or_else(|| value(card, "direction", 0.0) == 1.0),
        [
            options
                .fade_in_ms
                .map_or_else(|| value(card, "fade_in", 0.0), f64::from),
            options
                .fade_out_ms
                .map_or_else(|| value(card, "fade_out", 0.0), f64::from),
        ],
    )
}

fn slice_controls(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    body: Rect,
    ready: bool,
    selected: Option<usize>,
    mut settings: Settings,
) {
    let t = komponist_ui::lang(ui.ctx());
    let options = settings.options(selected);
    let (reverse, fades) = effective_options(card, options);
    let enabled = ready && !card.macro_mapping;
    let cell = |col, key, kind| {
        super::layout::cell_rect(&Cell::half(
            col,
            2,
            2,
            Half::Top,
            kind,
            CellParam::Id(ParamId::new(key)),
        ))
        .translate(body.min.to_vec2())
    };
    let undo = || {
        Action::Edit(EditAction::BeginGesture {
            label: "undo-sample-slices".to_owned(),
        })
    };
    let mut row = ui.new_child(
        UiBuilder::new()
            .id_salt(("slice-settings", selected))
            .max_rect(body),
    );
    if !enabled {
        row.disable();
    }
    let mut gate = usize::from(options.gate);
    let triggers = [
        Item::new(t.choice_sampler_mode_one_shot()),
        Item::new(t.choice_sampler_mode_gate()),
    ];
    if row
        .put(
            cell(2, "mode", WidgetKind::Dropdown),
            DropdownCell::new(t.devices_sampler_slice_trigger(), &triggers, &mut gate),
        )
        .changed()
    {
        settings.options_mut(selected).gate = gate != 0;
    }
    let mut direction = usize::from(reverse);
    let directions = [
        Item::new(t.param_general_sampler_forward()),
        Item::new(t.param_general_sampler_reverse()),
    ];
    if row
        .put(
            cell(4, "direction", WidgetKind::Dropdown),
            DropdownCell::new(
                t.param_general_sampler_direction(),
                &directions,
                &mut direction,
            ),
        )
        .changed()
    {
        settings.options_mut(selected).reverse = Some(direction != 0);
    }
    if settings.options(selected) != options {
        out.push(Action::Plugin(PluginAction::SetSamplerSlices {
            device: card.device,
            settings: settings.clone(),
        }));
    }
    for (col, key, label, mut ms) in [
        (6, "fade_in", t.param_general_sampler_fade_in(), fades[0]),
        (8, "fade_out", t.param_general_sampler_fade_out(), fades[1]),
    ] {
        let response = row.put(
            cell(col, key, WidgetKind::Number),
            Number::new(&mut ms, label)
                .range(0.0..=f64::from(MAX_FADE_MS))
                .step(1.0)
                .unit("ms")
                .cols(2),
        );
        if response.changed() {
            let updated = settings.options_mut(selected);
            if key == "fade_in" {
                updated.fade_in_ms = Some(ms as f32);
            } else {
                updated.fade_out_ms = Some(ms as f32);
            }
        }
        gesture_actions(&response, response.changed(), undo, out, |out| {
            out.push(Action::Plugin(PluginAction::SetSamplerSlices {
                device: card.device,
                settings: settings.clone(),
            }));
        });
    }
}

fn valid_slice_position(
    slices: &[Slice],
    position: f64,
    interval: &Range<f64>,
    spacing: f64,
) -> bool {
    position > interval.start
        && position < interval.end
        && !slices
            .iter()
            .any(|slice| (slice.time - position).abs() < spacing)
}

fn slice_insertion_position(
    slices: &[Slice],
    cursor: f64,
    interval: &Range<f64>,
    spacing: f64,
) -> Option<f64> {
    let valid = |position| valid_slice_position(slices, position, interval, spacing);
    if valid(cursor) {
        return Some(cursor);
    }

    let edges = std::iter::once(f64::NEG_INFINITY)
        .chain(slices.iter().map(|slice| slice.time))
        .chain(std::iter::once(f64::INFINITY));
    edges
        .clone()
        .zip(edges.skip(1))
        .filter_map(|(left, right)| {
            let start = (left + spacing).max(interval.start);
            let end = (right - spacing).min(interval.end);
            if start > end {
                return None;
            }
            let preferred = cursor.clamp(start, end);
            // The midpoint avoids floating-point roundoff at the edge of the exclusion zone.
            [
                preferred,
                preferred.next_up(),
                preferred.next_down(),
                start.midpoint(end),
            ]
            .into_iter()
            .find(|&position| valid(position))
        })
        .min_by(|a, b| (a - cursor).abs().total_cmp(&(b - cursor).abs()))
}

#[allow(clippy::too_many_arguments)]
fn edit_slices(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    editor: &Editor,
    view: &mut ViewState,
    selection: &mut Option<usize>,
    cancelled: bool,
    tools: Rect,
    shown: &GraphOutput,
    values: &[f64],
    interval: &Range<f64>,
    root: usize,
    viewport: &Viewport,
    playhead: Option<f64>,
    settings: &mut Settings,
) {
    let t = komponist_ui::lang(ui.ctx());
    let ready = editor.length();
    let length = ready.unwrap_or(1.0);
    let slice_index = shown
        .selected_slot
        .filter(|_| !cancelled)
        .and_then(|slot| slot.checked_sub(FIRST_SLICE_SLOT));
    if (shown.touch.clicked || shown.touch.dragging) && slice_index.is_some() {
        *selection = slice_index;
    }
    let edited = !cancelled
        && shown
            .changed
            .iter()
            .any(|(slot, _)| *slot >= FIRST_SLICE_SLOT);
    if edited {
        for (slice, &time) in settings.slices.iter_mut().zip(&values[FIRST_SLICE_SLOT..]) {
            slice.time = time;
        }
        *selection = slice_index;
    }
    let separation = editor.slice_spacing();
    let mut changed = edited || (shown.touch.drag_stopped && slice_index.is_some());
    let insert = |settings: &mut Settings, position: f64| {
        if settings.slices.len() < MAX_SLICES
            && valid_slice_position(&settings.slices, position, interval, separation)
        {
            let index = settings
                .slices
                .partition_point(|slice| slice.time < position);
            let previous = selection_settings(settings, index.checked_sub(1), interval);
            let options = settings.options(previous);
            settings.slices.insert(
                index,
                Slice {
                    time: position,
                    options,
                },
            );
            Some(index)
        } else {
            None
        }
    };
    for &event in shown.events.iter().filter(|_| !cancelled) {
        match event {
            GraphEvent::AddPoint { x, .. } => {
                if let Some(i) = insert(settings, x) {
                    *selection = Some(i);
                    changed = true;
                }
            }
            GraphEvent::Delete(handle) => {
                if let Some(i) = handle
                    .checked_sub(FIRST_SLICE_HANDLE)
                    .filter(|&i| i < settings.slices.len())
                {
                    settings.slices.remove(i);
                    *selection = active_boundary(&settings.slices, i.checked_sub(1), interval);
                    changed = true;
                }
            }
            _ => {}
        }
    }

    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(tools)
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = device::grid::GAP;
    let can_edit = ready.is_some() && !card.macro_mapping;
    if row
        .add_enabled(can_edit, Button::new(t.devices_sampler_detect()).small())
        .clicked()
    {
        let detected = editor
            .transients
            .iter()
            .copied()
            .filter(|&time| time > interval.start && time < interval.end)
            .take(MAX_SLICES);
        settings.slices = detected
            .map(|time| {
                let options = settings
                    .slices
                    .iter()
                    .find(|slice| slice.time == time)
                    .map_or_else(SliceOptions::default, |slice| slice.options);
                Slice { time, options }
            })
            .collect();
        *selection = None;
        changed = true;
    }
    if row
        .add_enabled(
            can_edit && settings.slices.len() < MAX_SLICES,
            Button::new(t.devices_sampler_add_slice()).small(),
        )
        .clicked()
    {
        let cursor = playhead
            .filter(|&time| time > interval.start && time < interval.end)
            .unwrap_or_else(|| {
                let start = viewport.start.max(interval.start);
                let end = viewport.end.min(interval.end);
                if start < end {
                    (start + end) * 0.5
                } else {
                    (interval.start + interval.end) * 0.5
                }
            });
        if let Some(position) =
            slice_insertion_position(&settings.slices, cursor, interval, separation)
            && let Some(i) = insert(settings, position)
        {
            *selection = Some(i);
            if !viewport.contains(position) {
                view.viewport.set(position, view.viewport.width(), length);
            }
            changed = true;
        }
    }
    if row
        .add_enabled(
            can_edit && selection.is_some(),
            Button::new(t.devices_sampler_remove_slice()).small(),
        )
        .clicked()
        && let Some(i) = selection.filter(|&i| i < settings.slices.len())
    {
        settings.slices.remove(i);
        *selection = active_boundary(&settings.slices, i.checked_sub(1), interval);
        changed = true;
    }
    let mut notes = vec![(None, t.devices_sampler_slice_start(note_name(root as u8)))];
    let mut active_count = 0;
    notes.extend(settings.slices.iter().enumerate().filter_map(|(i, slice)| {
        active_boundary(&settings.slices, Some(i), interval)?;
        let note = slice_note(t, i, slice.time, interval, root, &mut active_count);
        Some((
            Some(i),
            t.devices_sampler_slice_row(note, format_number(slice.time * 1000.0, 0)),
        ))
    }));
    let items: Vec<_> = notes.iter().map(|(_, label)| Item::new(label)).collect();
    let mut selected = notes
        .iter()
        .position(|(index, _)| *index == *selection)
        .unwrap_or(0);
    if row
        .add_enabled(
            can_edit,
            Dropdown::new(t.devices_sampler_midi_assignment(), &mut selected, &items)
                .device()
                .width(metrics::SAMPLER_FILE_WIDTH),
        )
        .changed()
    {
        *selection = notes[selected].0;
        let time = selection.map_or(interval.start, |i| settings.slices[i].time);
        if !viewport.contains(time) {
            view.viewport.set(time, view.viewport.width(), length);
        }
    }
    if shown.touch.dragging && slice_index.is_some() {
        view.pending_slices = Some(PendingSlices {
            baseline: editor.settings.clone(),
            preview: settings.clone(),
        });
    } else if changed && *settings != editor.settings {
        *selection = active_boundary(&settings.slices, *selection, interval);
        out.push(Action::Plugin(PluginAction::SetSamplerSlices {
            device: card.device,
            settings: settings.clone(),
        }));
    }
}

fn filter_graph(
    ui: &mut Ui,
    card: &Card,
    palette: &TrackPalette,
    origin: egui::Pos2,
    wave_width: f32,
) {
    let t = komponist_ui::lang(ui.ctx());
    let left = origin + vec2(wave_width * 0.5 + device::grid::GAP * 0.5, 0.0);
    let rect = Rect::from_min_size(
        left,
        vec2(
            wave_width - (left.x - origin.x),
            2.0 * metrics::SAMPLER_ROW - device::grid::GAP,
        ),
    );
    let env = value(card, "filter_env", 0.0);
    let duration = value(card, "filter_decay", 200.0) / 1000.0;
    let y_limit = (env.abs() * 1.25).max(0.5);
    let mut values = [env, duration];
    let infos = [
        SlotInfo::new(-8.0, 8.0, 0.0).locked(),
        SlotInfo::new(0.0, 10.0, 0.2).locked(),
    ];
    let mut spec = GraphSpec::new(
        t.devices_sampler_filter_envelope(),
        Axis::new(AxisKind::Time, 0.0, 1.0_f64.max(duration * 1.5)).grid(Grid::Divisions(5)),
        Axis::new(AxisKind::Lin, -y_limit, y_limit).grid(Grid::Divisions(4)),
    );
    spec.slots = vec!["depth".into(), "decay".into()];
    spec.layers = vec![Layer::Curve(Curve {
        f: CurveFn::new(|time, positions| positions[0] * (-time / positions[1].max(0.001)).exp()),
        baseline: Some(0.0),
        fill: true,
    })];
    graph(
        ui,
        rect,
        &spec,
        &mut values,
        &infos,
        palette,
        Look::Normal,
        &[],
        None,
        "filter-envelope",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_slice_finds_a_nearby_gap_when_the_preferred_position_is_blocked() {
        let settings = Settings::from_markers([2.0, 2.00003]);
        let interval = 0.0..4.0;
        let spacing = 1.0 / 48_000.0;
        let position = slice_insertion_position(&settings.slices, 2.0, &interval, spacing).unwrap();
        assert!(valid_slice_position(
            &settings.slices,
            position,
            &interval,
            spacing
        ));
        assert!((position - 2.0).abs() < 2.0 * spacing);
        assert_eq!(
            slice_insertion_position(&settings.slices, 1.0, &interval, spacing),
            Some(1.0)
        );
    }

    #[test]
    fn waveform_zoom_keeps_a_sample_in_view_and_pans_to_both_ends() {
        let mut view = Viewport::fit(10.0);
        view.set(5.0, 2.0, 10.0);
        assert_eq!((view.start, view.end), (4.0, 6.0));
        view.set(0.0, view.width(), 10.0);
        assert_eq!((view.start, view.end), (0.0, 2.0));
        view.set(10.0, view.width(), 10.0);
        assert_eq!((view.start, view.end), (8.0, 10.0));
        view.set(10.0, 0.0, 10.0);
        assert!(view.width() > 0.0 && view.end == 10.0);
    }

    #[test]
    fn zoom_reads_waveform_peaks_from_the_visible_source_frames() {
        let mut samples = vec![0.0; 48_000];
        samples[2_400..2_600].fill(0.8);
        let editor = Editor {
            source: None,
            peaks: Some(Arc::new(PeakData::from_planar(48_000, &[samples]))),
            playhead: None,
            transients: Vec::new(),
            settings: Settings::default(),
            sync_status: None,
        };
        let before = Viewport {
            start: 0.0,
            end: 0.1,
        };
        let after = Viewport {
            start: 0.9,
            end: 1.0,
        };
        assert!(
            waveform_data(&editor, &before, 40)
                .iter()
                .any(|&peak| peak > 0.7)
        );
        assert!(
            waveform_data(&editor, &after, 40)
                .iter()
                .all(|&peak| peak == 0.0)
        );
    }
}
