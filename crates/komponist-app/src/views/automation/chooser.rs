//! Discovers targets from the owning channel, and presents them in the shared parameter chooser.

use std::collections::HashMap;

use egui::{
    Color32, Id, Key, Modifiers, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_i18n::Lang;
use komponist_model::automation::{AutomationMode, AutomationOwner, AutomationTarget};
use komponist_model::macros::TrackMacro;
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};
use komponist_model::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};
use komponist_model::{Channel, Device, Project, TrackSend};
use komponist_plugins::DeviceStatus;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{focus, icons, px, widgets};

use super::{lanes, metrics as m};
use crate::device_host::LegacyParameters;
use crate::engine_sync::EngineSync;
use crate::ui_state::ParamTouch;
use crate::views::{ViewCtx, catalog_plugin_name, elided_galley};

#[derive(Clone)]
pub(crate) struct Target {
    pub key: AutomationTarget,
    pub label: String,
    pub info: ParamInfo,
    pub base: f32,
}

impl Target {
    fn new(key: AutomationTarget, label: String, info: ParamInfo, plain: f64) -> Target {
        Target {
            key,
            label,
            base: info.to_normalized(plain) as f32,
            info,
        }
    }

    pub fn format(&self, value: f32) -> String {
        self.info.format(self.info.to_plain(f64::from(value)))
    }

    pub fn parse(&self, text: &str) -> Option<f32> {
        let text = text.trim();
        let plain = match self.info.unit {
            Unit::Db | Unit::DbWidth => f64::from(komponist_model::format::parse_db(text)?),
            Unit::Percent => widgets::number_field::parse_number(text, "%")? / 100.0,
            Unit::Hz if text.ends_with("kHz") => {
                widgets::number_field::parse_number(text, "kHz")? * 1000.0
            }
            Unit::Hz => widgets::number_field::parse_number(text, "Hz")?,
            Unit::Ms if text.ends_with('s') && !text.ends_with("ms") => {
                widgets::number_field::parse_number(text, "s")? * 1000.0
            }
            Unit::Ms => widgets::number_field::parse_number(text, "ms")?,
            Unit::S if text.ends_with("ms") => {
                widgets::number_field::parse_number(text, "ms")? / 1000.0
            }
            Unit::S => widgets::number_field::parse_number(text, "s")?,
            Unit::Semitones => widgets::number_field::parse_number(text, "st")?,
            Unit::Cents => widgets::number_field::parse_number(text, "ct")?,
            Unit::Octaves => widgets::number_field::parse_number(text, "oct")?,
            Unit::Ratio => widgets::number_field::parse_number(text, ":1")?,
            Unit::Meters => widgets::number_field::parse_number(text, "m")?,
            Unit::None => match &self.info.kind {
                ParamKind::Float {
                    scale: Scale::Pan, ..
                } => f64::from(komponist_model::format::parse_pan(text)?),
                ParamKind::Enum(choices) => choices
                    .iter()
                    .position(|choice| choice.eq_ignore_ascii_case(text))
                    .map(|i| i as f64)
                    .or_else(|| widgets::number_field::parse_number(text, ""))?,
                ParamKind::Bool if self.info.format(0.0).eq_ignore_ascii_case(text) => 0.0,
                ParamKind::Bool if self.info.format(1.0).eq_ignore_ascii_case(text) => 1.0,
                _ => widgets::number_field::parse_number(text, "")?,
            },
        };
        Some(self.info.to_normalized(self.info.snap(plain)) as f32)
    }
}

pub(crate) fn percentage(lang: Lang, value: f32) -> String {
    ParamInfo::new("automation", String::new(), ParamKind::lin(0.0, 1.0), 0.0)
        .unit(Unit::Percent)
        .texts(crate::views::parameter_value_texts(lang))
        .format(f64::from(value))
}

pub(crate) fn macro_label(lang: Lang, index: u8, slot: &TrackMacro) -> String {
    let number = u16::from(index) + 1;
    if slot.name.is_empty() {
        lang.macros_slot(number)
    } else {
        lang.macros_slot_named(number, &slot.name)
    }
}

