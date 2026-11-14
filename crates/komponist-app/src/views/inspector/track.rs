//! The track section (`trackSec`: name with colour bar, the 14 colour swatches, Instrument,
//! Eingang, Kanal) and the devices list (`sec({title:'Geräte', meta:'+ Effekt', devs})`, `D`).

use egui::{Rangef, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_ipc::PluginKind;
use komponist_model::edits::TrackProp;
use komponist_model::{DeviceRole, Hue, MidiChannelSel, MidiInputSel, PluginRef, Project, Track};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, typography};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::context_menu::HUE_NAMES;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{focus, fonts, px};

use super::model::{self, DeviceRow, RowStatus, names};
use super::{Col, Inspector, Meta, RenameTarget, frame, label, metrics as m, place};
use crate::actions::{Action, DropTarget, EditAction, PluginAction, UiAction};
use crate::ui_state::BrowserSegment;
use crate::views::ViewCtx;

/// The track section of `track`.
pub(super) fn section(
    insp: &mut Inspector,
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    track: &Track,
) {
    let index = project.track_index(track.id).unwrap_or(0);
    let position = names::position(index, project.tracks.len());
    let chip = crate::views::mixer::model::base_color(track.hue.0);
    frame(ui, x, y, names::TRACK, Meta::Text(&position), |ui, col| {
        let target = RenameTarget::Track(track.id);
        let (name_rect, rename) =
            super::name_row(ui, col, &track.name, chip, true, !insp.renaming(&target));
        if rename {
            insp.start_rename(target.clone(), &track.name);
        }
        let id = track.id;
        insp.rename_field(ui, &target, &track.name, name_rect, |name| {
            ctx.send(Action::Edit(EditAction::SetTrackProp {
                tracks: vec![id],
                prop: TrackProp::Name(name),
            }));
        });
        swatches(ui, ctx, col, track);
        dropdowns(ui, ctx, col, track);
    });
}

/// The 14 track colours in one row; the current one with a `text.1` ring 1 px away; a click
/// sets the colour.
fn swatches(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, track: &Track) {
    let row = col.take(m::SWATCH_HEIGHT);
    let n = Hue::ALL.len();
    let w = (row.width() - (n - 1) as f32 * m::SWATCH_GAP) / n as f32;
    for (i, hue) in Hue::ALL.into_iter().enumerate() {
        let x = row.left() + i as f32 * (w + m::SWATCH_GAP);
        let rect = Rect::from_min_size(pos2(x, row.top()), vec2(w, m::SWATCH_HEIGHT));
        let response = ui.interact(rect, ui.scope_id().with(("swatch", hue.0)), Sense::click());
        let st = State::of(ui, &response);
        px::fill(
            ui.painter(),
            rect,
            radius::S,
            color32(crate::views::mixer::model::base_color(hue.0)),
        );
        let current = track.hue == hue;
        if current {
            px::outline(
                ui.painter(),
                rect,
                radius::S,
                m::SWATCH_RING_OFFSET,
                color32(color::TEXT_1),
            );
        } else if st.hovered {
            px::outline(
                ui.painter(),
                rect,
                radius::S,
                m::SWATCH_RING_OFFSET,
                color32(color::TEXT_3),
            );
        }
        if st.focused {
            focus::paint_ring(ui.painter(), rect, radius::S);
        }
        let name = names::swatch(HUE_NAMES[i]);
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, current, &name));
        Tip::new(HUE_NAMES[i]).attach(&response);
        if response.clicked() && !current {
            ctx.send(Action::Edit(EditAction::SetTrackProp {
                tracks: vec![track.id],
                prop: TrackProp::Hue(hue),
            }));
        }
    }
}

