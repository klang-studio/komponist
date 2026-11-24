//! Host controls for revisioned slice options on an authored asset waveform.

use std::ops::Range;

use egui::{Align, Id, Layout, PointerButton, Rect, Ui, UiBuilder};
use komponist_i18n::Lang;
use komponist_ipc::waveform_markers::{
    Edit, MAX_FADE_MS, MAX_MARKERS, Request, SliceOptions, Snapshot,
};
use komponist_model::format::note_name;
use komponist_tokens::device;
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::device::choice::DropdownCell;
use komponist_ui::widgets::device::graph::{GraphOutput, GraphSpec, Layer, Region, SlotInfo};
use komponist_ui::widgets::device::number::Number;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::format_number;

use super::asset_waveform::{self, Waveform};
use super::layout::{Cell, CellParam, Half, WidgetKind, cell_rect};
use super::metrics::SAMPLER_FILE_WIDTH;
use super::model::Card;
use super::viewport::Viewport;

/// The fixed controls a wave's slice options temporarily replace.
pub(super) fn hides_control(card: &Card, param: u32) -> bool {
    card.native_page().is_some_and(|page| {
        page.cells.iter().any(|cell| {
            cell.graph.as_ref().is_some_and(|graph| {
                graph
                    .waveform_slice_controls
                    .is_some_and(|ids| ids.contains(&param))
                    && card
                        .graph_signals
                        .get(&(cell.row, cell.col))
                        .is_some_and(|signals| {
                            signals
                                .markers
                                .as_ref()
                                .is_some_and(|markers| markers.slice_options.is_some())
                        })
            })
        })
    })
}

#[derive(Clone, Copy)]
struct PendingOptions {
    revision: u64,
    slice: u8,
    options: SliceOptions,
}

#[derive(Clone, Copy)]
struct Selection {
    seconds: f64,
    requested_at: Option<u64>,
}

pub(super) struct SliceView<'a> {
    key: Id,
    snapshot: &'a Snapshot,
    options: &'a [SliceOptions],
    trim: Range<f64>,
    root: u8,
    selected: Option<usize>,
    pending: Option<Selection>,
}

impl<'a> SliceView<'a> {
    pub fn new(
        ui: &Ui,
        key: Id,
        snapshot: &'a Snapshot,
        trim: Range<f64>,
        root: u8,
    ) -> Option<Self> {
        let options = snapshot.slice_options.as_deref()?;
        let selection = ui.data(|data| data.get_temp::<Selection>(key));
        let mut view = Self {
            key,
            snapshot,
            options,
            trim,
            root,
            selected: None,
            pending: None,
        };
        view.selected = selection
            .map(|selection| selection.seconds)
            .filter(|&seconds| view.active(seconds))
            .and_then(|seconds| {
                snapshot
                    .positions
                    .binary_search_by(|time| time.total_cmp(&seconds))
                    .ok()
            });
        if view.selected.is_none() {
            view.pending =
                selection.filter(|selection| selection.requested_at == Some(snapshot.revision));
        }
        Some(view)
    }

    fn active(&self, seconds: f64) -> bool {
        self.trim.start < seconds && seconds < self.trim.end
    }

    pub fn marker_name(&self, t: Lang, index: usize) -> String {
        let seconds = self.snapshot.positions[index];
        let first = self
            .snapshot
            .positions
            .partition_point(|&time| time <= self.trim.start);
        if self.active(seconds) && usize::from(self.root) + index + 1 - first < 128 {
            return note_name((usize::from(self.root) + index + 1 - first) as u8);
        }
        t.devices_sampler_slice(index + 1)
    }

    fn slice(&self) -> u8 {
        self.selected.map_or_else(
            || {
                self.snapshot
                    .positions
                    .partition_point(|&seconds| seconds <= self.trim.start)
            },
            |index| index + 1,
        ) as u8
    }

    fn options(&self) -> SliceOptions {
        self.options[usize::from(self.slice())]
    }

    fn effective(&self, options: SliceOptions) -> (bool, [f64; 2]) {
        let defaults = self.snapshot.default_options.unwrap_or_default();
        (
            options.reverse.or(defaults.reverse).unwrap_or(false),
            [
                f64::from(options.fade_in_ms.or(defaults.fade_in_ms).unwrap_or(0.0)),
                f64::from(options.fade_out_ms.or(defaults.fade_out_ms).unwrap_or(0.0)),
            ],
        )
    }