type Section = (String, Vec<Target>);

pub(crate) struct Catalog<'a> {
    project: &'a Project,
    engine: &'a EngineSync,
    lang: Lang,
    owner: AutomationOwner,
    channel: &'a Channel,
    sends: &'a [TrackSend],
    recent: &'a [ParamTouch],
}

impl<'a> Catalog<'a> {
    pub fn new(ctx: &'a ViewCtx<'_>, owner: AutomationOwner) -> Option<Self> {
        let mut catalog = Self::for_project(ctx.project, ctx.engine, ctx.lang, owner)?;
        catalog.recent = &ctx.ui.devices.automation_recent;
        Some(catalog)
    }

    pub(crate) fn for_project(
        project: &'a Project,
        engine: &'a EngineSync,
        lang: Lang,
        owner: AutomationOwner,
    ) -> Option<Self> {
        let (channel, sends) = match owner {
            AutomationOwner::Track(id) => {
                let track = project.track(id)?;
                (&track.channel, track.sends.as_slice())
            }
            AutomationOwner::Master => (project.master.as_ref(), &[][..]),
        };
        Some(Self {
            project,
            engine,
            lang,
            owner,
            channel,
            sends,
            recent: &[],
        })
    }

    fn gain(&self, key: AutomationTarget, name: String, db: f32) -> Target {
        let info = ParamInfo::new(
            "volume",
            name.clone(),
            ParamKind::scaled(
                f64::from(VOLUME_DB_MIN),
                f64::from(VOLUME_DB_MAX),
                Scale::Db,
            ),
            0.0,
        )
        .unit(Unit::Db);
        Target::new(key, name, info, f64::from(db))
    }

    fn send(&self, send: &TrackSend) -> Target {
        let bus = self
            .project
            .track(send.target)
            .map_or("", |track| track.name.as_str());
        self.gain(
            AutomationTarget::Send { send: send.id },
            self.lang.automation_send(bus),
            send.level_db,
        )
    }

    fn pan(&self) -> Target {
        let name = self.lang.param_pan().to_owned();
        let info = ParamInfo::new(
            "pan",
            name.clone(),
            ParamKind::scaled(-1.0, 1.0, Scale::Pan),
            0.0,
        );
        Target::new(
            AutomationTarget::Pan,
            name,
            info,
            f64::from(self.channel.pan),
        )
    }

    fn volume(&self) -> Target {
        self.gain(
            AutomationTarget::Volume,
            self.lang.param_volume().to_owned(),
            self.channel.volume_db,
        )
    }

    fn hosted_info(&self, info: &komponist_ipc::ParamInfo) -> ParamInfo {
        komponist_plugins::params::param_info(info)
            .texts(crate::views::parameter_value_texts(self.lang))
    }

    fn device_targets(&self, device: &Device, name: &str) -> Vec<Target> {
        if let Some(parameters) = LegacyParameters::new(device) {
            return parameters
                .all(self.lang)
                .into_iter()
                .filter(|p| p.info.automatable)
                .map(|p| self.device_target(device, p.info, p.value, name))
                .collect();
        }
        let host = self.engine.host();
        host.params(device.id)
            .unwrap_or_default()
            .iter()
            .filter(|p| komponist_plugins::params::is_automatable(p))
            .map(|p| {
                let info = self.hosted_info(p);
                let value = host.param_base(device.id, p.id).unwrap_or(p.default);
                self.device_target(device, info, value, name)
            })
            .collect()
    }

    /// Resolve just the visible lane's parameter. The full catalog is built when the chooser opens.
    fn device_param(&self, device: &Device, id: &ParamId) -> Option<(ParamInfo, f64)> {
        if let Some(parameters) = LegacyParameters::new(device) {
            let p = parameters.get(self.lang, id)?;
            return Some((p.info, p.value));
        }
        let wire = id.plugin_id()?;
        let host = self.engine.host();
        let param = host
            .params(device.id)?
            .iter()
            .find(|param| param.id == wire && komponist_plugins::params::is_listed(param))?;
        let info = self.hosted_info(param);
        let value = host.param_base(device.id, wire).unwrap_or(param.default);
        Some((info, value))
    }

