//! Shared two-column input chooser, audio monitoring toggle and input meters (audio proposal).

use std::sync::{Arc, Mutex};

use egui::{
    Align, Area, Id, Order, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_audio::{HostKind, InputConfig, InputDeviceInfo, InputDeviceRole};
use komponist_i18n::Lang;
use komponist_model::{AudioInput, HardwareInput, MonitorMode, Project, Track};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, floating};
use komponist_ui::{fonts, icons, px};

use crate::actions::{Action, EditAction, SettingsAction, UiAction};
use crate::views::ViewCtx;

/// Approved D9B input chooser width, with equal source and channel columns.
pub const WIDTH: f32 = 380.0;
/// Header input meter in mockup 06, and the parallel thin input meter in mockup 06's strip.
pub const INPUT_BAR: f32 = 3.0;
/// Width of the header's input bar (mockup 06: x 12 through 106).
pub const HEADER_INPUT_WIDTH: f32 = 94.0;
/// Small A marker in the monitoring button (audio proposal §5).
pub const AUTO_MARK: f32 = 8.0;
/// Header input line disappears below this row height (audio proposal §5).
pub const INPUT_LINE_MIN: f32 = 44.0;
/// Keep D9 within the viewport while allowing longer channel/device inventories to scroll.
const MAX_ROWS: usize = 10;

pub fn input_label(t: Lang, project: &Project, input: AudioInput) -> String {
    match input {
        AudioInput::None => t.audio_input_none().to_owned(),
        AudioInput::Hardware(h) if h.stereo => {
            t.audio_input_pair(u32::from(h.first) + 1, u32::from(h.first) + 2)
        }
        AudioInput::Hardware(h) => t.audio_input_mono(u32::from(h.first) + 1),
        AudioInput::Bus(id) => t.audio_input_bus(
            project
                .track(id)
                .map_or(t.audio_input_missing_bus(), |t| t.name.as_str()),
        ),
    }
}

pub(crate) fn hardware_input_available(input: HardwareInput, channels: u16) -> bool {
    input
        .first
        .checked_add(input.channel_count())
        .is_some_and(|end| end <= channels)
}

pub fn input_available(project: &Project, track: &Track, channels: u16) -> bool {
    match track.audio.input {
        AudioInput::None => true,
        AudioInput::Hardware(input) => hardware_input_available(input, channels),
        AudioInput::Bus(bus) => {
            komponist_model::routing::input_sources(project, track.id).contains(&bus)
        }
    }
}

pub fn monitoring(track: &Track) -> bool {
    track.audio.monitor == MonitorMode::On
        || (track.audio.monitor == MonitorMode::Auto && track.arm)
}

#[derive(Default)]
struct Inventory {
    host: Option<HostKind>,
    devices: Vec<InputDeviceInfo>,
    pending: bool,
}

fn refresh_inventory(ui: &Ui) {
    if let Some(cache) = ui
        .ctx()
        .data(|data| data.get_temp::<Arc<Mutex<Inventory>>>(Id::unique("audio_input_inventory")))
    {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host = None;
    }
}

fn inventory(ui: &Ui, ctx: &ViewCtx<'_>) -> Vec<InputDeviceInfo> {
    let key = Id::unique("audio_input_inventory");
    let state = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<Inventory>>>(key)
            .clone()
    });
    let host = match ctx.engine.audio_state() {
        komponist_audio::AudioState::Running { config, .. }
        | komponist_audio::AudioState::NoDevice { config, .. } => config.host,
        komponist_audio::AudioState::Stopped => HostKind::Null,
    };
    let mut cache = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.host != Some(host)
        && !cache.pending
        && let Some(provider) = ctx.audio_devices.as_ref()
    {
        cache.pending = true;
        let provider = Arc::clone(provider);
        let state = Arc::clone(&state);
        let repaint = ui.ctx().clone();
        std::thread::spawn(move || {
            let devices = provider.input_devices(host);
            let mut cache = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.devices = devices;
            cache.host = Some(host);
            cache.pending = false;
            repaint.request_repaint();
        });
    }
    cache.devices.clone()
}

