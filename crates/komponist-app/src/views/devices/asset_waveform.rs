//! File-pool access for visible waveform graphs declared by CLAP devices.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use egui::{
    Align, Id, Layout, Pos2, Rect, Role, Sense, Ui, UiBuilder, Vec2, WidgetInfo, pos2, vec2,
};
use komponist_audiofile::{FileId, FilePool, PeakData, PeakState, PoolFile};
use komponist_i18n::Lang;
use komponist_ipc::device_assets::{AssetKind, AssetStatus};
use komponist_ipc::native_layout::{Graph, TrimUnit};
use komponist_ipc::waveform_markers::{Edit, Preparation, Request, Snapshot};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{Device, DeviceId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, device, size, space, typography};
use komponist_ui::fonts;
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::device::graph::{
    GraphSpec, Grid, HandleLook, Layer, Marker, MarkerHead, Tick,
};
use komponist_ui::widgets::number_field::format_number;

use super::layout::{Cell, CellParam, Half, WidgetKind, cell_rect};
use super::metrics::{
    SAMPLER_FILE_INFO_WIDTH, SAMPLER_FILE_WIDTH, SAMPLER_TICK_COUNT, WAVEFORM_MIN_ZOOM,
};
use super::model::{Card, CardState};
use super::viewport::Viewport;
use crate::actions::{Action, PluginAction};
use crate::views::waveform_lookup::{LookupWorker, PendingLookup};

#[derive(Clone, Copy)]
pub(super) struct MarkerDrag {
    pub revision: u64,
    pub index: u8,
    pub seconds: f64,
}

/// Keeps a local drag preview and emits a single operation on release. Keyboard and
/// accessibility edits arrive as complete gestures.
pub(super) fn marker_request(
    ui: &Ui,
    key: Id,
    snapshot: &Snapshot,
    first_slot: usize,
    first_handle: usize,
    shown: &komponist_ui::widgets::device::graph::GraphOutput,
) -> Option<Request> {
    use komponist_ui::widgets::device::graph::GraphEvent;

    if ui.input(|input| !input.focused || input.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|data| data.remove::<MarkerDrag>(key));
        return None;
    }
    let index = |absolute: usize, start| {
        let index = absolute.checked_sub(start)?;
        (index < snapshot.positions.len()).then_some(index as u8)
    };
    let changed = shown.changed.iter().find_map(|&(slot, seconds)| {
        Some(MarkerDrag {
            revision: snapshot.revision,
            index: index(slot, first_slot)?,
            seconds,
        })
    });
    let moved = ui.data_mut(|data| {
        let previous = data.get_temp::<MarkerDrag>(key);
        data.remove::<MarkerDrag>(key);
        if shown.touch.drag_stopped {
            changed.or(previous.filter(|drag| drag.revision == snapshot.revision))
        } else if shown.touch.dragging || shown.touch.drag_started {
            if let Some(drag) =
                changed.or(previous.filter(|drag| drag.revision == snapshot.revision))
            {
                data.insert_temp(key, drag);
            }
            None
        } else {
            changed
        }
    });
    let operation = shown
        .events
        .iter()
        .find_map(|event| match *event {
            GraphEvent::AddPoint { x, .. } => Some(Edit::Insert { seconds: x }),
            GraphEvent::Delete(handle) => Some(Edit::Remove {
                index: index(handle, first_handle)?,
            }),
            GraphEvent::ResetZoom => None,
        })
        .or_else(|| {
            let drag = moved?;
            (snapshot.positions[usize::from(drag.index)] != drag.seconds).then_some(Edit::Move {
                index: drag.index,
                seconds: drag.seconds,
            })
        })?;
    Some(Request {
        revision: snapshot.revision,
        operation,
    })
}

const RECHECK: Duration = Duration::from_secs(1);
const RETRY_QUEUE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Waveform {
    pub duration: f64,
    pub file: FileId,
    pub peaks: Option<Arc<PeakData>>,
}

impl Waveform {
    pub fn amplitudes(&self, view: &Viewport, columns: usize) -> Vec<f32> {
        let Some(peaks) = &self.peaks else {
            return Vec::new();
        };
        let mut amplitudes = vec![0.0_f32; columns];
        for channel in 0..peaks.channels() {
            let query = peaks.query_secs(channel, view.start, view.end, columns);
            for (amplitude, peak) in amplitudes.iter_mut().zip(query.columns) {
                *amplitude = amplitude.max(peak.abs_max());
            }
        }
        amplitudes
    }
}