    fn device_target(&self, device: &Device, info: ParamInfo, value: f64, prefix: &str) -> Target {
        let key = AutomationTarget::Param {
            device: device.id,
            param: info.id.clone(),
        };
        let label = format!("{prefix} · {}", info.name);
        Target::new(key, label, info, value)
    }

    pub fn resolve(&self, target: &AutomationTarget) -> Result<Target, Option<String>> {
        let lang = self.lang;
        match target {
            AutomationTarget::Volume => Ok(self.volume()),
            AutomationTarget::Pan => Ok(self.pan()),
            AutomationTarget::Send { send } => self
                .sends
                .iter()
                .find(|s| s.id == *send)
                .map(|s| self.send(s))
                .ok_or_else(|| Some(lang.automation_param_missing().to_owned())),
            AutomationTarget::Param { device, param } => {
                let device = self
                    .channel
                    .devices
                    .iter()
                    .find(|d| d.id == *device)
                    .ok_or_else(|| Some(lang.automation_not_on_track().to_owned()))?;
                let name = catalog_plugin_name(lang, self.engine.host().catalog(), &device.plugin);
                match self.engine.device_status(device.id) {
                    DeviceStatus::Missing | DeviceStatus::Crashed(_) => {
                        return Err(Some(lang.arrangement_plugin_missing(&name)));
                    }
                    DeviceStatus::Loading => return Err(None),
                    DeviceStatus::Ok => {}
                }
                if let Some((info, value)) = self.device_param(device, param) {
                    if info.automatable {
                        return Ok(self.device_target(device, info, value, &name));
                    }
                    return Err(Some(lang.automation_not_automatable().to_owned()));
                }
                let hidden = param.plugin_id().is_some_and(|id| {
                    self.engine
                        .host()
                        .params(device.id)
                        .is_some_and(|infos| infos.iter().any(|p| p.id == id))
                });
                Err(Some(
                    if hidden {
                        lang.automation_not_automatable()
                    } else {
                        lang.automation_param_missing()
                    }
                    .to_owned(),
                ))
            }
            AutomationTarget::Macro { index } => self
                .track()
                .and_then(|track| track.macros.0.get(usize::from(*index)))
                .map(|slot| self.track_macro(*index, slot))
                .ok_or_else(|| Some(lang.automation_param_missing().to_owned())),
        }
    }

    fn track(&self) -> Option<&komponist_model::Track> {
        let AutomationOwner::Track(id) = self.owner else {
            return None;
        };
        self.project.track(id).map(AsRef::as_ref)
    }

    fn track_macro(&self, index: u8, slot: &TrackMacro) -> Target {
        let label = macro_label(self.lang, index, slot);
        let info = ParamInfo::new("track_macro", label.clone(), ParamKind::lin(0.0, 1.0), 0.0)
            .unit(Unit::Percent)
            .texts(crate::views::parameter_value_texts(self.lang))
            .flags(true, false);
        Target::new(
            AutomationTarget::Macro { index },
            label,
            info,
            f64::from(slot.value),
        )
    }

    pub(crate) fn driven(&self, target: &AutomationTarget) -> Option<String> {
        let track = self.track()?;
        let index = track.macros.driver(target)?;
        let source = crate::views::track_macros::reference(
            self.lang,
            index,
            &track.macros.0[usize::from(index)],
        );
        Some(self.lang.macros_lane_overridden(source))
    }