    pub fn select_handle(&self, ui: &Ui, graph: Id, first: usize) {
        let key = graph.with("slice-selection");
        let handle = self.selected.map(|index| first + index);
        if ui.data(|data| data.get_temp::<Option<usize>>(key)) != Some(handle) {
            komponist_ui::widgets::device::graph::Graph::select_handle(ui.ctx(), graph, handle);
            ui.data_mut(|data| data.insert_temp(key, handle));
        }
    }

    pub fn add_layers(
        &self,
        spec: &mut GraphSpec,
        values: &mut Vec<f64>,
        infos: &mut Vec<SlotInfo<'_>>,
        first_marker: usize,
    ) {
        let Some((start, end)) = spec.slot("start").zip(spec.slot("end")) else {
            return;
        };
        let mut bounds = [start, end];
        let mut scales = [0.001; 2];
        if let Some(index) = self.selected {
            bounds[0] = first_marker + index;
            scales[0] = 1.0;
        }
        let start = self
            .selected
            .map_or(self.trim.start, |index| self.snapshot.positions[index]);
        if let Some(index) = self
            .snapshot
            .positions
            .iter()
            .position(|&time| start < time && time < self.trim.end)
        {
            bounds[1] = first_marker + index;
            scales[1] = 1.0;
        }
        spec.layers
            .push(Layer::Region(Region::Inside { bounds, scales }));
        let (reverse, fades) = self.effective(self.options());
        add_fades(spec, values, infos, bounds, scales, reverse, fades);
    }

    pub fn observe(&mut self, ui: &Ui, shown: &GraphOutput, first_marker: usize) {
        if (shown.touch.clicked || shown.touch.dragging)
            && let Some(index) = shown
                .selected_slot
                .and_then(|slot| slot.checked_sub(first_marker))
            && self
                .snapshot
                .positions
                .get(index)
                .is_some_and(|&seconds| self.active(seconds))
        {
            self.selected = Some(index);
            self.pending = None;
        }
        let primary = PointerButton::Primary;
        if ui.input(|input| input.pointer.button_pressed(primary))
            && (shown.response.is_pointer_button_down_on() || shown.response.clicked_by(primary))
            && let Some(seconds) = shown
                .pointer_x
                .filter(|seconds| self.trim.contains(seconds))
        {
            self.selected = self
                .snapshot
                .positions
                .iter()
                .rposition(|&time| time <= seconds && self.active(time));
            self.pending = None;
        }
    }