/// Instrument, Eingang and Kanal: keys in a 72 px column, the dropdowns filling the rest.
fn dropdowns(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, track: &Track) {
    let rows = 3;
    let h = rows as f32 * komponist_tokens::size::CONTROL + (rows - 1) as f32 * m::DD_ROW_GAP;
    let area = col.take(h);
    let field_left = area.left() + m::DD_KEY_WIDTH + m::DD_COLUMN_GAP;
    let row = |i: usize| {
        let top = area.top() + i as f32 * (komponist_tokens::size::CONTROL + m::DD_ROW_GAP);
        let key = Rect::from_x_y_ranges(
            area.left()..=area.left() + m::DD_KEY_WIDTH,
            top..=top + komponist_tokens::size::CONTROL,
        );
        let field = Rect::from_x_y_ranges(field_left..=area.right(), key.y_range());
        (key, field)
    };

    // Instrument: the catalog's instruments; the current one first if it is not among them.
    let (key, field) = row(0);
    super::field_key(ui, key, names::INSTRUMENT);
    let current = track
        .channel
        .devices
        .first()
        .filter(|d| d.role == DeviceRole::Instrument)
        .map(|d| d.plugin.clone());
    let mut plugins: Vec<PluginRef> = ctx
        .engine
        .host()
        .catalog()
        .entries()
        .iter()
        .filter(|e| e.kind == PluginKind::Instrument)
        .map(|e| e.plugin.clone())
        .collect();
    let catalog = ctx.engine.host().catalog();
    let selected_entry = current.as_ref().and_then(|c| {
        catalog
            .resolve(c)
            .and_then(|e| plugins.iter().position(|p| *p == e.plugin))
    });
    let mut labels: Vec<String> = plugins.iter().map(|p| p.name.clone()).collect();
    let mut enabled = vec![true; labels.len()];
    let mut selected = match (selected_entry, &current) {
        (Some(i), _) => i,
        (None, Some(c)) => {
            labels.insert(0, c.name.clone());
            plugins.insert(0, c.clone());
            enabled.insert(0, false);
            0
        }
        (None, None) => {
            labels.insert(0, names::NO_INSTRUMENT.to_owned());
            plugins.insert(0, no_instrument());
            enabled.insert(0, false);
            0
        }
    };
    let items: Vec<Item<'_>> = labels
        .iter()
        .zip(&enabled)
        .map(|(l, e)| {
            let item = Item::new(l);
            if *e { item } else { item.disabled() }
        })
        .collect();
    let before = selected;
    let r = place(ui, field, "instrument", |ui| {
        ui.add(Dropdown::new(names::INSTRUMENT, &mut selected, &items).width(field.width()))
    });
    if r.changed() && selected != before && enabled[selected] {
        ctx.send(Action::Plugin(PluginAction::Add {
            plugin: plugins[selected].clone(),
            target: DropTarget::Track(track.id),
        }));
    }

    // Eingang: all inputs, or one port by name (kept when the device is absent).
    let (key, field) = row(1);
    super::field_key(ui, key, names::INPUT);
    let mut ports: Vec<String> = ctx
        .engine
        .midi()
        .map(|h| h.ports().into_iter().map(|p| p.name).collect())
        .unwrap_or_default();
    if let MidiInputSel::Device(name) = &track.input
        && !ports.contains(name)
    {
        ports.push(name.clone());
    }
    let mut labels = vec![names::ALL_INPUTS.to_owned()];
    labels.extend(ports.iter().cloned());
    let mut selected = match &track.input {
        MidiInputSel::All => 0,
        MidiInputSel::Device(name) => ports.iter().position(|p| p == name).map_or(0, |i| i + 1),
    };
    let before = selected;
    let items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let r = place(ui, field, "input", |ui| {
        ui.add(Dropdown::new(names::INPUT, &mut selected, &items).width(field.width()))
    });
    if r.changed() && selected != before {
        let input = match selected {
            0 => MidiInputSel::All,
            i => MidiInputSel::Device(ports[i - 1].clone()),
        };
        ctx.send(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track.id],
            prop: TrackProp::Input(input),
        }));
    }

    // Kanal: Omni, 1 … 16.
    let (key, field) = row(2);
    super::field_key(ui, key, names::MIDI_CHANNEL);
    let labels: Vec<String> = std::iter::once(names::OMNI.to_owned())
        .chain((1..=16).map(|c| c.to_string()))
        .collect();
    let items: Vec<Item<'_>> = labels.iter().map(|l| Item::new(l)).collect();
    let mut selected = match track.midi_channel {
        MidiChannelSel::Omni => 0,
        MidiChannelSel::Channel(c) => usize::from(c) + 1,
    };
    let before = selected;
    let r = place(ui, field, "midi_channel", |ui| {
        ui.add(Dropdown::new(names::MIDI_CHANNEL, &mut selected, &items).width(field.width()))
    });
    if r.changed() && selected != before {
        let channel = match selected {
            0 => MidiChannelSel::Omni,
            c => MidiChannelSel::Channel((c - 1) as u8),
        };
        ctx.send(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track.id],
            prop: TrackProp::MidiChannel(channel),
        }));
    }
}

/// A placeholder reference for the disabled "Kein Instrument" entry (never sent).
fn no_instrument() -> PluginRef {
    PluginRef {
        format: komponist_model::PluginFormat::Builtin,
        id: String::new(),
        name: names::NO_INSTRUMENT.to_owned(),
        vendor: String::new(),
        version: String::new(),
        module: Box::default(),
    }
}

/// The devices list of `track`: a row per device (power square, name, status or format); a
/// click opens the plugin window, the square (or Alt-click) switches bypass; "+ Effekt" opens
/// the browser on Effekte.
pub(super) fn devices_section(
    insp: &mut Inspector,
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    x: Rangef,
    y: &mut f32,
    track: &Track,
) {
    let rows = model::device_rows(&track.channel.devices, ctx.engine.host());
    let now = ui.input(|i| i.time);
    let (_, add) = frame(
        ui,
        x,
        y,
        names::DEVICES,
        Meta::Action(names::ADD_EFFECT),
        |ui, col| {
            if rows.is_empty() {
                return;
            }
            let n = rows.len();
            let h = n as f32 * m::DEVICE_ROW + (n - 1) as f32 * m::DEVICE_GAP;
            let area = col.take(h);
            for (i, (row, tag)) in rows.iter().enumerate() {
                let top = area.top() + i as f32 * (m::DEVICE_ROW + m::DEVICE_GAP);
                let rect =
                    Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), m::DEVICE_ROW));
                let late = insp.late_state(ctx, row, now);
                device_row(ui, ctx, rect, row, tag, late);
            }
        },
    );
    if add.is_some_and(|r| r.clicked()) {
        ctx.send(Action::Ui(UiAction::FocusBrowserSearch));
        ctx.send(Action::Ui(UiAction::SetBrowserSegment(
            BrowserSegment::Effects,
        )));
    }
}