/// An accessible icon/text input line; all three views share the same chooser behavior.
pub fn input_button(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    track: &Track,
    rect: Rect,
) -> Response {
    let t = ctx.lang;
    if rect.height() >= size::CONTROL {
        px::fill(ui.painter(), rect, radius::S, color32(color::SURFACE_3));
    }
    let label = input_label(t, project, track.audio.input);
    let available = input_available(project, track, ctx.engine.audio_input_channels());
    let text = if available {
        label.clone()
    } else {
        t.audio_input_unavailable(&label)
    };
    let enabled = ctx.audio_recording.is_none();
    let response = ui.interact(
        rect,
        ui.scope_id().with(("audio_input", track.id.0)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let name = t.audio_input_tip(&text);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &name));
    w::accesskit(&response, |node| {
        node.set_has_popup(egui::accesskit::HasPopup::Menu)
    });
    let ink = if !available {
        color::STATE_ERROR
    } else if track.arm {
        color::ACCENT
    } else if track.audio.input == AudioInput::None {
        color::TEXT_4
    } else {
        color::TEXT_2
    };
    let icon = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - size::ICON / 2.0),
        vec2(size::ICON, size::ICON),
    );
    icons::paint_in(
        ui,
        icon,
        if available {
            Icon::AudioWaveform
        } else {
            Icon::TriangleExclamationPoint
        },
        color32(ink),
    );
    if rect.width() > 2.0 * size::ICON {
        let x = icon.right() + space::SPACE_1;
        let compact = if rect.width() < HEADER_INPUT_WIDTH {
            match track.audio.input {
                AudioInput::Hardware(h) if h.stereo => {
                    format!("{}+{}", u32::from(h.first) + 1, u32::from(h.first) + 2)
                }
                AudioInput::Hardware(h) => (u32::from(h.first) + 1).to_string(),
                AudioInput::Bus(id) => project
                    .track(id)
                    .map_or_else(|| text.clone(), |bus| bus.name.clone()),
                AudioInput::None => text.clone(),
            }
        } else {
            text.clone()
        };
        let galley = super::elided_galley(
            ui,
            &compact,
            fonts::font_id(&typography::SMALL),
            typography::SMALL.line_height,
            rect.right() - x,
        );
        ui.painter().galley(
            w::text_pos(rect, &galley, x, ui.pixels_per_point()),
            galley,
            color32(ink),
        );
    }
    Tip::new(&text)
        .detail(t.audio_input_tip2())
        .attach(&response);
    let popup = response.id.with("chooser");
    if !enabled {
        floating::close(ui.ctx(), popup);
    }
    if response.clicked() {
        if floating::is_open(ui.ctx(), popup) {
            floating::close(ui.ctx(), popup)
        } else {
            ui.ctx()
                .data_mut(|data| data.remove::<usize>(popup.with("source")));
            refresh_inventory(ui);
            floating::open(ui.ctx(), popup)
        }
    }
    if floating::is_open(ui.ctx(), popup) {
        chooser(ui, ctx, project, track, &response, popup);
    }
    response
}

fn menu_row(ui: &mut Ui, label: &str, selected: bool, enabled: bool) -> Response {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), size::MENU_ROW), Sense::hover());
    let response = ui.interact(
        rect,
        ui.next_auto_id(),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if selected || response.hovered() {
        px::fill(
            ui.painter(),
            rect,
            radius::S,
            color32(if selected {
                color::ACCENT_SURFACE
            } else {
                color::SURFACE_4
            }),
        );
    }
    let g = w::styled_galley(ui, label, &typography::UI);
    let painter = ui.painter().with_clip_rect(rect);
    painter.galley(
        w::text_pos(
            rect,
            &g,
            rect.left() + space::SPACE_2,
            ui.pixels_per_point(),
        ),
        g,
        color32(if enabled {
            color::TEXT_1
        } else {
            color::TEXT_4
        }),
    );
    response.widget_info(|| WidgetInfo::selected(Role::MenuItem, enabled, selected, label));
    response
}

/// The same localized capture device label used in Settings and the track chooser.
pub(crate) fn input_device_label(t: Lang, device: &InputDeviceInfo) -> String {
    match device.role() {
        Some(InputDeviceRole::Input) => t.audio_input_device_source(&device.name),
        Some(InputDeviceRole::OutputMonitor) => t.audio_input_device_monitor(&device.name),
        None => device.name.clone(),
    }
}