    // The waveform cell supplies the row geometry, source viewport, and current playback cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        card: &Card,
        cell: &Cell,
        rect: Rect,
        slot: u32,
        viewport: Option<&Viewport>,
        playhead: Option<f64>,
        existing: Option<Request>,
    ) -> Option<Request> {
        let t = komponist_ui::lang(ui.ctx());
        let origin = rect.min - cell_rect(cell).min.to_vec2();
        let rect_at = |col, cols, half| {
            let widget = Cell::half(
                cell.col + col,
                2,
                cols,
                half,
                WidgetKind::Number,
                CellParam::None,
            );
            cell_rect(&widget).translate(origin.to_vec2())
        };
        let enabled = self.snapshot.editable && !card.macro_mapping;
        let mut ui = ui.new_child(UiBuilder::new().id_salt(self.key));
        if !enabled {
            ui.disable();
        }
        let mut tools = ui.new_child(
            UiBuilder::new()
                .max_rect(rect_at(0, cell.cols, Half::Bottom))
                .layout(Layout::left_to_right(Align::Center)),
        );
        tools.spacing_mut().item_spacing.x = device::grid::GAP;
        let source = card.asset_waveforms.get(&slot);
        let before_tools = self.selected;
        let from_tools = self.tools(&mut tools, t, source, viewport, playhead);
        let settings = self.settings(&mut ui, t, rect_at);
        let operation = existing
            .map(|request| request.operation)
            .or(from_tools)
            .or(settings);
        let selected = match operation {
            Some(Edit::Insert { seconds }) => Some(Selection {
                seconds,
                requested_at: Some(self.snapshot.revision),
            }),
            Some(Edit::Move { index, seconds }) if self.selected == Some(usize::from(index)) => {
                Some(Selection {
                    seconds,
                    requested_at: Some(self.snapshot.revision),
                })
            }
            Some(Edit::Remove { index }) => self.snapshot.positions[..usize::from(index)]
                .iter()
                .rfind(|&&time| self.active(time))
                .map(|&seconds| Selection {
                    seconds,
                    requested_at: None,
                }),
            Some(Edit::DetectSlices) => None,
            _ => self
                .selected
                .map(|index| Selection {
                    seconds: self.snapshot.positions[index],
                    requested_at: None,
                })
                .or(self.pending),
        };
        if let Some(selection) = selected {
            ui.data_mut(|data| data.insert_temp(self.key, selection));
        } else {
            ui.data_mut(|data| data.remove::<Selection>(self.key));
        }
        if (matches!(from_tools, Some(Edit::Insert { .. })) || before_tools != self.selected)
            && let Some(time) = selected
                .map(|selection| selection.seconds)
                .or_else(|| (before_tools != self.selected).then_some(self.trim.start))
            && let Some(view) = viewport.filter(|view| !view.contains(time))
        {
            asset_waveform::recenter(&ui, card, cell, slot, view, time);
        }
        operation.map(|operation| Request {
            revision: self.snapshot.revision,
            operation,
        })
    }

    fn tools(
        &mut self,
        row: &mut Ui,
        t: Lang,
        source: Option<&Waveform>,
        viewport: Option<&Viewport>,
        playhead: Option<f64>,
    ) -> Option<Edit> {
        let detect = row
            .add(Button::new(t.devices_sampler_detect()).small())
            .clicked();
        let add = row
            .add_enabled(
                self.snapshot.positions.len() < MAX_MARKERS,
                Button::new(t.devices_sampler_add_slice()).small(),
            )
            .clicked();
        let remove = row
            .add_enabled(
                self.selected.is_some(),
                Button::new(t.devices_sampler_remove_slice()).small(),
            )
            .clicked();
        let positions = &self.snapshot.positions;
        let mut notes = vec![(None, t.devices_sampler_slice_start(note_name(self.root)))];
        notes.extend(
            positions
                .iter()
                .enumerate()
                .filter(|&(_, &seconds)| self.active(seconds))
                .map(|(index, &seconds)| {
                    (
                        Some(index),
                        t.devices_sampler_slice_row(
                            self.marker_name(t, index),
                            format_number(seconds * 1000.0, 0),
                        ),
                    )
                }),
        );
        let items: Vec<_> = notes.iter().map(|(_, text)| Item::new(text)).collect();
        let mut selected = notes
            .iter()
            .position(|(index, _)| *index == self.selected)
            .unwrap_or(0);
        if row
            .add(
                Dropdown::new(t.devices_sampler_midi_assignment(), &mut selected, &items)
                    .device()
                    .width(SAMPLER_FILE_WIDTH),
            )
            .changed()
        {
            self.selected = notes[selected].0;
            self.pending = None;
        }
        if detect {
            return Some(Edit::DetectSlices);
        }
        if remove {
            return self
                .selected
                .map(|index| Edit::Remove { index: index as u8 });
        }
        if add {
            let cursor = playhead
                .filter(|&time| self.active(time))
                .unwrap_or_else(|| {
                    let visible = viewport.and_then(|view| {
                        let start = view.start.max(self.trim.start);
                        let end = view.end.min(self.trim.end);
                        (start < end).then_some(start.midpoint(end))
                    });
                    visible.unwrap_or_else(|| self.trim.start.midpoint(self.trim.end))
                });
            let spacing = 1.0
                / f64::from(
                    source
                        .and_then(|source| source.peaks.as_ref())
                        .map_or(48_000, |peaks| peaks.sample_rate()),
                );
            return insertion_position(positions, cursor, &self.trim, spacing)
                .map(|seconds| Edit::Insert { seconds });
        }
        None
    }

    fn settings(&self, row: &mut Ui, t: Lang, rect: impl Fn(u8, u8, Half) -> Rect) -> Option<Edit> {
        let key = self.key.with("slice-options-drag");
        let slice = self.slice();
        let original = self.options();
        let pending = row
            .data(|data| data.get_temp::<PendingOptions>(key))
            .filter(|pending| pending.slice == slice && pending.revision == self.snapshot.revision);
        let mut options = pending.map_or(original, |pending| pending.options);
        let (reverse, fades) = self.effective(options);
        let mut gate = usize::from(options.gate);
        let triggers = [
            Item::new(t.choice_sampler_mode_one_shot()),
            Item::new(t.choice_sampler_mode_gate()),
        ];
        if row
            .put(
                rect(2, 2, Half::Top),
                DropdownCell::new(t.devices_sampler_slice_trigger(), &triggers, &mut gate),
            )
            .changed()
        {
            options.gate = gate != 0;
        }
        let mut direction = usize::from(reverse);
        let directions = [
            Item::new(t.param_general_sampler_forward()),
            Item::new(t.param_general_sampler_reverse()),
        ];
        if row
            .put(
                rect(4, 2, Half::Top),
                DropdownCell::new(
                    t.param_general_sampler_direction(),
                    &directions,
                    &mut direction,
                ),
            )
            .changed()
        {
            options.reverse = Some(direction != 0);
        }
        let mut dragging = false;
        for (col, label, mut ms, target) in [
            (
                6,
                t.param_general_sampler_fade_in(),
                fades[0],
                &mut options.fade_in_ms,
            ),
            (
                8,
                t.param_general_sampler_fade_out(),
                fades[1],
                &mut options.fade_out_ms,
            ),
        ] {
            let response = row.put(
                rect(col, 2, Half::Top),
                Number::new(&mut ms, label)
                    .range(0.0..=f64::from(MAX_FADE_MS))
                    .step(1.0)
                    .unit("ms")
                    .cols(2),
            );
            if response.changed() {
                *target = Some(ms as f32);
            }
            dragging |= response.dragged() || response.drag_started();
        }
        row.data_mut(|data| data.remove::<PendingOptions>(key));
        if row.input(|input| !input.focused || input.key_pressed(egui::Key::Escape))
            || !row.is_enabled()
        {
            return None;
        }
        if dragging {
            row.data_mut(|data| {
                data.insert_temp(
                    key,
                    PendingOptions {
                        revision: self.snapshot.revision,
                        slice,
                        options,
                    },
                )
            });
            return None;
        }
        (options != original).then_some(Edit::SetSliceOptions { slice, options })
    }
}