#[derive(Debug)]
struct Source {
    binding: (AssetLink, Option<PathBuf>),
    checked: Option<Instant>,
    file: Option<Arc<PoolFile>>,
    pending: Option<PendingLookup>,
    frame: u64,
}

#[derive(Debug, Default)]
pub(super) struct Cache {
    sources: BTreeMap<(DeviceId, u32), Source>,
    worker: Option<LookupWorker<AssetLink>>,
    workers: Arc<AtomicUsize>,
    worker_retry: Option<Instant>,
}

impl Cache {
    pub fn for_card(
        &mut self,
        ui: &egui::Context,
        card: &Card,
        device: &Device,
        project_dir: Option<&Path>,
    ) -> BTreeMap<u32, Waveform> {
        card.native_page()
            .into_iter()
            .flat_map(|page| &page.cells)
            .filter_map(|cell| cell.graph.as_ref()?.asset_slot)
            .filter_map(|id| {
                if !sample_slot(card, id) {
                    return None;
                }
                let link = device.asset_links.get(&id)?;
                Some((id, self.read(ui, (card.device, id), link, project_dir)?))
            })
            .collect()
    }

    fn read(
        &mut self,
        ui: &egui::Context,
        key: (DeviceId, u32),
        link: &AssetLink,
        project_dir: Option<&Path>,
    ) -> Option<Waveform> {
        let frame = ui.cumulative_frame_nr();
        let open = || Source {
            binding: (link.clone(), project_dir.map(Path::to_owned)),
            checked: None,
            file: None,
            pending: None,
            frame,
        };
        let source = self.sources.entry(key).or_insert_with(open);
        if &source.binding.0 != link || source.binding.1.as_deref() != project_dir {
            *source = open();
        }
        source.frame = frame;
        if let Some(pending) = &source.pending {
            match pending.result.try_recv() {
                Ok(file) => {
                    if let Some(current) = &file
                        && (source.file.is_none() && source.checked.is_some()
                            || matches!(
                                current.peaks_state(),
                                PeakState::Failed(komponist_audiofile::AudioError::Io(_))
                            ))
                    {
                        current.allow_peak_retry();
                    }
                    source.file = file;
                    source.checked = Some(Instant::now());
                    source.pending = None;
                }
                Err(TryRecvError::Disconnected) => {
                    source.checked = Some(Instant::now());
                    source.pending = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if source.pending.is_none() {
            let remaining = source
                .checked
                .map_or(Duration::ZERO, |at| RECHECK.saturating_sub(at.elapsed()));
            if remaining != Duration::ZERO {
                ui.request_repaint_after(remaining);
            } else {
                if self.worker.is_none()
                    && self.worker_retry.is_none_or(|at| at.elapsed() >= RECHECK)
                {
                    self.worker = LookupWorker::spawn(&self.workers, |link: &AssetLink, dir| {
                        link.resolve(dir)
                            .and_then(|path| FilePool::global().open(&path).ok())
                    });
                    self.worker_retry = self.worker.is_none().then(Instant::now);
                }
                if let Some(worker) = &self.worker {
                    source.pending =
                        worker.request(link, project_dir, ui, source.checked.is_none());
                    if source.pending.is_none() {
                        ui.request_repaint_after(RETRY_QUEUE);
                    }
                } else {
                    ui.request_repaint_after(RECHECK);
                }
            }
        }
        let file = source.file.as_ref()?;
        let peaks = match file.peaks_state() {
            PeakState::Ready(peaks) => Some(peaks),
            PeakState::Idle => {
                let ctx = ui.clone();
                match file.request_peaks(move || ctx.request_repaint()) {
                    PeakState::Ready(peaks) => Some(peaks),
                    _ => None,
                }
            }
            PeakState::Pending | PeakState::Failed(_) => None,
        };
        let duration = peaks.as_ref().map_or_else(
            || file.info().duration_secs(),
            |data| data.frames() as f64 / f64::from(data.sample_rate()),
        );
        (duration.is_finite() && duration > 0.0).then(|| Waveform {
            duration,
            file: file.id().clone(),
            peaks,
        })
    }

    pub fn clear(&mut self) {
        self.sources.clear();
        self.worker = None;
        self.worker_retry = None;
    }

    pub fn retain_visible(&mut self, ui: &egui::Context) {
        let frame = ui.cumulative_frame_nr();
        self.sources.retain(|_, source| source.frame == frame);
    }
}

fn sample_slot(card: &Card, id: u32) -> bool {
    card.asset_slots
        .iter()
        .any(|(slot, _)| slot.id == id && slot.kind == AssetKind::AudioSample)
}

/// Source milliseconds per unit of a fractional trim parameter on the visible page.
pub(super) fn trim_milliseconds(card: &Card, param: u32) -> Option<f64> {
    card.native_page()?.cells.iter().find_map(|cell| {
        let graph = cell.graph.as_ref()?;
        if graph.trim_unit != Some(TrimUnit::SourceFraction)
            || !["start", "end"]
                .iter()
                .any(|slot| graph.slots.get(*slot) == Some(&param))
        {
            return None;
        }
        let source = card.asset_waveforms.get(&graph.asset_slot?)?;
        let duration = source.duration * 1000.0;
        (duration.is_finite() && duration > 0.0).then_some(duration)
    })
}

pub(super) fn dropped_file(ui: &Ui, card: &Card, rect: Rect, slot: u32) -> Option<PathBuf> {
    if !ui.is_enabled()
        || !sample_slot(card, slot)
        || !ui.input(|input| {
            input
                .pointer
                .latest_pos()
                .is_some_and(|pos| rect.intersect(ui.clip_rect()).contains(pos))
        })
    {
        return None;
    }
    if ui.input(|input| input.raw.hovered_files.len() == 1) {
        komponist_ui::state::paint_select_ring(ui.painter(), rect, komponist_tokens::radius::M);
    }
    ui.input_mut(|input| {
        let [file] = input.raw.dropped_files.as_slice() else {
            return None;
        };
        let path = file.path();
        if path.as_os_str().is_empty() {
            return None;
        }
        let path = path.to_owned();
        input.raw.dropped_files.clear();
        Some(path)
    })
}

#[derive(Clone)]
struct ViewState {
    file: FileId,
    duration: f64,
    padding: [f32; 2],
    viewport: Viewport,
}

impl ViewState {
    fn wheel(&self, ui: &Ui, rect: Rect) -> Option<Viewport> {
        let plot = Rect::from_min_max(
            pos2(rect.left() + self.padding[0], rect.top()),
            pos2(rect.right() - self.padding[1], rect.bottom()),
        );
        self.viewport
            .wheel(ui, plot, self.duration, WAVEFORM_MIN_ZOOM)
    }
}

fn view_id(card: &Card, cell: &Cell, slot: u32) -> Id {
    Id::unique((
        "asset-waveform-view",
        card.device,
        card.page,
        cell.row,
        cell.col,
        slot,
    ))
}

fn view_state(ui: &Ui, id: Id, source: &Waveform) -> ViewState {
    ui.data(|data| data.get_temp::<ViewState>(id))
        .filter(|state| state.file == source.file && state.duration == source.duration)
        .unwrap_or_else(|| ViewState {
            file: source.file.clone(),
            duration: source.duration,
            padding: [0.0; 2],
            viewport: Viewport::fit(source.duration),
        })
}

fn waveform_rect(rect: Rect, toolbar: bool) -> Rect {
    let mut rect = rect;
    if toolbar {
        rect.min.y += size::CONTROL_COMPACT + space::SPACE_2;
    }
    rect
}

/// A full sample editor includes trim flags and time labels in its waveform plot.
pub(super) fn style(spec: &mut GraphSpec) {
    spec.x.pad = [space::SPACE_1_5; 2];
    spec.y.pad = [space::SPACE_3, space::SPACE_2];
    for (handle, head) in spec
        .handles
        .iter_mut()
        .zip([MarkerHead::Right, MarkerHead::Left])
    {
        handle.look = HandleLook::Marker(head);
    }
    for layer in &mut spec.layers {
        if matches!(layer, Layer::Marker(Marker::Playhead)) {
            *layer = Layer::Marker(Marker::FullHeightPlayhead);
        }
    }
}

pub(super) fn time_ticks(spec: &mut GraphSpec) {
    let width = spec.x.max - spec.x.min;
    let ticks = (1..SAMPLER_TICK_COUNT)
        .map(|index| {
            let time = spec.x.min + width * f64::from(index) / f64::from(SAMPLER_TICK_COUNT);
            Tick::major(time, format_number(time, if width >= 1.0 { 1 } else { 3 }))
        })
        .collect();
    spec.x.grid = Grid::Ticks(ticks);
    spec.x.labels = true;
}

pub(super) fn preview_enabled(ui: &Ui, card: &Card, cell: &Cell, slot: u32) -> bool {
    let key = view_id(card, cell, slot).with("preview-disabled");
    !ui.data(|data| data.get_temp::<bool>(key).unwrap_or_default())
}

/// Draw the optional host controls above a native waveform and return its plot rectangle.
pub(super) fn toolbar(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    rect: Rect,
    native: &Graph,
    playhead: Option<f32>,
) -> Rect {
    let Some(slot) = native.asset_slot.filter(|_| native.waveform_toolbar) else {
        return rect;
    };
    let plot = waveform_rect(rect, true);
    let Some((asset, linked)) = card.asset_slots.iter().find(|(asset, _)| asset.id == slot) else {
        return plot;
    };
    let t = komponist_ui::lang(ui.ctx());
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), size::CONTROL_COMPACT));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    ui.spacing_mut().item_spacing.x = device::grid::GAP;
    let source = card.asset_waveforms.get(&slot);
    let name = match &asset.status {
        AssetStatus::Empty => t.devices_load_sample(),
        AssetStatus::Loading => t.devices_pad_loading(),
        AssetStatus::Ready => card
            .asset_names
            .get(&slot)
            .map_or(asset.label.as_str(), String::as_str),
        AssetStatus::Missing => t.devices_pad_missing(),
        AssetStatus::Failed(_) => t.devices_pad_failed(),
    };
    sample_button(&mut ui, out, card, slot, &asset.status, *linked, name);
    let ready = source.filter(|_| asset.status == AssetStatus::Ready);
    let has_preview = native.audition_note.is_some();
    let icons = if has_preview { 6.0 } else { 5.0 };
    let controls_width = icons * (size::CONTROL + device::grid::GAP);
    if rect.width() >= SAMPLER_FILE_WIDTH + SAMPLER_FILE_INFO_WIDTH + controls_width
        && let Some(source) = ready
    {
        source_metadata(&mut ui, source, t);
    }
    let key = view_id(card, cell, slot);
    let mut state = ready.map(|source| view_state(&ui, key, source));
    let preview_key = key.with("preview-disabled");
    let mut preview = preview_enabled(&ui, card, cell, slot);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let source_length = ready.map(|source| source.duration);
        let viewport = state.as_mut().map(|state| &mut state.viewport);
        let controls = viewport_controls(ui, t, source_length, viewport.as_deref());
        if has_preview
            && ui
                .add(
                    Button::icon(Icon::Volume2, t.devices_sampler_click_preview())
                        .small()
                        .on(preview),
                )
                .clicked()
        {
            preview = !preview;
            ui.data_mut(|data| data.insert_temp(preview_key, !preview));
        }
        if let Some((viewport, length)) = viewport.zip(source_length)
            && let Some(control) = controls
        {
            update_viewport(viewport, length, playhead, control);
        }
    });
    if let Some(state) = state {
        ui.data_mut(|data| data.insert_temp(key, state));
    }
    plot
}