    fn sections(&self, query: &str) -> Vec<Section> {
        let lang = self.lang;
        let mut mixer = vec![self.volume(), self.pan()];
        mixer.extend(self.sends.iter().map(|send| self.send(send)));
        let mut sections = vec![(lang.automation_mixer().to_owned(), mixer)];
        if let Some(track) = self.track() {
            let targets = track
                .macros
                .0
                .iter()
                .enumerate()
                .map(|(index, slot)| self.track_macro(index as u8, slot))
                .collect();
            sections.push((lang.macros_title().to_owned(), targets));
        }
        let host = self.engine.host();
        for device in self.channel.devices.iter() {
            let Some(entry) = host.catalog().resolve(&device.plugin) else {
                continue;
            };
            let name = entry.display_name(lang.code()).to_owned();
            let mut targets = self.device_targets(device, &name);
            if let Some(infos) = host.params(device.id) {
                let modules: HashMap<_, _> = infos
                    .iter()
                    .filter(|info| !info.module.is_empty())
                    .map(|info| (info.id, info))
                    .collect();
                for target in &mut targets {
                    if let Some(info) = target.info.id.plugin_id().and_then(|id| modules.get(&id)) {
                        target.label = format!("{name} · {} · {}", info.module, info.name);
                    }
                }
            }
            sections.push((name, targets));
        }
        let recent = self
            .recent
            .iter()
            .filter_map(|touch| {
                let device = self
                    .channel
                    .devices
                    .iter()
                    .find(|device| device.id == touch.device)?;
                host.catalog().resolve(&device.plugin)?;
                self.resolve(&AutomationTarget::Param {
                    device: touch.device,
                    param: touch.param.clone(),
                })
                .ok()
            })
            .take(m::RECENT_TARGETS)
            .collect::<Vec<_>>();
        if !recent.is_empty() {
            sections.insert(0, (lang.automation_recent().to_owned(), recent));
        }
        let query = query.to_lowercase();
        if !query.is_empty() {
            for (_, targets) in &mut sections {
                targets.retain(|target| target.label.to_lowercase().contains(&query));
            }
            sections.retain(|(_, targets)| !targets.is_empty());
        }
        sections
    }

    fn already_automated(&self, target: &AutomationTarget) -> bool {
        lanes(self.project, self.owner)
            .is_some_and(|a| a.lanes.iter().any(|lane| &lane.target == target))
    }
}

pub(crate) fn mode_names(lang: Lang) -> [(AutomationMode, &'static str); 4] {
    [
        (AutomationMode::Off, lang.automation_mode_off()),
        (AutomationMode::Read, lang.automation_mode_read()),
        (AutomationMode::Touch, lang.automation_mode_touch()),
        (AutomationMode::Latch, lang.automation_mode_latch()),
    ]
}

pub(crate) fn mode_chip_width(ui: &Ui, lang: Lang) -> f32 {
    let items = mode_names(lang).map(|(_, label)| Item::new(label));
    let mut selected = 0;
    Dropdown::new("", &mut selected, &items)
        .compact()
        .natural_width(ui)
}

pub(crate) fn mode_chip(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    lang: Lang,
    param: &str,
    mode: AutomationMode,
) -> Option<AutomationMode> {
    let names = mode_names(lang);
    let items = names.map(|(_, label)| Item::new(label));
    let mut selected = names.iter().position(|(m, _)| *m == mode).unwrap_or(1);
    let before = selected;
    let label = lang.automation_mode_lane(param);
    let mut child = ui.new_child(UiBuilder::new().id_salt(id).max_rect(rect));
    let dropdown = Dropdown::new(&label, &mut selected, &items)
        .compact()
        .width(rect.width())
        .id(id);
    let dropdown = match mode {
        AutomationMode::Off => dropdown.field_tint(None, color32(color::TEXT_3)),
        AutomationMode::Read => dropdown,
        AutomationMode::Touch | AutomationMode::Latch => dropdown.field_tint(
            Some(color32(color::STATE_RECORD_SURFACE)),
            color32(color::STATE_RECORD_INK),
        ),
    };
    child.add(dropdown);
    (selected != before).then_some(names[selected].0)
}

pub(crate) fn chooser_button(ui: &mut Ui, rect: Rect, id: Id, lang: Lang, label: &str) -> Response {
    let r = ui.interact(rect, id, Sense::click());
    r.widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, lang.a11y_automation_chooser()));
    px::fill(
        ui.painter(),
        rect,
        radius::S,
        color32(if r.hovered() {
            color::SURFACE_4
        } else {
            color::SURFACE_3
        }),
    );
    let mut text = rect.shrink2(vec2(space::SPACE_1, 0.0));
    text.max.x -= size::ICON;
    let style = typography::SMALL;
    let galley = elided_galley(
        ui,
        label,
        komponist_ui::fonts::font_id(&style),
        style.line_height,
        text.width().max(0.0),
    );
    let at = widgets::text_pos(text, &galley, text.left(), ui.pixels_per_point());
    ui.painter().galley(at, galley, color32(color::TEXT_1));
    let icon = Rect::from_center_size(
        pos2(rect.right() - size::ICON / 2.0, rect.center().y),
        vec2(space::SPACE_1_5, space::SPACE_1_5),
    );
    icons::paint_in(ui, icon, Icon::ChevronDown, color32(color::TEXT_2));
    if focus::ring_visible(&r) {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    Tip::new(label).attach(&r);
    r
}

