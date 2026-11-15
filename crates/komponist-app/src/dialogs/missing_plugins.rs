//! S06 "Plugins fehlen": after opening a project whose plugins are not in the catalog
//! (`Screens.dc.html` `ovMissing`, `missing`; notes `S06`).
//!
//! The dialog lists every missing device with its plugin name, format, place ("Spur Arp ·
//! Instrument", "Spur Pad · Effekt 4") and module file name. "Ohne diese Plugins fortfahren" (Enter, Esc)
//! keeps them as placeholders with their state; "Plugin-Ordner prüfen" opens the settings at
//! "Plugins" and rescans.
//!
//! **Missing samples (WP-87)** are listed in the same dialog, in the same row style: the format
//! chip says "SAMPLE", the place is "Spur Drums · Pad 3", the module line the file name, and
//! "Suchen …" opens the file browser to relink it (the other missing samples of the project that
//! are in the chosen folder follow). The rows come from the project every frame, so a found
//! sample leaves the list; with nothing left the dialog closes. With samples only the title is
//! "1 Sample fehlt" / "n Samples fehlen" and the primary action "Ohne diese Samples fortfahren".

use egui::{Color32, Context, Rect, Role, Sense, Ui, pos2, vec2};
use komponist_i18n::Lang;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, PluginFormat, PluginRef, Project};
use komponist_plugins::MissingPlugin;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, typography};
use komponist_ui::px;
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::{self as w, metrics as wm};

use super::metrics::{dialog as dm, missing as m};
use super::{ActionButton, button_row, dot, galley, label_node, modal, styled};

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
    /// A missing sample: the sampler device and pad (0-based) "Suchen …" relinks.
    pub search: Option<(DeviceId, usize)>,
}

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// Project name ("Nachtfahrt").
    pub project: String,
    /// The missing devices in document order.
    pub rows: Vec<Row>,
    /// The missing samples in document order (after the devices).
    pub samples: Vec<Row>,
}

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// "Ohne diese Plugins fortfahren", Enter, Esc or a click beside the dialog.
    Continue,
    /// "Plugin-Ordner prüfen".
    CheckFolders,
    /// "Suchen …" on a missing sample: the file browser for this pad.
    Search(DeviceId, usize),
}

/// The format chip of a plugin format ("INTERN" / "BUILT-IN" for the built-in devices).
pub fn format_label(t: Lang, format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Builtin => t.missing_format_builtin(),
    }
}

/// The owner of a device in a place: "Spur Arp" ("Track Arp"), "Master".
fn owner_label(t: Lang, track: Option<&komponist_model::Track>) -> String {
    match track {
        Some(track) => t.dialog_track_place(&track.name),
        None => t.dialog_master().to_owned(),
    }
}

/// The model for `missing` in `project`.
pub fn model(t: Lang, project: &Project, missing: &[MissingPlugin]) -> Model {
    let rows = missing
        .iter()
        .map(|mp| {
            let channel = match mp.track {
                Some(t) => ChannelRef::Track(t),
                None => ChannelRef::Master,
            };
            let (owner, devices) = match channel {
                ChannelRef::Track(id) => match project.track(id) {
                    Some(track) => (owner_label(t, Some(track)), &track.channel.devices),
                    None => (t.dialog_track_unknown().to_owned(), &project.master.devices),
                },
                ChannelRef::Master => (owner_label(t, None), &project.master.devices),
            };
            let role = match mp.role {
                DeviceRole::Instrument => t.missing_role_instrument().to_owned(),
                DeviceRole::Effect => {
                    // The effect's number in its chain, the instrument not counted.
                    let n = devices
                        .iter()
                        .take(mp.index + 1)
                        .filter(|d| d.role == DeviceRole::Effect)
                        .count()
                        .max(1);
                    t.missing_role_effect(n)
                }
                DeviceRole::MidiEffect => {
                    let n = devices
                        .iter()
                        .take(mp.index + 1)
                        .filter(|d| d.role == DeviceRole::MidiEffect)
                        .count()
                        .max(1);
                    t.missing_role_midi_effect(n)
                }
            };
            Row {
                name: mp.plugin.name.clone(),
                format: format_label(t, mp.plugin.format),
                place: t.missing_place(owner, role),
                module: module_label(&mp.plugin),
                search: None,
            }
        })
        .collect();
    Model {
        project: project.name.clone(),
        rows,
        samples: Vec::new(),
    }
}

/// A sample the project links that is not at its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSample {
    /// The sampler device.
    pub device: DeviceId,
    /// The track it is on (`None`: master).
    pub track: Option<komponist_model::TrackId>,
    /// The pad, 0-based.
    pub pad: usize,
    /// The pad's name (the sample's).
    pub name: String,
    /// The file the link names, for the module line.
    pub file: String,
}

/// The missing samples of every sampler device of `project`, resolved against `project_dir`, in
/// document order.
pub fn missing_samples(
    project: &Project,
    project_dir: Option<&std::path::Path>,
) -> Vec<MissingSample> {
    let chains = project
        .tracks
        .iter()
        .map(|t| (Some(t.id), &t.channel.devices))
        .chain(std::iter::once((None, &project.master.devices)));
    let mut out = Vec::new();
    for (track, devices) in chains {
        for device in devices.iter() {
            if !crate::device_host::internal_kind(device).is_some_and(|k| k.is_instrument()) {
                continue;
            }
            for (pad, link) in
                komponist_builtin::sampler::links::missing(device.state.as_deref(), project_dir)
            {
                let file = link
                    .abs
                    .file_name()
                    .or_else(|| link.rel.file_name())
                    .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                out.push(MissingSample {
                    device: device.id,
                    track,
                    pad,
                    name: link.name.clone(),
                    file,
                });
            }
        }
    }
    out
}