#[derive(Clone, Copy)]
enum SampleFile {
    Pick,
    Remove,
}

fn sample_button(
    ui: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    slot: u32,
    status: &AssetStatus,
    linked: bool,
    name: &str,
) {
    let t = komponist_ui::lang(ui.ctx());
    let style = typography::SMALL;
    let visible = crate::views::elided_galley(
        ui,
        name,
        komponist_ui::widgets::font_at(&style, komponist_ui::widgets::metrics::MEDIUM),
        style.line_height,
        SAMPLER_FILE_WIDTH - 2.0 * space::SPACE_2,
    );
    let visible = visible.rows.first().map(|row| row.text());
    let file = ui.add(
        Button::new(visible.as_deref().unwrap_or(name))
            .name(name)
            .small()
            .width(SAMPLER_FILE_WIDTH),
    );
    let detail = match status {
        AssetStatus::Failed(reason) => reason,
        _ => name,
    };
    file.clone().on_hover_text(detail);
    let pick = PluginAction::PickPluginAsset {
        device: card.device,
        slot,
    };
    if file.clicked() {
        if *status == AssetStatus::Ready {
            ContextMenu::<SampleFile>::open_at(ui.ctx(), file.id, file.rect.left_bottom());
        } else {
            out.push(Action::Plugin(pick.clone()));
        }
    }
    let load = if *status == AssetStatus::Missing {
        t.devices_search()
    } else {
        t.devices_load_sample()
    };
    let remove = Entry::action(t.devices_remove_sample(), SampleFile::Remove);
    let entries = [
        Entry::action(load, SampleFile::Pick),
        if linked { remove } else { remove.disabled() },
    ];
    match ContextMenu::new(t.devices_sampler_sample_menu(), &entries).show_on(&file) {
        Some(SampleFile::Pick) => out.push(Action::Plugin(pick)),
        Some(SampleFile::Remove) => out.push(Action::Plugin(PluginAction::SetPluginAsset {
            device: card.device,
            slot,
            path: None,
        })),
        None => {}
    }
}