fn chooser(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    track: &Track,
    anchor: &Response,
    id: Id,
) {
    let devices = inventory(ui, ctx);
    let t = ctx.lang;
    let selected_key = id.with("source");
    let configured_device = ctx
        .engine
        .audio_input_config()
        .and_then(|config| config.device_index(&devices));
    let initial = if matches!(track.audio.input, AudioInput::Bus(_)) {
        devices.len()
    } else {
        configured_device
            .or_else(|| InputConfig::default().device_index(&devices))
            .unwrap_or(0)
    };
    let mut source = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(selected_key))
        .unwrap_or(initial)
        .min(devices.len());
    let mut source_changed = false;
    let rows = if source == devices.len() {
        project.tracks.iter().filter(|track| track.is_bus()).count()
    } else {
        devices.get(source).map_or(0, |device| {
            usize::from(device.channels) + usize::from(device.channels / 2)
        })
    };
    let height = (rows.max(devices.len() + 2).min(MAX_ROWS) as f32)
        * (size::MENU_ROW + ui.spacing().item_spacing.y);
    let title = t.audio_input_title(&track.name);
    let shown = Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(anchor.rect.left_bottom())
        .show(ui.ctx(), |ui| {
            w::menu_frame().show(ui, |ui| {
                ui.set_width(WIDTH);
                let builder = UiBuilder::new()
                    .accessibility_role(Role::Menu)
                    .accessibility_label(&title);
                ui.scope_builder(builder, |ui| {
                    ui.label(fonts::rich_text(&title, &typography::UI));
                    if menu_row(
                        ui,
                        t.audio_input_clear(),
                        track.audio.input == AudioInput::None,
                        true,
                    )
                    .clicked()
                    {
                        ctx.send(Action::Edit(EditAction::SetTrackProp {
                            tracks: vec![track.id],
                            prop: komponist_model::edits::TrackProp::AudioInput(AudioInput::None),
                        }));
                        floating::close(ui.ctx(), id);
                    }
                    egui::ScrollArea::vertical()
                        .max_height(height)
                        .min_scrolled_height(height)
                        .show(ui, |ui| {
                            ui.columns(2, |columns| {
                                let left = &mut columns[0];
                                left.label(fonts::rich_text(
                                    t.audio_input_sources(),
                                    &typography::LABEL,
                                ));
                                for (i, device) in devices.iter().enumerate() {
                                    let label = input_device_label(t, device);
                                    let row = menu_row(left, &label, source == i, true);
                                    w::accesskit(&row, |node| node.set_selected(source == i));
                                    Tip::new(&label).attach(&row);
                                    if row.clicked() {
                                        source = i;
                                        source_changed = true;
                                    }
                                }
                                if menu_row(
                                    left,
                                    t.audio_input_section_bus(),
                                    source == devices.len(),
                                    true,
                                )
                                .clicked()
                                {
                                    source = devices.len();
                                    source_changed = true;
                                }
                                let right = &mut columns[1];
                                if source == devices.len() {
                                    let allowed =
                                        komponist_model::routing::input_sources(project, track.id);
                                    for bus in project.tracks.iter().filter(|track| track.is_bus())
                                    {
                                        let valid = allowed.contains(&bus.id);
                                        let label = if valid {
                                            bus.name.clone()
                                        } else {
                                            format!("{} · {}", bus.name, t.audio_input_loop())
                                        };
                                        if menu_row(
                                            right,
                                            &label,
                                            track.audio.input == AudioInput::Bus(bus.id),
                                            valid,
                                        )
                                        .clicked()
                                        {
                                            ctx.send(Action::Edit(EditAction::SetTrackProp {
                                                tracks: vec![track.id],
                                                prop: komponist_model::edits::TrackProp::AudioInput(
                                                    AudioInput::Bus(bus.id),
                                                ),
                                            }));
                                            floating::close(right.ctx(), id);
                                        }
                                    }
                                } else if let Some(device) = devices.get(source) {
                                    let config = InputConfig {
                                        device: Some(device.name.clone()),
                                        device_id: device.id.clone(),
                                    };
                                    let current = configured_device == Some(source);
                                    let channels = (0..device.channels)
                                        .map(|first| HardwareInput {
                                            first,
                                            stereo: false,
                                        })
                                        .chain(
                                            (0..device.channels.saturating_sub(1)).step_by(2).map(
                                                |first| HardwareInput {
                                                    first,
                                                    stereo: true,
                                                },
                                            ),
                                        );
                                    for input in channels {
                                        let value = AudioInput::Hardware(input);
                                        let label = format!(
                                            "{} · {}",
                                            input_label(t, project, value),
                                            if input.stereo {
                                                t.audio_channel_stereo()
                                            } else {
                                                t.audio_channel_mono()
                                            }
                                        );
                                        if menu_row(
                                            right,
                                            &label,
                                            current && track.audio.input == value,
                                            true,
                                        )
                                        .clicked()
                                        {
                                            ctx.send(Action::Settings(
                                                SettingsAction::ChooseTrackAudioInput {
                                                    track: track.id,
                                                    input: value,
                                                    device: config.clone(),
                                                },
                                            ));
                                            floating::close(right.ctx(), id);
                                        }
                                    }
                                }
                            });
                        });
                    if devices.is_empty() {
                        ui.label(fonts::rich_text(
                            t.audio_input_no_device(),
                            &typography::SMALL,
                        ));
                        if menu_row(ui, t.audio_input_open_settings(), false, true).clicked() {
                            ctx.send(Action::Ui(UiAction::OpenSettings(
                                crate::ui_state::SettingsSection::Audio,
                            )));
                            floating::close(ui.ctx(), id);
                        }
                    }
                });
            });
        });
    if source_changed {
        ui.ctx()
            .data_mut(|data| data.insert_temp(selected_key, source));
    }
    if w::pressed_outside(ui.ctx(), &[anchor.rect, shown.response.rect])
        || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        floating::close(ui.ctx(), id);
    }
}

