//! Shared two-column input chooser, audio monitoring toggle and input meters (audio proposal).

use std::sync::{Arc, Mutex};

use egui::{
    Align, Area, Id, Order, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_audio::{
    HostKind, InputConfig, InputDeviceInfo, InputDeviceRole, JackCaptureChannel,
};
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
pub(crate) const MAX_ROWS: usize = 10;

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

pub(crate) struct NamedJackInput {
    pub input: HardwareInput,
    pub name: String,
    pub detail: Option<String>,
    pub available: bool,
}

fn named_jack_input(
    sources: &[JackCaptureChannel],
    input: HardwareInput,
) -> Option<NamedJackInput> {
    let first = sources.get(usize::from(input.first))?;
    let (name, detail, available) = if input.stereo {
        let second = sources.get(usize::from(input.first) + 1)?;
        let name = match (
            first.port.name.split_once(':'),
            second.port.name.split_once(':'),
        ) {
            (Some((owner, left)), Some((other, right))) if owner == other => {
                format!("{owner}:{left} + {right}")
            }
            _ => format!("{} + {}", first.port.name, second.port.name),
        };
        let detail = format!("{} + {}", first.port.label(), second.port.label());
        let detail = (detail != name).then_some(detail);
        (name, detail, first.connected && second.connected)
    } else {
        (
            first.port.name.clone(),
            first.port.alias.clone(),
            first.connected,
        )
    };
    Some(NamedJackInput {
        input,
        name,
        detail,
        available,
    })
}

pub(crate) fn jack_input_choices(sources: &[JackCaptureChannel]) -> [Vec<NamedJackInput>; 2] {
    let count = sources.len() as u16;
    let choices = |stereo, count, step| {
        (0..count)
            .step_by(step)
            .filter_map(|first| named_jack_input(sources, HardwareInput { first, stereo }))
            .collect()
    };
    [
        choices(false, count, 1),
        choices(true, count.saturating_sub(1), 2),
    ]
}

impl NamedJackInput {
    pub(crate) fn label(&self, t: Lang) -> String {
        if self.available {
            self.name.clone()
        } else {
            t.audio_input_unavailable(&self.name)
        }
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
    let jack = ctx
        .engine
        .jack_capture_sources()
        .zip(match track.audio.input {
            AudioInput::Hardware(input) => Some(input),
            _ => None,
        });
    let named = jack.and_then(|(sources, input)| named_jack_input(sources, input));
    let label = named.as_ref().map_or_else(
        || input_label(t, project, track.audio.input),
        |source| source.name.clone(),
    );
    let available = match track.audio.input {
        AudioInput::Hardware(input) => ctx.engine.hardware_input_available(input),
        _ => input_available(project, track, ctx.engine.audio_input_channels()),
    };
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
        let compact = if rect.width() < HEADER_INPUT_WIDTH && jack.is_none() {
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
    let details = named
        .as_ref()
        .and_then(|source| source.detail.as_ref())
        .map(|source| format!("{} · {source}", t.audio_input_tip2()));
    Tip::new(&text)
        .detail(details.as_deref().unwrap_or_else(|| t.audio_input_tip2()))
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
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), size::MENU_ROW),
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

fn input_columns(ui: &mut Ui, fit_names: bool, draw: impl FnOnce(&mut Ui, &mut Ui)) {
    if !fit_names {
        ui.columns_const(|[left, right]| draw(left, right));
        return;
    }
    let spacing = ui.spacing().item_spacing.x;
    let left_width = (WIDTH - spacing) / 2.0;
    let top = ui.cursor().min;
    let bottom = ui.max_rect().bottom();
    let layout = egui::Layout::top_down_justified(Align::LEFT);
    let mut left = ui.new_child(
        UiBuilder::new()
            .layout(layout)
            .max_rect(Rect::from_min_max(top, pos2(top.x + left_width, bottom))),
    );
    let mut right = ui.new_child(UiBuilder::new().layout(layout).max_rect(Rect::from_min_max(
        pos2(top.x + left_width + spacing, top.y),
        pos2(ui.max_rect().right(), bottom),
    )));
    draw(&mut left, &mut right);
    let height = left.min_size().y.max(right.min_size().y);
    ui.advance_cursor_after_rect(Rect::from_min_size(top, vec2(ui.available_width(), height)));
}

#[derive(Clone)]
enum ChooserSource {
    Buses,
    Jack,
    Device(InputConfig),
}

fn chooser(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    track: &Track,
    anchor: &Response,
    id: Id,
) {
    let t = ctx.lang;
    let jack_sources = ctx.engine.jack_capture_sources();
    let jack_choices = jack_sources.map(jack_input_choices);
    let devices = match jack_sources {
        Some([]) => Vec::new(),
        Some(sources) => vec![InputDeviceInfo {
            id: None,
            name: t.settings_jack_inputs().to_owned(),
            channels: sources.len() as u16,
            is_default: true,
            sample_rate: ctx.engine.sample_rate(),
        }],
        None => inventory(ui, ctx),
    };
    let width = jack_choices.as_ref().map_or(WIDTH, |groups| {
        let longest = groups
            .iter()
            .flatten()
            .map(|choice| {
                w::styled_galley(ui, &choice.label(t), &typography::UI)
                    .size()
                    .x
            })
            .fold(0.0_f32, f32::max);
        (WIDTH / 2.0 + longest + 4.0 * space::SPACE_2).clamp(
            WIDTH,
            (ui.ctx().content_rect().width() - 4.0 * space::SPACE_4).max(WIDTH),
        )
    });
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
        .data(|data| data.get_temp::<ChooserSource>(selected_key))
        .and_then(|source| match source {
            ChooserSource::Buses => Some(devices.len()),
            ChooserSource::Jack if jack_sources.is_some() && !devices.is_empty() => Some(0),
            ChooserSource::Device(config) if jack_sources.is_none() => {
                config.device_index(&devices)
            }
            _ => None,
        })
        .unwrap_or(initial);
    let mut new_source = None;
    let rows = if source == devices.len() {
        project.tracks.iter().filter(|track| track.is_bus()).count()
    } else if let Some(groups) = &jack_choices {
        groups
            .iter()
            .filter(|group| !group.is_empty())
            .map(|group| group.len() + 1)
            .sum()
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
                ui.set_width(width);
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
                            input_columns(ui, jack_choices.is_some(), |left, right| {
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
                                        new_source = Some(if jack_sources.is_some() {
                                            ChooserSource::Jack
                                        } else {
                                            ChooserSource::Device(InputConfig {
                                                device: Some(device.name.clone()),
                                                device_id: device.id.clone(),
                                            })
                                        });
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
                                    new_source = Some(ChooserSource::Buses);
                                }
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
                                } else if let Some(groups) = &jack_choices {
                                    for (heading, choices) in [t.audio_input_section_mono(), t.audio_input_section_stereo()]
                                        .into_iter().zip(groups).filter(|(_, choices)| !choices.is_empty())
                                    {
                                        let group = UiBuilder::new()
                                            .accessibility_role(Role::Group)
                                            .accessibility_label(heading);
                                        right.scope_builder(group, |ui| {
                                            ui.label(fonts::rich_text(heading, &typography::LABEL));
                                            for choice in choices {
                                                let value = AudioInput::Hardware(choice.input);
                                                let available = ctx.engine.hardware_input_available(choice.input);
                                                let label = if available { choice.name.clone() } else { t.audio_input_unavailable(&choice.name) };
                                                let row = menu_row(ui, &label, track.audio.input == value, available);
                                                let mut tip = Tip::new(&label);
                                                if let Some(detail) = &choice.detail {
                                                    tip = tip.detail(detail);
                                                }
                                                tip.attach(&row);
                                                if row.clicked() {
                                                    ctx.send(Action::Edit(EditAction::SetTrackProp {
                                                        tracks: vec![track.id],
                                                        prop: komponist_model::edits::TrackProp::AudioInput(value),
                                                    }));
                                                    floating::close(ui.ctx(), id);
                                                }
                                            }
                                        });
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
    if let Some(source) = new_source {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use komponist_audio::{JackDirection, JackPort};
    use komponist_model::{TrackId, edits};

    const LEFT: &str = "Hotone USB Audio:capture_FL";
    const RIGHT: &str = "Hotone USB Audio:capture_FR";
    const MISSING: &str = "Backup Interface:capture_mic";
    const EXTRA: &str = "Room FX:Stereo return from effects rack channel 2";

    fn source(name: &str, alias: Option<&str>, connected: bool) -> JackCaptureChannel {
        JackCaptureChannel {
            port: JackPort {
                name: name.into(),
                alias: alias.map(str::to_owned),
                direction: JackDirection::Capture,
            },
            connected,
        }
    }

    #[test]
    fn jack_mono_and_stereo_choices_keep_route_order_and_distinguish_duplicate_aliases() {
        let [mono, stereo] = jack_input_choices(&[
            source(RIGHT, Some("alsa:pcm:hotone:capture_2"), true),
            source(LEFT, Some("alsa:pcm:hotone:capture_1"), true),
            source(MISSING, Some("Mic"), false),
            source(EXTRA, Some("Mic"), true),
            source("Other:dry", None, true),
        ]);
        assert_eq!(
            mono.iter()
                .map(|choice| (choice.input.first, choice.available))
                .collect::<Vec<_>>(),
            [(0, true), (1, true), (2, false), (3, true), (4, true)]
        );
        assert_ne!(mono[2].name, mono[3].name);
        assert_eq!(stereo.len(), 2);
        assert_eq!(
            stereo[0].input,
            HardwareInput {
                first: 0,
                stereo: true
            }
        );
        assert_eq!(stereo[0].name, "Hotone USB Audio:capture_FR + capture_FL");
        assert_eq!(
            stereo[1].input,
            HardwareInput {
                first: 2,
                stereo: true
            }
        );
        assert!(!stereo[1].available);
        assert!(stereo[1].name.contains(MISSING) && stereo[1].name.contains(EXTRA));
    }

    fn fixture(lang: Lang) -> (App, TrackId) {
        let (project, track) = edits::add_audio_track(
            &Project::new("JACK capture"),
            edits::NewAudioTrack {
                name: Some("Voice".into()),
                input: AudioInput::Hardware(HardwareInput {
                    first: 0,
                    stereo: false,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let (project, _) = edits::add_bus(
            &project,
            edits::NewBus {
                name: Some("Backing".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let mut app = App::headless(project).with_lang(lang);
        let _source = app.engine_mut().set_test_audio_input(4, 48_000).unwrap();
        app.engine_mut().set_test_jack_capture_sources(vec![
            source(LEFT, Some("alsa:pcm:hotone:capture_1"), true),
            source(RIGHT, Some("alsa:pcm:hotone:capture_2"), true),
            source(MISSING, None, false),
            source(EXTRA, None, true),
        ]);
        (app, track)
    }

    #[test]
    fn jack_choices_set_track_inputs_from_both_uis_without_changing_audio_settings() {
        for lang in [Lang::De, Lang::En] {
            let (app, track) = fixture(lang);
            let mut h = komponist_ui::snapshot::builder::<App>(1.0)
                .with_size(vec2(1280.0, 800.0))
                .with_max_steps(64)
                .build_ui_state(|ui, app: &mut App| app.show(ui), app);
            h.run_steps(3);

            let pair = "Hotone USB Audio:capture_FL + capture_FR";
            let missing = lang.audio_input_unavailable(MISSING);
            h.get_by_role_and_label(Role::Button, &lang.audio_input_tip(LEFT))
                .click();
            h.run_steps(3);
            assert!(
                h.get_by_role_and_label(Role::Group, lang.audio_input_section_mono())
                    .get_by_role_and_label(Role::MenuItem, &missing)
                    .accesskit_node()
                    .is_disabled()
            );
            let choice = h
                .get_by_role_and_label(Role::Group, lang.audio_input_section_stereo())
                .get_by_role_and_label(Role::MenuItem, pair);
            choice.click();
            h.run_steps(3);
            assert_eq!(
                h.state().project().track(track).unwrap().audio.input,
                AudioInput::Hardware(HardwareInput {
                    first: 0,
                    stereo: true
                })
            );
            assert!(
                h.get_by_role_and_label(Role::Button, &lang.audio_input_tip(pair))
                    .accesskit_node()
                    .description()
                    .as_deref()
                    .is_some_and(|detail| detail.contains("alsa:pcm:hotone:capture_1"))
            );
            assert!(h.state().engine.audio_input_config().is_none());
            assert_eq!(h.state().engine.audio_input_channels(), 4);

            h.key_press_modifiers(
                egui::Modifiers {
                    command: true,
                    shift: true,
                    ..Default::default()
                },
                egui::Key::T,
            );
            h.run_steps(3);
            let input = h.get_by_role_and_label(Role::ComboBox, lang.new_track_input());
            assert_eq!(input.accesskit_node().value().as_deref(), Some(EXTRA));
            input.click();
            h.run_steps(3);
            h.get_by_role_and_label(Role::Heading, lang.audio_input_section_mono());
            h.get_by_role_and_label(Role::Heading, lang.audio_input_section_stereo());
            assert!(
                h.get_by_role_and_label(Role::ListBoxOption, &missing)
                    .accesskit_node()
                    .is_disabled()
            );
            let sources = h.state().engine.jack_capture_sources().unwrap().to_vec();
            let mut refreshed = sources.clone();
            refreshed.swap(0, 1);
            let tracks_before = h.state().project().tracks.len();
            h.key_press(egui::Key::ArrowUp);
            h.run_steps(2);
            h.state_mut()
                .engine_mut()
                .set_test_jack_capture_sources(refreshed);
            h.key_press(egui::Key::Enter);
            h.run_steps(3);
            assert!(h.query_by_role(Role::ListBox).is_none());
            assert_eq!(h.state().project().tracks.len(), tracks_before);
            assert_eq!(
                h.get_by_role_and_label(Role::ComboBox, lang.new_track_input())
                    .accesskit_node()
                    .value()
                    .as_deref(),
                Some(EXTRA)
            );

            h.state_mut()
                .engine_mut()
                .set_test_jack_capture_sources(sources);
            h.get_by_role_and_label(Role::ComboBox, lang.new_track_input())
                .click();
            h.run_steps(3);
            h.get_by_role_and_label(Role::ListBoxOption, RIGHT).click();
            h.run_steps(3);
            h.get_by_role_and_label(Role::Button, lang.new_track_create())
                .click();
            h.run_steps(3);
            let added = h
                .state()
                .project()
                .tracks
                .iter()
                .find(|item| item.id != track && item.is_audio())
                .unwrap();
            assert_eq!(
                added.audio.input,
                AudioInput::Hardware(HardwareInput {
                    first: 1,
                    stereo: false
                })
            );
            assert!(h.state().engine.audio_input_config().is_none());
        }
    }
}