fn source_metadata(ui: &mut Ui, source: &Waveform, t: Lang) {
    let seconds = format_number(source.duration, 2);
    let duration = t.devices_sampler_length(if t == Lang::De {
        seconds.replace('.', ",")
    } else {
        seconds
    });
    let text = source.peaks.as_ref().map_or_else(
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
    let (rect, response) = ui.allocate_exact_size(
        vec2(SAMPLER_FILE_INFO_WIDTH, size::CONTROL_COMPACT),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        fonts::font_id(&typography::SMALL),
        color32(color::TEXT_2),
    );
}

/// Stretching can lag a linked file's availability. Display its status in open space beside a
/// waveform editor while preserving the source's ready state.
pub(super) fn preparation(ui: &mut Ui, card: &Card, cell: &Cell, rect: Rect, status: Preparation) {
    let Some(page) = card.current_page() else {
        return;
    };
    let start = cell.col + cell.cols;
    let cols = page.cols.saturating_sub(start);
    if cols == 0 {
        return;
    }
    let slot = Cell::half(
        start,
        1,
        cols,
        Half::Bottom,
        WidgetKind::Number,
        CellParam::None,
    );
    let local = cell_rect(&slot);
    if page
        .cells
        .iter()
        .any(|other| cell_rect(other).intersects(local))
    {
        return;
    }
    let origin = rect.min - cell_rect(cell).min.to_vec2();
    let rect = local.translate(origin.to_vec2());
    let t = komponist_ui::lang(ui.ctx());
    let label = match status {
        Preparation::Preparing => t.devices_sampler_sync_preparing(),
        Preparation::Unavailable => t.devices_sampler_sync_unavailable(),
    };
    let response = ui.allocate_rect(rect, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        fonts::font_id(&typography::SMALL),
        color32(color::TEXT_2),
    );
}

#[derive(Clone, Copy)]
enum ViewControl {
    Fit,
    ZoomIn,
    ZoomOut,
    Later,
    Earlier,
}

fn viewport_controls(
    ui: &mut Ui,
    t: Lang,
    length: Option<f64>,
    view: Option<&Viewport>,
) -> Option<ViewControl> {
    let is_zoomed = length
        .zip(view)
        .is_some_and(|(length, view)| view.width() < length);
    let can_zoom = length
        .zip(view)
        .is_some_and(|(length, view)| view.width() > WAVEFORM_MIN_ZOOM.min(length));
    let mut clicked = None;
    for (enabled, icon, name, control) in [
        (
            is_zoomed,
            Icon::Maximize2,
            t.devices_sampler_fit(),
            ViewControl::Fit,
        ),
        (can_zoom, Icon::Plus, t.audio_zoom_in(), ViewControl::ZoomIn),
        (
            is_zoomed,
            Icon::Minus,
            t.audio_zoom_out(),
            ViewControl::ZoomOut,
        ),
        (
            is_zoomed,
            Icon::ChevronRight,
            t.devices_sampler_later(),
            ViewControl::Later,
        ),
        (
            is_zoomed,
            Icon::ChevronLeft,
            t.devices_sampler_earlier(),
            ViewControl::Earlier,
        ),
    ] {
        if ui
            .add_enabled(enabled, Button::icon(icon, name).small())
            .clicked()
        {
            clicked = Some(control);
        }
    }
    clicked
}

fn update_viewport(view: &mut Viewport, length: f64, playhead: Option<f32>, control: ViewControl) {
    let width = view.width();
    let centre = view.centre();
    let (anchor, width) = match control {
        ViewControl::Fit => {
            *view = Viewport::fit(length);
            return;
        }
        ViewControl::ZoomIn | ViewControl::ZoomOut => {
            let anchor = playhead
                .map(|position| f64::from(position) * length)
                .filter(|&position| view.contains(position))
                .unwrap_or(centre);
            let factor = if matches!(control, ViewControl::ZoomIn) {
                0.5
            } else {
                2.0
            };
            (anchor, width * factor)
        }
        ViewControl::Earlier => (centre - 0.5 * width, width),
        ViewControl::Later => (centre + 0.5 * width, width),
    };
    view.set(anchor, width, length, WAVEFORM_MIN_ZOOM);
}

pub(super) fn view(
    ui: &Ui,
    card: &Card,
    cell: &Cell,
    rect: Rect,
    padding: [f32; 2],
    slot: u32,
) -> Option<Viewport> {
    let memory = view_id(card, cell, slot);
    let Some(source) = card.asset_waveforms.get(&slot) else {
        ui.data_mut(|data| data.remove::<ViewState>(memory));
        return None;
    };
    let mut state = view_state(ui, memory, source);
    state.padding = padding;
    if !ui.input(|input| input.pointer.primary_down())
        && ui.rect_contains_pointer(rect)
        && let Some(next) = state.wheel(ui, rect)
    {
        state.viewport = next;
        ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
    }
    let viewport = state.viewport.clone();
    ui.data_mut(|data| data.insert_temp(memory, state));
    Some(viewport)
}

/// Give an authored waveform priority when it can act on wheel input.
pub(super) fn takes_wheel(ui: &Ui, card: &Card, origin: Pos2, clip: Rect) -> bool {
    if card.native_layout.is_none()
        || card.collapsed
        || card.state != CardState::Ok
        || card.warning.is_some()
        || ui.input(|input| input.pointer.primary_down())
    {
        return false;
    }
    card.current_page()
        .into_iter()
        .flat_map(|page| &page.cells)
        .any(|cell| {
            let Some((graph, slot)) = card.native_cell(cell).and_then(|native| {
                let graph = native.graph.as_ref()?;
                Some((graph, graph.asset_slot?))
            }) else {
                return false;
            };
            let rect = waveform_rect(
                cell_rect(cell).translate(origin.to_vec2()),
                graph.waveform_toolbar,
            );
            ui.rect_contains_pointer(rect.intersect(clip))
                && ui
                    .data(|data| data.get_temp::<ViewState>(view_id(card, cell, slot)))
                    .is_some_and(|state| state.wheel(ui, rect).is_some())
        })
}

pub(super) fn reset(ui: &Ui, card: &Card, cell: &Cell, slot: u32) {
    ui.data_mut(|data| data.remove::<ViewState>(view_id(card, cell, slot)));
    ui.ctx().request_repaint();
}

pub(super) fn recenter(ui: &Ui, card: &Card, cell: &Cell, slot: u32, view: &Viewport, time: f64) {
    let Some(source) = card.asset_waveforms.get(&slot) else {
        return;
    };
    let key = view_id(card, cell, slot);
    let mut state = view_state(ui, key, source);
    state
        .viewport
        .set(time, view.width(), source.duration, WAVEFORM_MIN_ZOOM);
    ui.data_mut(|data| data.insert_temp(key, state));
    ui.ctx().request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_reads_waveform_peaks_from_the_visible_source_frames() {
        let mut samples = vec![0.0; 48_000];
        samples[2_400..2_600].fill(0.8);
        let source = Waveform {
            duration: 1.0,
            file: FileId {
                path: "sample.wav".into(),
                len: 0,
                mtime_ns: None,
            },
            peaks: Some(Arc::new(PeakData::from_planar(48_000, &[samples]))),
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
            source
                .amplitudes(&before, 40)
                .iter()
                .any(|&peak| peak > 0.7)
        );
        assert!(
            source
                .amplitudes(&after, 40)
                .iter()
                .all(|&peak| peak == 0.0)
        );
    }
}