pub(crate) fn toggle(
    ui: &mut Ui,
    right: f32,
    center_y: f32,
    id: Id,
    lang: Lang,
    shown: bool,
    tint: Color32,
) -> Response {
    let rect = Rect::from_min_size(
        pos2(right - m::TOGGLE_WIDTH, center_y - m::TOGGLE_HEIGHT / 2.0),
        vec2(m::TOGGLE_WIDTH, m::TOGGLE_HEIGHT),
    );
    let label = if shown {
        lang.automation_hide()
    } else {
        lang.automation_show()
    };
    let r = ui.interact(rect, id, Sense::click());
    r.widget_info(|| WidgetInfo::selected(Role::Button, true, shown, label));
    widgets::accesskit(&r, |node| {
        node.set_toggled(if shown {
            egui::accesskit::Toggled::True
        } else {
            egui::accesskit::Toggled::False
        })
    });
    let fill = if shown {
        Some(color::SURFACE_5)
    } else if r.hovered() {
        Some(color::SURFACE_4)
    } else {
        None
    };
    if let Some(fill) = fill {
        px::fill(ui.painter(), rect, radius::S, color32(fill));
    }
    icons::paint_in(
        ui,
        Rect::from_center_size(rect.center(), vec2(m::TOGGLE_ICON, m::TOGGLE_ICON)),
        Icon::Spline,
        tint,
    );
    if focus::ring_visible(&r) {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    Tip::new(label).attach(&r);
    r
}

/// One popover is active at a time per editor. The query and its highlighted entry belong to
/// the popup rather than to the document.
#[derive(Debug, Default)]
pub(crate) struct Chooser {
    active: Option<Id>,
    query: String,
    highlighted: Highlight,
}

/// Re-resolve the selected target against the latest rows, since plugin metadata can arrive
/// while the popup remains open. A target may be listed both under Recent and its own device.
#[derive(Debug, Default)]
enum Highlight {
    #[default]
    First,
    Target {
        key: AutomationTarget,
        occurrence: usize,
    },
    Add,
}

impl Highlight {
    fn update(
        &mut self,
        rows: &[&AutomationTarget],
        add: bool,
        up: bool,
        down: bool,
    ) -> (Option<usize>, bool) {
        let previous = match self {
            Self::First => Some(0),
            Self::Target { key, occurrence } => {
                let mut matches = rows.iter().enumerate().filter(|(_, row)| **row == &*key);
                let first = matches.next();
                if *occurrence == 0 {
                    first.map(|(index, _)| index)
                } else {
                    matches
                        .nth(*occurrence - 1)
                        .or(first)
                        .map(|(index, _)| index)
                }
            }
            Self::Add => add.then_some(rows.len()),
        };
        let count = rows.len() + usize::from(add);
        if count == 0 {
            *self = Self::First;
            return (None, previous.is_none());
        }
        let mut index = previous.unwrap_or(0);
        if up {
            index = index.saturating_sub(1);
        }
        if down {
            index = (index + 1).min(count - 1);
        }
        *self = if let Some(key) = rows.get(index) {
            let occurrence = rows[..index].iter().filter(|row| *row == key).count();
            Self::Target {
                key: (**key).clone(),
                occurrence,
            }
        } else {
            Self::Add
        };
        (Some(index), previous.is_none())
    }
}

impl Chooser {
    pub fn open(&mut self, id: Id) {
        if self.active != Some(id) {
            self.active = Some(id);
            self.query.clear();
            self.highlighted = Highlight::default();
        } else {
            self.active = None;
        }
    }

    pub fn show(
        &mut self,
        ui: &Ui,
        id: Id,
        anchor: Rect,
        catalog: &Catalog<'_>,
        add: bool,
    ) -> Option<Choice> {
        if self.active != Some(id) {
            return None;
        }
        let lang = catalog.lang;
        let content = ui.ctx().content_rect();
        let gap = space::SPACE_0_5;
        let below = content.bottom() - anchor.bottom();
        let above = anchor.top() - content.top();
        let down = below >= m::CHOOSER_HEIGHT || below >= above;
        let height = m::CHOOSER_HEIGHT.min(if down { below } else { above } - gap);
        let pos = if down {
            anchor.left_bottom() + vec2(0.0, gap)
        } else {
            anchor.left_top() - vec2(0.0, gap)
        };
        let popup_id = id.with("automation-chooser");
        let mut result = None;
        let mut cancel = false;
        let popup = egui::Area::new(popup_id)
            .kind(egui::UiKind::Popup)
            .accessibility_parent(id)
            .accessible_name(lang.automation_title())
            .role(Role::ListBox)
            .order(egui::Order::Foreground)
            .pivot(if down {
                egui::Align2::LEFT_TOP
            } else {
                egui::Align2::LEFT_BOTTOM
            })
            .fixed_pos(pos)
            .constrain(true)
            .show(ui.ctx(), |ui| {
                widgets::menu_frame().show(ui, |ui| {
                    ui.set_width(m::CHOOSER_WIDTH);
                    let search_id = popup_id.with("search");
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .id(search_id)
                            .hint_text(lang.automation_search())
                            .desired_width(m::CHOOSER_WIDTH),
                    );
                    search.widget_info(|| {
                        WidgetInfo::labeled(Role::TextInput, true, lang.automation_search())
                    });
                    if !ui.memory(|mem| mem.has_focus(search_id)) {
                        search.request_focus();
                    }
                    if search.changed() {
                        self.highlighted = Highlight::default();
                    }
                    let sections = catalog.sections(&self.query);
                    let up = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp));
                    let down = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown));
                    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    cancel = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    let (highlighted, previous_missing) = {
                        let rows = sections
                            .iter()
                            .flat_map(|(_, rows)| rows)
                            .filter(|target| catalog.driven(&target.key).is_none())
                            .map(|target| &target.key)
                            .collect::<Vec<_>>();
                        self.highlighted.update(&rows, add, up, down)
                    };
                    let enter = enter && !previous_missing;
                    egui::ScrollArea::vertical()
                        .max_height((height - search.rect.height()).max(0.0))
                        .show(ui, |ui| {
                            let mut index = 0;
                            let mut row = 0;
                            for (name, targets) in sections {
                                let label_style = typography::LABEL;
                                ui.label(
                                    egui::RichText::new(name)
                                        .font(komponist_ui::fonts::font_id(&label_style))
                                        .color(color32(color::TEXT_3)),
                                );
                                if targets.is_empty() {
                                    ui.label(lang.automation_no_params());
                                }
                                for target in targets {
                                    let dot = catalog.already_automated(&target.key);
                                    let driven = catalog.driven(&target.key);
                                    let enabled = driven.is_none();
                                    let label = driven.map_or_else(
                                        || target.label.clone(),
                                        |reason| format!("{} · {reason}", target.label),
                                    );
                                    let selected = enabled && highlighted == Some(index);
                                    let response =
                                        chooser_row(ui, row, &label, selected, dot, enabled);
                                    if selected && (up || down) {
                                        response.scroll_to_me(None);
                                    }
                                    if enabled && (response.clicked() || (enter && selected)) {
                                        result = Some(Choice::Target {
                                            key: target.key,
                                            label: target.label,
                                        });
                                    }
                                    index += usize::from(enabled);
                                    row += 1;
                                }
                            }
                            if add {
                                let response = chooser_row(
                                    ui,
                                    row,
                                    lang.automation_add_lane(),
                                    highlighted == Some(index),
                                    false,
                                    true,
                                );
                                if highlighted == Some(index) && (up || down) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() || (enter && highlighted == Some(index)) {
                                    result = Some(Choice::Add);
                                }
                            }
                        });
                });
            });
        let outside = ui.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|at| !popup.response.rect.contains(at) && !anchor.contains(at))
        });
        if result.is_some() || cancel || outside {
            self.active = None;
            ui.memory_mut(|memory| memory.surrender_focus(popup_id.with("search")));
        }
        result
    }
}