impl Inspector {
    /// The late-block count of a running device while it shows "zu spät".
    fn late_state(&mut self, ctx: &ViewCtx<'_>, row: &DeviceRow, now: f64) -> Option<u64> {
        let running = row.status == RowStatus::Ok;
        self.late.state(ctx.engine.host(), row.device, running, now)
    }
}

/// One device row (`D`): `surface.3` for the instrument (semibold), `surface.2` for effects; the
/// power square green when running, `surface.4` with a `text.3` contour when bypassed, `text.3`
/// while loading, `surface.4` when crashed or missing; the name `text.3` when bypassed, red when
/// crashed or missing; on the right the format tag in Mono, or the failure, or "zu spät".
fn device_row(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    rect: Rect,
    row: &DeviceRow,
    tag: &'static str,
    late: Option<u64>,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("device", row.device.0)),
        Sense::click(),
    );
    let st = State::of(ui, &response);
    let base = if row.instrument { 3 } else { 2 };
    px::fill(ui.painter(), rect, radius::M, state::surface_for(base, st));
    let inner = rect.shrink2(vec2(m::DEVICE_PAD_X, 0.0));

    // Power square.
    let power = Rect::from_min_size(
        pos2(inner.left(), rect.center().y - m::POWER / 2.0),
        vec2(m::POWER, m::POWER),
    );
    let failed = !matches!(row.status, RowStatus::Ok | RowStatus::Loading);
    let fill = if row.bypass || failed {
        color::SURFACE_4
    } else if row.status == RowStatus::Loading {
        color::TEXT_3
    } else {
        color::STATE_PLAY
    };
    px::fill(ui.painter(), power, radius::S, color32(fill));
    if row.bypass {
        let ppp = ui.pixels_per_point();
        ui.painter().rect_stroke(
            px::snap_rect(power, ppp),
            komponist_ui::theme::corner_radius(radius::S),
            egui::Stroke::new(px::line_width(ppp), color32(color::TEXT_3)),
            egui::StrokeKind::Inside,
        );
    }
    let power_hit = power.expand(m::DEVICE_ITEM_GAP / 2.0);
    let power_response = ui.interact(
        power_hit,
        ui.scope_id().with(("power", row.device.0)),
        Sense::click(),
    );
    let active = !row.bypass;
    power_response
        .widget_info(|| WidgetInfo::selected(Role::CheckBox, true, active, names::ACTIVE));
    Tip::new(names::ACTIVE)
        .detail("Bypass")
        .attach(&power_response);

    // Tag on the right.
    let style = typography::VALUE_S;
    let (tag_text, tag_ink) = match (late, row.status) {
        (Some(_), _) => (names::LATE, color::METER_LOUD),
        (None, RowStatus::Ok | RowStatus::Loading) => (row.tag(tag), color::TEXT_3),
        (None, _) => (row.tag(tag), color::STATE_ERROR),
    };
    let g = komponist_ui::widgets::galley(ui, tag_text, fonts::font_id(&style), style.line_height);
    let pos = komponist_ui::widgets::text_pos(
        inner,
        &g,
        inner.right() - g.size().x,
        ui.pixels_per_point(),
    );
    let tag_left = pos.x;
    ui.painter().galley(pos, g, color32(tag_ink));

    // Name.
    let ink = if failed {
        color::STATE_ERROR
    } else if row.bypass {
        color::TEXT_3
    } else {
        color::TEXT_1
    };
    let name_rect = Rect::from_x_y_ranges(
        power.right() + m::DEVICE_ITEM_GAP..=tag_left - m::DEVICE_ITEM_GAP,
        inner.y_range(),
    );
    let weight = row.instrument.then_some(super::SEMIBOLD);
    label(ui, name_rect, &row.name, &typography::UI, weight, ink);
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::M);
    }

    let name = names::device(&row.name);
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Button, true, &name);
        info.current_text_value = Some(tag_text.to_owned());
        info
    });
    let late_text = late.map(names::late_blocks);
    match &late_text {
        Some(t) => Tip::new(&row.name).detail(t).attach(&response),
        None => Tip::new(&row.name).attach(&response),
    }
    let alt = ui.input(|i| i.modifiers.alt);
    if power_response.clicked() || (response.clicked() && alt) {
        ctx.send(Action::Edit(EditAction::SetDeviceBypass {
            device: row.device,
            bypass: !row.bypass,
        }));
    } else if response.clicked() {
        ctx.send(Action::Plugin(PluginAction::OpenWindow(row.device)));
    }
    if late.is_some() {
        crate::views::late::repaint_when_expired(ui.ctx());
    }
}
