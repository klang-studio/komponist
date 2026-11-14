//! S06 "Plugins fehlen": after opening a project whose plugins are not in the catalog
//! (`Screens.dc.html` `ovMissing`, `missing`; notes `S06`).
//!
//! The dialog lists every missing device with its plugin name, format, place ("Spur Arp ·
//! Instrument", "Spur Pad · Effekt 4") and module file name. "Ohne diese Plugins fortfahren" (Enter, Esc)
//! keeps them as placeholders with their state; "Plugin-Ordner prüfen" opens the settings at
//! "Plugins" and rescans.

use egui::{Color32, Context, Rect, Role, Sense, Ui, pos2, vec2};
use komponist_model::{ChannelRef, DeviceRole, PluginFormat, PluginRef, Project};
use komponist_plugins::MissingPlugin;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, typography};
use komponist_ui::px;
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::{self as w, metrics as wm};

use super::metrics::{dialog as dm, missing as m};
use super::{ActionButton, button_row, dot, galley, label_node, modal, styled};

/// The S06 texts.
pub mod names {
    /// "Plugin-Ordner prüfen".
    pub const CHECK_FOLDERS: &str = "Plugin-Ordner prüfen";
    /// The primary action with several plugins.
    pub const CONTINUE_MANY: &str = "Ohne diese Plugins fortfahren";
    /// The primary action with one plugin.
    pub const CONTINUE_ONE: &str = "Ohne dieses Plugin fortfahren";
}

/// One missing device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Plugin name ("Vital").
    pub name: String,
    /// Format chip ("VST3", "CLAP", "INTERN").
    pub format: &'static str,
    /// Where it sits ("Spur Arp · Instrument").
    pub place: String,
    /// Module file name ("Vital.vst3", "Surge XT.clap"); empty when the project does not know it.
    pub module: String,
}

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// Project name ("Nachtfahrt").
    pub project: String,
    /// The missing devices in document order.
    pub rows: Vec<Row>,
}

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// "Ohne diese Plugins fortfahren", Enter, Esc or a click beside the dialog.
    Continue,
    /// "Plugin-Ordner prüfen".
    CheckFolders,
}

/// The format chip of a plugin format.
pub fn format_label(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Builtin => "INTERN",
    }
}

/// The model for `missing` in `project`.
pub fn model(project: &Project, missing: &[MissingPlugin]) -> Model {
    let rows = missing
        .iter()
        .map(|mp| {
            let channel = match mp.track {
                Some(t) => ChannelRef::Track(t),
                None => ChannelRef::Master,
            };
            let (owner, devices) = match channel {
                ChannelRef::Track(t) => match project.track(t) {
                    Some(track) => (format!("Spur {}", track.name), &track.channel.devices),
                    None => ("Spur".to_owned(), &project.master.devices),
                },
                ChannelRef::Master => ("Master".to_owned(), &project.master.devices),
            };
            let role = match mp.role {
                DeviceRole::Instrument => "Instrument".to_owned(),
                DeviceRole::Effect => {
                    // The effect's number in its chain, the instrument not counted.
                    let n = devices
                        .iter()
                        .take(mp.index + 1)
                        .filter(|d| d.role == DeviceRole::Effect)
                        .count()
                        .max(1);
                    format!("Effekt {n}")
                }
            };
            Row {
                name: mp.plugin.name.clone(),
                format: format_label(mp.plugin.format),
                place: format!("{owner} · {role}"),
                module: module_label(&mp.plugin),
            }
        })
        .collect();
    Model {
        project: project.name.clone(),
        rows,
    }
}

/// The line under the place: the module file name. A CLAP plugin from an old project falls back to
/// its readable id; a VST3 hex class id is never shown.
fn module_label(plugin: &PluginRef) -> String {
    if !plugin.module.is_empty() {
        plugin.module.to_string()
    } else if plugin.format == PluginFormat::Clap {
        plugin.id.clone()
    } else {
        String::new()
    }
}

/// "2 Plugins fehlen", "1 Plugin fehlt".
pub fn title(n: usize) -> String {
    if n == 1 {
        "1 Plugin fehlt".to_owned()
    } else {
        format!("{n} Plugins fehlen")
    }
}

/// The explanation under the title.
pub fn text(project: &str, n: usize) -> String {
    if n == 1 {
        format!(
            "„{project}“ ist geöffnet. Dieses Gerät ist nicht installiert oder liegt nicht in \
             einem Plugin-Ordner. Seine Einstellungen bleiben im Projekt gespeichert und werden \
             wieder geladen, sobald das Plugin gefunden wird."
        )
    } else {
        format!(
            "„{project}“ ist geöffnet. Diese Geräte sind nicht installiert oder liegen nicht in \
             einem Plugin-Ordner. Ihre Einstellungen bleiben im Projekt gespeichert und werden \
             wieder geladen, sobald das Plugin gefunden wird."
        )
    }
}