pub(super) fn playback_fades(
    spec: &mut GraphSpec,
    values: &mut Vec<f64>,
    infos: &mut Vec<SlotInfo<'_>>,
    options: SliceOptions,
) {
    if let Some((start, end)) = spec.slot("start").zip(spec.slot("end")) {
        let fades =
            [options.fade_in_ms, options.fade_out_ms].map(|fade| f64::from(fade.unwrap_or(0.0)));
        add_fades(
            spec,
            values,
            infos,
            [start, end],
            [0.001; 2],
            options.reverse.unwrap_or(false),
            fades,
        );
    }
}

fn add_fades(
    spec: &mut GraphSpec,
    values: &mut Vec<f64>,
    infos: &mut Vec<SlotInfo<'_>>,
    bounds: [usize; 2],
    scales: [f64; 2],
    reverse: bool,
    fades: [f64; 2],
) {
    let index = values.len();
    for (name, fade) in ["sample-fade-in", "sample-fade-out"].into_iter().zip(fades) {
        spec.slots.push(name.into());
        values.push(fade / 1000.0);
        infos.push(SlotInfo::new(0.0, f64::from(MAX_FADE_MS) / 1000.0, 0.0).locked());
    }
    let durations = if reverse {
        [index + 1, index]
    } else {
        [index, index + 1]
    };
    spec.layers.push(Layer::Region(Region::Fades {
        bounds,
        scales,
        durations,
    }));
}

pub(super) fn insertion_position(
    positions: &[f64],
    cursor: f64,
    trim: &Range<f64>,
    spacing: f64,
) -> Option<f64> {
    let valid = |time: f64| {
        trim.start < time
            && time < trim.end
            && !positions
                .iter()
                .any(|&prior| (time - prior).abs() < spacing)
    };
    if valid(cursor) {
        return Some(cursor);
    }
    let edges = std::iter::once(f64::NEG_INFINITY)
        .chain(positions.iter().copied())
        .chain(std::iter::once(f64::INFINITY));
    edges
        .clone()
        .zip(edges.skip(1))
        .filter_map(|(left, right)| {
            let min = (left + spacing).max(trim.start);
            let max = (right - spacing).min(trim.end);
            if min > max {
                return None;
            }
            let preferred = cursor.clamp(min, max);
            [
                preferred,
                preferred.next_up(),
                preferred.next_down(),
                min.midpoint(max),
            ]
            .into_iter()
            .find(|&time| valid(time))
        })
        .min_by(|a, b| (a - cursor).abs().total_cmp(&(b - cursor).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_slice_finds_a_nearby_gap_when_the_preferred_position_is_blocked() {
        let markers = [2.0, 2.00003];
        let interval = 0.0..4.0;
        let spacing = 1.0 / 48_000.0;
        let position = insertion_position(&markers, 2.0, &interval, spacing).unwrap();
        assert!(interval.start < position && position < interval.end);
        assert!(
            markers
                .into_iter()
                .all(|marker| (position - marker).abs() >= spacing)
        );
        assert!((position - 2.0).abs() < 2.0 * spacing);
        assert_eq!(
            insertion_position(&markers, 1.0, &interval, spacing),
            Some(1.0)
        );
    }
}