pub(crate) enum Choice {
    Target {
        key: AutomationTarget,
        label: String,
    },
    Add,
}

fn chooser_row(
    ui: &mut Ui,
    index: usize,
    text: &str,
    highlighted: bool,
    dot: bool,
    enabled: bool,
) -> Response {
    ui.push_id(index, |ui| {
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), size::MENU_ROW), sense);
        response.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, enabled, text));
        if !enabled {
            Tip::new(text).attach(&response);
        }
        let look =
            komponist_ui::widgets::dropdown::row_look(highlighted || response.hovered(), enabled);
        if let Some(bg) = look.fill {
            px::fill(ui.painter(), rect, radius::S, bg);
        }
        let style = typography::UI;
        let offset = if dot {
            space::SPACE_1 + space::SPACE_2
        } else {
            space::SPACE_1
        };
        let galley = elided_galley(
            ui,
            text,
            komponist_ui::fonts::font_id(&style),
            style.line_height,
            (rect.width() - offset).max(0.0),
        );
        let pos = widgets::text_pos(rect, &galley, rect.left() + offset, ui.pixels_per_point());
        ui.painter().galley(pos, galley, look.text);
        if dot {
            ui.painter().circle_filled(
                pos2(rect.left() + space::SPACE_1, rect.center().y),
                komponist_tokens::widget::knob::automation_dot::SIZE / 2.0,
                look.text,
            );
        }
        response
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_follows_target_when_rows_change() {
        let mut highlight = Highlight::default();
        let volume = AutomationTarget::Volume;
        let pan = AutomationTarget::Pan;
        let new = AutomationTarget::Macro { index: 0 };
        assert_eq!(
            highlight.update(&[&volume, &pan], false, false, true),
            (Some(1), false)
        );
        assert_eq!(
            highlight.update(&[&new, &volume, &pan], false, false, false),
            (Some(2), false)
        );
        assert_eq!(
            highlight.update(&[&new, &volume], false, false, false),
            (Some(0), true)
        );
    }

    #[test]
    fn highlight_distinguishes_recent_from_regular_and_tracks_add_row() {
        let mut highlight = Highlight::default();
        let volume = AutomationTarget::Volume;
        let pan = AutomationTarget::Pan;
        assert_eq!(
            highlight.update(&[&volume, &pan, &volume], true, false, true),
            (Some(1), false)
        );
        assert_eq!(
            highlight.update(&[&volume, &pan, &volume], true, false, true),
            (Some(2), false)
        );
        assert_eq!(
            highlight.update(&[&pan, &volume], true, false, false),
            (Some(1), false)
        );
        assert_eq!(
            highlight.update(&[&pan, &volume], true, false, true),
            (Some(2), false)
        );
        assert_eq!(
            highlight.update(&[&volume], true, false, false),
            (Some(1), false)
        );
    }
}