/// Draws the dialog; returns the user's choice in the frame it is made.
pub fn show(ctx: &Context, model: &Model) -> Option<Output> {
    let n = model.rows.len();
    let mut out = None;
    let response = modal(ctx, "missing_plugins", color32(color::SURFACE_3), |ui| {
        let width = m::WIDTH;
        ui.set_width(width);
        ui.add_space(dm::PADDING);
        let inner = width - 2.0 * dm::PADDING;
        indented(ui, dm::PADDING, |ui| {
            title_row(ui, &title(n));
            ui.add_space(m::TEXT_GAP);
            let body = text(&model.project, n);
            let g = galley(
                ui,
                &body,
                w::font_at(&typography::TRACK, dm::TEXT_WEIGHT),
                dm::TEXT_LINE_HEIGHT,
                color32(color::TEXT_2),
                Some(inner),
            );
            super::paint_text(ui, g, &body);
        });
        ui.add_space(m::LIST_MARGIN_Y);
        indented(ui, dm::PADDING, |ui| {
            egui::ScrollArea::vertical()
                .max_height(m::LIST_MAX_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = m::LIST_GAP;
                    for row in &model.rows {
                        missing_row(ui, inner, row);
                    }
                });
        });
        ui.add_space(m::LIST_MARGIN_Y + m::FOOTER_PADDING_TOP);
        indented(ui, dm::PADDING, |ui| {
            let primary = if n == 1 {
                names::CONTINUE_ONE
            } else {
                names::CONTINUE_MANY
            };
            let r = button_row(
                ui,
                inner,
                vec![
                    ActionButton::dialog(names::CHECK_FOLDERS, Kind::Secondary, 3),
                    ActionButton::dialog(primary, Kind::Primary, 3),
                ],
            );
            if r[0].clicked() {
                out = Some(Output::CheckFolders);
            }
            if r[1].clicked() {
                out = Some(Output::Continue);
            }
        });
        ui.add_space(dm::PADDING);
    });
    let enter = ctx.memory(|mem| mem.focused().is_none())
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if out.is_none() && (enter || response.should_close()) {
        out = Some(Output::Continue);
    }
    out
}

/// Runs `add` in a column `left` px in from the dialog's edge.
fn indented(ui: &mut Ui, left: f32, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.add_space(left);
        ui.vertical(add);
    });
}

/// The red dot and the title.
fn title_row(ui: &mut Ui, title: &str) {
    let g = styled(ui, title, &typography::TITLE, color32(color::TEXT_1), None);
    let height = g.size().y;
    let (rect, _) = ui.allocate_exact_size(
        vec2(m::DOT + m::TITLE_GAP + g.size().x, height),
        Sense::hover(),
    );
    dot(
        ui,
        pos2(rect.left() + m::DOT / 2.0, rect.center().y),
        m::DOT,
        color32(color::STATE_ERROR),
    );
    let pos = pos2(rect.left() + m::DOT + m::TITLE_GAP, rect.top());
    let text_rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, Color32::PLACEHOLDER);
    label_node(ui, text_rect, title, Role::Label);
}