/// The model with the missing samples added.
pub fn with_samples(
    t: Lang,
    mut model: Model,
    project: &Project,
    samples: &[MissingSample],
) -> Model {
    model.samples = samples
        .iter()
        .map(|s| {
            let owner = owner_label(t, s.track.and_then(|id| project.track(id)).map(|t| &**t));
            Row {
                name: if s.name.is_empty() {
                    s.file.clone()
                } else {
                    s.name.clone()
                },
                format: t.missing_sample_chip(),
                place: t.missing_place_pad(owner, s.pad + 1),
                module: s.file.clone(),
                search: Some((s.device, s.pad)),
            }
        })
        .collect();
    model
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

/// "2 Plugins fehlen", "1 Plugin fehlt" ("2 Plugins Missing").
pub fn title(t: Lang, n: usize) -> String {
    t.missing_title_plugins(n)
}

/// The title for `plugins` missing plugins and `samples` missing samples: "1 Sample fehlt",
/// "2 Plugins und 3 Samples fehlen". The status bar's `project-opened-problems` uses it too.
pub fn title_for(t: Lang, plugins: usize, samples: usize) -> String {
    match (plugins, samples) {
        (_, 0) => title(t, plugins),
        (0, n) => t.missing_title_samples(n),
        (p, s) => t.missing_title_both(t.missing_count_plugins(p), t.missing_count_samples(s)),
    }
}

/// The explanation for samples only.
pub fn samples_text(t: Lang, project: &str, n: usize) -> String {
    t.missing_samples_text(n, project)
}

/// The explanation under the title.
pub fn text(t: Lang, project: &str, n: usize) -> String {
    t.missing_text(n, project)
}

/// Draws the dialog; returns the user's choice in the frame it is made.
///
/// `keys` is false while something is on top of the dialog (the file browser of "Suchen …"): Enter
/// and Esc belong to it then.
pub fn show(ctx: &Context, model: &Model, keys: bool) -> Option<Output> {
    let t = komponist_ui::lang(ctx);
    let n = model.rows.len();
    let samples = model.samples.len();
    let mut out = None;
    let response = modal(ctx, "missing_plugins", color32(color::SURFACE_3), |ui| {
        let width = m::WIDTH;
        ui.set_width(width);
        ui.add_space(dm::PADDING);
        let inner = width - 2.0 * dm::PADDING;
        indented(ui, dm::PADDING, |ui| {
            title_row(ui, &title_for(t, n, samples));
            ui.add_space(m::TEXT_GAP);
            let body = if n == 0 {
                samples_text(t, &model.project, samples)
            } else {
                text(t, &model.project, n)
            };
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
                    for row in model.rows.iter().chain(&model.samples) {
                        if let Some(search) = missing_row(ui, t, inner, row) {
                            out = Some(search);
                        }
                    }
                });
        });
        ui.add_space(m::LIST_MARGIN_Y + m::FOOTER_PADDING_TOP);
        indented(ui, dm::PADDING, |ui| {
            let primary = match (n, samples) {
                (0, 1) => t.missing_continue_sample(),
                (0, _) => t.missing_continue_samples(),
                (1, 0) => t.missing_continue_one(),
                _ => t.missing_continue_many(),
            };
            let r = button_row(
                ui,
                inner,
                vec![
                    ActionButton::dialog(t.missing_check_folders(), Kind::Secondary, 3),
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
    let enter = keys
        && ctx.memory(|mem| mem.focused().is_none())
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if out.is_none() && (enter || (keys && response.should_close())) {
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
fn missing_row(ui: &mut Ui, t: Lang, width: f32, row: &Row) -> Option<Output> {
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
    let painter = ui.painter().clone();
    let painter = &painter;
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
    // "Suchen …" of a missing sample, left of the chip column, centred on the first line.
    let (device, pad) = row.search?;
    let size = vec2(m::SEARCH_WIDTH, m::SEARCH_HEIGHT);
    // The buttons share one column, whatever the width of the file name beside them.
    let button_right = (inner.right() - m::SAMPLE_COLUMN).min(col_x - gap_x);
    let at = Rect::from_min_size(
        pos2(button_right - size.x, row1.center().y - size.y / 2.0),
        size,
    );
    // A child at a fixed place: `put` would move the list's cursor.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(at));
    let button = child.add(
        komponist_ui::widgets::button::Button::new(t.missing_search())
            .small()
            .ground(2),
    );
    button.clicked().then_some(Output::Search(device, pad))
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
            Lang::De,
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
        let row = &model(Lang::De, &project, &[master]).rows[0];
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
            model(Lang::De, &project, &[vst3("Vital.vst3")]).rows[0].module,
            "Vital.vst3"
        );
        assert_eq!(model(Lang::De, &project, &[vst3("")]).rows[0].module, "");
        // The demo's CLAP devices carry their module name.
        let rows = model(Lang::De, &project, &[missing(&project, 3, 0)]).rows;
        assert_eq!(rows[0].module, "Surge XT.clap");
    }

    #[test]
    fn titles_and_texts_follow_the_count() {
        assert_eq!(title(Lang::De, 2), "2 Plugins fehlen");
        assert_eq!(title(Lang::De, 1), "1 Plugin fehlt");
        assert!(
            text(Lang::De, "Nachtfahrt", 2).starts_with("„Nachtfahrt“ ist geöffnet. Diese Geräte")
        );
        assert!(text(Lang::De, "Nachtfahrt", 1).contains("Dieses Gerät ist nicht installiert"));
    }
}