pub fn monitor_button(ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: &Track, rect: Rect) {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("audio_monitor", track.id.0))
            .layout(egui::Layout::left_to_right(Align::Min)),
    );
    let mode = track.audio.monitor;
    let response = TrackToggle::new(ToggleKind::Monitor, monitoring(track))
        .track(&track.name)
        .size(rect.size())
        .show(&mut child);
    let t = ctx.lang;
    let value = match mode {
        MonitorMode::Off => t.ui_monitor_off(),
        MonitorMode::Auto => t.ui_monitor_auto(),
        MonitorMode::On => t.ui_monitor_on(),
    };
    w::accesskit(&response.response, |node| {
        node.set_value(value);
        if mode == MonitorMode::Auto && track.arm {
            node.set_description(t.audio_monitor_auto_armed());
        }
    });
    if mode == MonitorMode::Auto {
        px::inner_border(ui.painter(), rect, color32(color::STATE_PLAY));
        let mut style = typography::VALUE_S;
        style.size = AUTO_MARK;
        style.line_height = AUTO_MARK;
        let g = w::styled_galley(ui, t.ui_monitor_auto_mark(), &style);
        ui.painter().galley(
            pos2(rect.right() - g.size().x, rect.bottom() - g.size().y),
            g,
            color32(color::STATE_PLAY),
        );
    }
    let send = |mode| {
        Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track.id],
            prop: komponist_model::edits::TrackProp::MonitorMode(mode),
        })
    };
    if matches!(response.event, Some(ToggleEvent::Set { .. })) {
        ctx.send(send(if mode == MonitorMode::Off {
            MonitorMode::On
        } else {
            MonitorMode::Off
        }));
    }
    let items = [
        Entry::action(t.ui_monitor_menu_off(), MonitorMode::Off).checked(mode == MonitorMode::Off),
        Entry::action(t.ui_monitor_menu_auto(), MonitorMode::Auto)
            .checked(mode == MonitorMode::Auto),
        Entry::action(t.ui_monitor_menu_on(), MonitorMode::On).checked(mode == MonitorMode::On),
    ];
    if let Some(mode) = ContextMenu::new(t.audio_monitoring(), &items).show_on(&response.response) {
        ctx.send(send(mode));
    }
    let detail = if mode == MonitorMode::Auto && track.arm {
        format!("{} · {}", t.audio_monitor_auto_armed(), t.ui_monitor_tip2())
    } else {
        t.ui_monitor_tip2().to_owned()
    };
    let tip = t.audio_monitor_tip(value);
    Tip::new(&tip).detail(&detail).attach(&response.response);
}

pub fn input_meter(ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: &Track, rect: Rect) {
    let state = ctx.meters.input(track.id);
    let level = state.map_or(0.0, |state| {
        komponist_ui::widgets::meter::fraction(state.level_db(0).max(state.level_db(1)))
    });
    let clipped = state.is_some_and(|state| state.clipped());
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_0));
    let horizontal = rect.width() > rect.height();
    let fill = if horizontal {
        Rect::from_min_size(rect.min, vec2(rect.width() * level, rect.height()))
    } else {
        Rect::from_min_max(
            pos2(rect.left(), rect.bottom() - rect.height() * level),
            rect.max,
        )
    };
    px::fill(
        ui.painter(),
        fill,
        0.0,
        color32(if clipped {
            color::STATE_ERROR
        } else {
            color::STATE_PLAY
        }),
    );
    let response = ui.interact(
        rect,
        ui.scope_id().with(("audio_input_meter", track.id.0)),
        Sense::click(),
    );
    let name = ctx.lang.audio_input_meter(&track.name);
    response.widget_info(|| WidgetInfo::labeled(Role::Meter, true, &name));
    w::accesskit(&response, |node| {
        node.set_numeric_value(f64::from(level));
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(1.0);
    });
    if response.clicked() {
        ctx.send(Action::Ui(UiAction::ResetMeters {
            track: Some(track.id),
            all: ui.input(|input| input.modifiers.alt),
        }));
    }
}

/// Wide labeled action rather than the circular arm toggle (owner decision D10B).
pub fn record_bus_button(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    track: &Track,
    rect: Rect,
    compact: bool,
) {
    let t = ctx.lang;
    let label = if compact {
        t.audio_bus_record_short()
    } else {
        t.routing_record_bus()
    };
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("record_bus", track.id.0)),
    );
    let response = child.add(w::button::Button::new(label).small().ground(1));
    let name = t.routing_record_bus_name(&track.name);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    Tip::new(t.routing_record_bus())
        .detail(t.routing_record_bus_tip())
        .attach(&response);
    if response.clicked() {
        ctx.send(Action::Edit(EditAction::RecordBus(track.id)));
    }
}