/// One row: name and format chip, place and module file name (`gap:4px 12px`, `padding:10px 12px`).
fn missing_row(ui: &mut Ui, width: f32, row: &Row) {
    let ppp = ui.pixels_per_point();
    let name = galley(
        ui,
        &row.name,
        w::font_at(&typography::TRACK, wm::MEDIUM),
        typography::TRACK.line_height,
        color32(color::TEXT_1),
        None,
    );
    let place = styled(
        ui,
        &row.place,
        &typography::SMALL,
        color32(color::TEXT_3),
        None,
    );
    let chip = styled(
        ui,
        row.format,
        &typography::VALUE_S,
        color32(color::TEXT_3),
        None,
    );
    let id = styled(
        ui,
        &row.module,
        &typography::VALUE_S,
        color32(color::TEXT_3),
        None,
    );
    let [pad_x, pad_y] = m::ROW_PADDING;
    let [gap_x, gap_y] = m::ROW_GAP;
    let [chip_x, chip_y] = m::CHIP_PADDING;
    let line1 = name.size().y.max(chip.size().y + 2.0 * chip_y);
    let line2 = place.size().y.max(id.size().y);
    let height = pad_y * 2.0 + line1 + gap_y + line2;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    px::fill(painter, rect, radius::M, color32(color::SURFACE_2));
    let inner = rect.shrink2(vec2(pad_x, pad_y));
    // The second column is as wide as its widest entry; the chip fills it (CSS grid stretch).
    let column = (chip.size().x + 2.0 * chip_x).max(id.size().x).ceil();
    let col_x = inner.right() - column;
    let row1 = Rect::from_min_size(inner.min, vec2(inner.width(), line1));
    let row2 = Rect::from_min_size(
        pos2(inner.left(), row1.bottom() + gap_y),
        vec2(inner.width(), line2),
    );
    let clip = painter.with_clip_rect(Rect::from_x_y_ranges(
        inner.left()..=col_x - gap_x,
        rect.y_range(),
    ));
    clip.galley(
        w::text_pos(row1, &name, inner.left(), ppp),
        name,
        Color32::PLACEHOLDER,
    );
    let chip_rect = Rect::from_min_size(
        pos2(
            col_x,
            row1.center().y - (chip.size().y + 2.0 * chip_y) / 2.0,
        ),
        vec2(column, chip.size().y + 2.0 * chip_y),
    );
    px::fill(painter, chip_rect, radius::S, color32(color::SURFACE_1));
    painter.galley(
        w::text_pos(chip_rect, &chip, chip_rect.left() + chip_x, ppp),
        chip,
        Color32::PLACEHOLDER,
    );
    clip.galley(
        w::text_pos(row2, &place, inner.left(), ppp),
        place,
        Color32::PLACEHOLDER,
    );
    painter.galley(
        w::text_pos(row2, &id, inner.right() - id.size().x, ppp),
        id,
        Color32::PLACEHOLDER,
    );
    let label = if row.module.is_empty() {
        format!("{} {}, {}", row.name, row.format, row.place)
    } else {
        format!("{} {}, {}, {}", row.name, row.format, row.place, row.module)
    };
    label_node(ui, rect, &label, Role::ListItem);
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;
    use komponist_model::{DeviceId, PluginRef};

    fn missing(project: &Project, track: usize, index: usize) -> MissingPlugin {
        let t = &project.tracks[track];
        let d = &t.channel.devices[index];
        MissingPlugin {
            track: Some(t.id),
            device: d.id,
            role: d.role,
            index,
            plugin: d.plugin.clone(),
        }
    }

    #[test]
    fn rows_name_track_role_and_id() {
        let project = demo_project();
        // Pad (track 3): Surge XT, then effects.
        let pad = &project.tracks[3];
        assert!(pad.channel.devices.len() >= 2);
        let rows = model(
            &project,
            &[missing(&project, 3, 0), missing(&project, 3, 1)],
        )
        .rows;
        assert_eq!(rows[0].place, format!("Spur {} · Instrument", pad.name));
        assert_eq!(rows[0].format, "CLAP");
        assert_eq!(rows[0].name, "Surge XT");
        assert_eq!(rows[1].place, format!("Spur {} · Effekt 1", pad.name));
        // Master effects count from 1 too.
        let master = MissingPlugin {
            track: None,
            device: DeviceId(999),
            role: DeviceRole::Effect,
            index: 0,
            plugin: PluginRef {
                format: PluginFormat::Vst3,
                id: "com.tal.reverb4".into(),
                name: "TAL-Reverb-4".into(),
                vendor: "TAL".into(),
                version: "1".into(),
                module: Box::default(),
            },
        };
        let row = &model(&project, &[master]).rows[0];
        assert_eq!(row.place, "Master · Effekt 1");
        assert_eq!(row.format, "VST3");
        // An old project without a module name: a VST3 shows nothing, never the class id.
        assert_eq!(row.module, "");
    }

    #[test]
    fn module_file_name_replaces_the_class_id() {
        let project = demo_project();
        let vst3 = |module: &str| MissingPlugin {
            track: None,
            device: DeviceId(999),
            role: DeviceRole::Effect,
            index: 0,
            plugin: PluginRef {
                format: PluginFormat::Vst3,
                id: "ABCDEF0123456789ABCDEF0123456789".into(),
                name: "Vital".into(),
                vendor: "Vital Audio".into(),
                version: "1".into(),
                module: module.into(),
            },
        };
        assert_eq!(
            model(&project, &[vst3("Vital.vst3")]).rows[0].module,
            "Vital.vst3"
        );
        assert_eq!(model(&project, &[vst3("")]).rows[0].module, "");
        // The demo's CLAP devices carry their module name.
        let rows = model(&project, &[missing(&project, 3, 0)]).rows;
        assert_eq!(rows[0].module, "Surge XT.clap");
    }

    #[test]
    fn titles_and_texts_follow_the_count() {
        assert_eq!(title(2), "2 Plugins fehlen");
        assert_eq!(title(1), "1 Plugin fehlt");
        assert!(text("Nachtfahrt", 2).starts_with("„Nachtfahrt“ ist geöffnet. Diese Geräte"));
        assert!(text("Nachtfahrt", 1).contains("Dieses Gerät ist nicht installiert"));
    }
}
