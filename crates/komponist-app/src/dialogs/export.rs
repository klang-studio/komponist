//! S08 "WAV exportieren": the settings dialog, the wait for devices and the progress dialog
//! (`Screens.dc.html` `ovExport` and its notes, `ks-view.js` `exportView`, `Prototyp.dc.html`
//! `ex`).
//!
//! A pure view like the other dialogs: a [`Model`] goes in, [`Output`]s come out, and the app's
//! export flow (`crate::export`) applies them.
//!
//! - **Settings** ([`Stage::Settings`], the left dialog of S08): Bereich (Ganzer Song · Loop 9–13 ·
//!   Auswahl; Loop and Auswahl are disabled without a current range), Von – bis with the range's
//!   length, Nachlauf in seconds, Abtastrate (dropdown 44100 / 48000 / 96000 Hz, ARCHITECTURE
//!   §13.5), Bittiefe, Normalisieren ("auf −1.0 dBFS"), Dithering (disabled for 32 bit float,
//!   which is never dithered), Datei, Ordner with "Ändern" (the file browser). "Exportieren" or
//!   Enter starts; "Abbrechen", × and a click on the scrim close (Esc is the keymap's).
//! - **Devices** ([`Stage::Devices`], no screen): while devices of the project load, are missing
//!   or crashed, the progress dialog's frame lists them and the export waits; it goes on by itself
//!   when all run. "Ohne sie exportieren" goes on at once, "Abbrechen" / Esc ends it.
//! - **Progress** ([`Stage::Progress`], S08 "danach · Fortschritt"): "Exportiere Nachtfahrt.wav",
//!   the percentage, rendered of total time, the bar, Geschwindigkeit, Spitze bisher (the sample
//!   peak rendered so far, before normalisation), Restzeit; "Abbrechen" / Esc.
//!
//! Texts are verbatim from S08 and the prototype; those of the device list and of the status bar
//! messages other than S08's are komponist's own ([`names`]).

use egui::{
    Align, Color32, Context, Galley, Key, Layout, Margin, Modifiers, Rect, Response, Role, Sense,
    Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_model::format::{fmt_db, fmt_time};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, space, typography};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::button::{Button, Kind};
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::switch::Switch;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px};

use super::metrics::MONO_SMALL;
use super::metrics::dialog as d;
use super::metrics::export as m;
use super::{ActionButton, divider, header, label_node, modal, styled};
use crate::actions::{BitDepth, ExportRange, ExportSettings};

/// Texts of the export dialog and of its status bar messages.
pub mod names {
    /// Title of the settings dialog.
    pub const TITLE: &str = "WAV exportieren";
    /// "Bereich" (also the segmented control's accessible name).
    pub const RANGE: &str = "Bereich";
    /// The whole song.
    pub const SONG: &str = "Ganzer Song";
    /// The loop segment without a current loop (`exportView`: "Loop " + bars).
    pub const LOOP: &str = "Loop";
    /// The selection segment.
    pub const SELECTION: &str = "Auswahl";
    /// "Von – bis".
    pub const FROM_TO: &str = "Von – bis";
    /// Accessible name of the range start.
    pub const FROM: &str = "Von";
    /// Accessible name of the range end.
    pub const TO: &str = "Bis";
    /// Accessible name of the range length.
    pub const LENGTH: &str = "Länge";
    /// "Nachlauf".
    pub const TAIL: &str = "Nachlauf";
    /// Its hint.
    pub const TAIL_HINT: &str = "für Hall und Delay";
    /// "Abtastrate".
    pub const RATE: &str = "Abtastrate";
    /// "Bittiefe".
    pub const BIT_DEPTH: &str = "Bittiefe";
    /// The bit depth segments.
    pub const BIT_LABELS: [&str; 3] = ["16 bit", "24 bit", "32 bit float"];
    /// "Normalisieren".
    pub const NORMALIZE: &str = "Normalisieren";
    /// Next to the switch when on (`normInfo`).
    pub const NORMALIZE_ON: &str = "auf \u{2212}1.0 dBFS";
    /// Next to the switch when off.
    pub const NORMALIZE_OFF: &str = "aus";
    /// "Dithering".
    pub const DITHER: &str = "Dithering";
    /// "Datei".
    pub const FILE: &str = "Datei";
    /// "Ordner".
    pub const FOLDER: &str = "Ordner";
    /// The folder's button.
    pub const CHANGE: &str = "Ändern";
    /// "Abbrechen".
    pub const CANCEL: &str = "Abbrechen";
    /// "Exportieren" (primary).
    pub const EXPORT: &str = "Exportieren";
    /// Its shortcut.
    pub const ENTER: &str = "Enter";
    /// The cancel shortcut while exporting.
    pub const ESC: &str = "Esc";
    /// Accessible name of the progress bar.
    pub const PROGRESS: &str = "Fortschritt";
    /// "Geschwindigkeit".
    pub const SPEED: &str = "Geschwindigkeit";
    /// "Spitze bisher".
    pub const PEAK: &str = "Spitze bisher";
    /// "Restzeit".
    pub const REMAINING: &str = "Restzeit";
    /// The device list's text (no screen).
    pub const WAITING: &str = "Der Export wartet, bis alle Geräte laufen.";
    /// A device still loading (prototype `arrView`: "· lädt …").
    pub const LOADING: &str = "lädt …";
    /// A device whose plugin is not installed.
    pub const MISSING: &str = "fehlt";
    /// A device whose bridge crashed or failed to load.
    pub const CRASHED: &str = "abgestürzt";
    /// Go on without the devices that are not ready.
    pub const WITHOUT: &str = "Ohne sie exportieren";
    /// The link of the status bar message (S08 "fertig · Statuszeile").
    pub const SHOW_IN_FOLDER: &str = "Im Ordner zeigen";
    /// The title of the file browser for "Ändern".
    pub const FOLDER_TITLE: &str = "Export-Ordner wählen";
    /// Nothing to export (prototype `openExport`).
    pub const EMPTY: &str = "Leeres Projekt · nichts zu exportieren";
    /// The export was cancelled.
    pub const CANCELLED: &str = "Export abgebrochen";

    /// "Loop 9–13" (`exportView`: bar numbers of the loop's start and end).
    pub fn loop_bars(from: i64, to: i64) -> String {
        format!("{LOOP} {from}–{to}")
    }

    /// Title of the progress dialog: "Exportiere Nachtfahrt.wav".
    pub fn exporting(file: &str) -> String {
        format!("Exportiere {file}")
    }

    /// "00:19.6 von 00:33.0".
    pub fn of(done: &str, total: &str) -> String {
        format!("{done} von {total}")
    }

    /// "6.2 × Echtzeit".
    pub fn speed(factor: f64) -> String {
        format!("{factor:.1} × Echtzeit")
    }

    /// "ca. 2 s" (whole seconds, rounded up), "ca. 3 min" from a minute on.
    pub fn remaining(seconds: f64) -> String {
        let s = seconds.max(0.0).ceil() as u64;
        if s < 60 {
            format!("ca. {s} s")
        } else {
            format!("ca. {} min", s.div_ceil(60))
        }
    }

    /// The peak: "−2.4 dBFS".
    pub fn peak(db: f32) -> String {
        format!("{} dBFS", super::fmt_db(db))
    }

    /// The status bar message of a finished export (prototype: `file + ' exportiert · ' +
    /// mm:ss + ' · ' + bits + ' bit'`): "Nachtfahrt.wav exportiert · 00:33 · 24 bit".
    pub fn done(file: &str, length: &str, bits: u16) -> String {
        format!("{file} exportiert · {length} · {bits} bit")
    }

    /// Appended to [`done`] when devices dropped out of the render: " · Surge XT ist
    /// ausgefallen", " · Surge XT, Dexed sind ausgefallen".
    pub fn dropped(devices: &[String]) -> String {
        match devices {
            [] => String::new(),
            [one] => format!(" · {one} ist ausgefallen"),
            many => format!(" · {} sind ausgefallen", many.join(", ")),
        }
    }

    /// The export failed: "Export fehlgeschlagen · …".
    pub fn failed(why: &str) -> String {
        format!("Export fehlgeschlagen · {why}")
    }

    /// "Im Ordner zeigen" failed.
    pub fn open_failed(why: &str) -> String {
        format!("Ordner lässt sich nicht öffnen · {why}")
    }
}

/// The sample rates of the dropdown (ARCHITECTURE §13.5).
pub const RATES: [u32; 3] = [44_100, 48_000, 96_000];

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// The settings being edited (while exporting: the ones in use).
    pub settings: ExportSettings,
    /// The stage.
    pub stage: Stage,
    /// Whether the dialog takes Enter (not while the file browser is open above it).
    pub keys: bool,
}

/// The dialog's stage.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Configuring.
    Settings(SettingsView),
    /// Waiting for devices that are not ready.
    Devices(Vec<DeviceRow>),
    /// Rendering.
    Progress(ProgressView),
}

/// What the settings dialog shows besides the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsView {
    /// "Loop 9–13" while the loop is on, `None` = no current loop (segment disabled).
    pub loop_label: Option<String>,
    /// Whether there is a selection ("Auswahl" enabled).
    pub selection: bool,
    /// Start of the range ("1.1.1").
    pub from: String,
    /// End of the range ("17.1.1").
    pub to: String,
    /// Length of the range without the tail ("00:31.0").
    pub length: String,
    /// The folder for display ("~/Musik/Exporte").
    pub folder: String,
    /// Whether "Exportieren" is possible (a file name, a non-empty range or a tail).
    pub can_export: bool,
}

/// Why a device is not ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceState {
    /// Loading ("lädt …"); the export goes on by itself when it runs.
    Loading,
    /// Not installed ("fehlt").
    Missing,
    /// Crashed or failed to load ("abgestürzt").
    Crashed,
}

/// A device in the wait list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// The plugin name ("Surge XT").
    pub name: String,
    /// The track ("Pad"), or "Master".
    pub track: String,
    /// Why it is listed.
    pub state: DeviceState,
}

/// What the progress dialog shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressView {
    /// Rendered share `0.0 ..= 1.0`.
    pub fraction: f32,
    /// Rendered time ("00:19.6").
    pub done: String,
    /// Total time with the tail ("00:33.0").
    pub total: String,
    /// Render speed as a multiple of real time, once known.
    pub speed: Option<f64>,
    /// Sample peak so far in dBFS.
    pub peak_db: f32,
    /// Seconds left, once known.
    pub remaining: Option<f64>,
    /// Cancel was asked for; the render stops at its next check.
    pub cancelling: bool,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A setting changed.
    Settings(ExportSettings),
    /// "Exportieren" or Enter.
    Export,
    /// "Ohne sie exportieren".
    ExportWithout,
    /// "Abbrechen", ×, a click on the scrim.
    Cancel,
    /// "Ändern": choose the folder.
    ChooseFolder,
}

/// The file name the export writes: `name` trimmed, with `.wav` added unless it ends with it
/// (any case). `None` for an empty name or one with a path separator.
pub fn file_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    let lower = name.to_lowercase();
    if lower.ends_with(".wav") && lower.len() > 4 {
        Some(name.to_owned())
    } else {
        Some(format!("{name}.wav"))
    }
}

/// The segment index of `range`.
fn range_index(range: ExportRange) -> usize {
    match range {
        ExportRange::Song => 0,
        ExportRange::Loop => 1,
        ExportRange::Selection => 2,
    }
}

/// The segment index of `bits`.
fn bits_index(bits: BitDepth) -> usize {
    match bits {
        BitDepth::Int16 => 0,
        BitDepth::Int24 => 1,
        BitDepth::Float32 => 2,
    }
}

/// Draws the dialog of `model`; returns what the user did this frame.
pub fn show(ctx: &Context, model: &Model) -> Vec<Output> {
    match &model.stage {
        Stage::Settings(view) => settings_dialog(ctx, model, view),
        Stage::Devices(rows) => devices_dialog(ctx, model, rows),
        Stage::Progress(view) => progress_dialog(ctx, model, view),
    }
}

// ------------------------------------------------------------------------------------------------
// Settings
// ------------------------------------------------------------------------------------------------

fn settings_dialog(ctx: &Context, model: &Model, view: &SettingsView) -> Vec<Output> {
    let mut out = Vec::new();
    let mut s = model.settings.clone();
    let mut enter = false;
    let response = modal(ctx, "export", color32(color::SURFACE_2), |ui| {
        let width = super::fit(ctx, vec2(m::WIDTH, 0.0)).x;
        ui.set_width(width);
        if header(ui, width, names::TITLE) {
            out.push(Output::Cancel);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(Margin::same(d::PADDING as i8));
        body.show(ui, |ui| {
            let inner = width - 2.0 * d::PADDING;
            ui.set_width(inner);
            enter = settings_body(ui, inner, &mut s, view, &mut out);
        });
        let footer = egui::Frame::new().inner_margin(Margin::symmetric(
            d::PADDING as i8,
            d::FOOTER_PADDING_Y as i8,
        ));
        footer.show(ui, |ui| {
            let row = Rect::from_min_size(
                ui.cursor().min,
                vec2(width - 2.0 * d::PADDING, d::BUTTON_HEIGHT),
            );
            ui.allocate_rect(row, Sense::hover());
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(row)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            child.spacing_mut().item_spacing.x = d::BUTTON_GAP;
            let mut export = ActionButton::dialog(names::EXPORT, Kind::Primary, 2);
            export.trailing = Some((
                names::ENTER,
                w::font_at(&typography::VALUE_S, wm::MEDIUM),
                None,
            ));
            let can = view.can_export;
            if child
                .add_enabled_ui(can, |ui| export.show(ui))
                .inner
                .clicked()
            {
                out.push(Output::Export);
            }
            // `background:#303236` on the `surface.2` footer.
            if ActionButton::dialog(names::CANCEL, Kind::Secondary, 3)
                .show(&mut child)
                .clicked()
            {
                out.push(Output::Cancel);
            }
        });
        ui.allocate_space(vec2(width, 0.0));
    });
    let free = ctx.memory(|mem| mem.focused().is_none())
        && komponist_ui::widgets::floating::current(ctx).is_none();
    if model.keys
        && view.can_export
        && (enter || (free && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))))
    {
        out.push(Output::Export);
    }
    if response.should_close() && !out.contains(&Output::Cancel) {
        out.push(Output::Cancel);
    }
    if s != model.settings {
        out.insert(0, Output::Settings(s));
    }
    out
}

/// A row of the body grid, `height` high: the label column (120, `type.ui`, `text.2`, centred),
/// the gap, then `add` in the rest, left to right and centred on the row; the row gap below.
fn grid_row<R>(
    ui: &mut Ui,
    width: f32,
    label: &str,
    height: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let r = row(ui, width, label, height, add);
    ui.add_space(m::ROW_GAP);
    r
}

/// [`grid_row`] without the gap below (the last row).
fn row<R>(ui: &mut Ui, width: f32, label: &str, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let label_rect = Rect::from_min_size(rect.min, vec2(m::LABEL_COLUMN, height));
    let g = styled(ui, label, &typography::UI, color32(color::TEXT_2), None);
    let pos = w::text_pos(label_rect, &g, label_rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, g, Color32::PLACEHOLDER);
    label_node(ui, label_rect, label, Role::Label);
    let field = Rect::from_min_max(
        pos2(label_rect.right() + m::COLUMN_GAP, rect.top()),
        rect.max,
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(field)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing = vec2(m::COLUMN_GAP, 0.0);
    add(&mut child)
}

/// A divider row across the grid (`margin:4px 0`).
fn divider_row(ui: &mut Ui, width: f32) {
    ui.add_space(m::DIVIDER_MARGIN);
    divider(ui, width);
    ui.add_space(m::DIVIDER_MARGIN + m::ROW_GAP);
}

/// A read-only value box (`surface.2`, `radius.m`, padding 0 8, Mono 12).
fn value_box(ui: &mut Ui, width: f32, text: &str, name: &str, ink: Color32) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, size::CONTROL), Sense::hover());
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_2));
    let g = super::styled_line(
        ui,
        text,
        &typography::VALUE_M,
        ink,
        width - 2.0 * space::SPACE_2,
    );
    let pos = w::text_pos(
        rect,
        &g,
        rect.left() + space::SPACE_2,
        ui.pixels_per_point(),
    );
    ui.painter().galley(pos, g, ink);
    let response = ui.interact(
        rect,
        ui.scope_id().with(("komponist_export_value", name)),
        Sense::hover(),
    );
    let value = text.to_owned();
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::TextInput, false, name);
        info.current_text_value = Some(value.clone());
        info
    });
    response
}

/// Text in `style` and `ink` at the cursor, with a label node.
fn text(ui: &mut Ui, text: &str, style: &typography::TypeStyle, ink: Color32) -> Response {
    let g = styled(ui, text, style, ink, None);
    let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, g, ink);
    label_node(ui, rect, text, Role::Label)
}

/// The grid of the settings dialog; returns whether Enter left the file name field.
fn settings_body(
    ui: &mut Ui,
    width: f32,
    s: &mut ExportSettings,
    view: &SettingsView,
    out: &mut Vec<Output>,
) -> bool {
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let field_width = width - m::LABEL_COLUMN - m::COLUMN_GAP;
    let control = size::CONTROL;
    let line = typography::UI.line_height;

    let loop_label = view.loop_label.as_deref().unwrap_or(names::LOOP);
    let mut ranges = [
        Item::new(names::SONG),
        Item::new(loop_label),
        Item::new(names::SELECTION),
    ];
    if view.loop_label.is_none() {
        ranges[1] = ranges[1].disabled();
    }
    if !view.selection {
        ranges[2] = ranges[2].disabled();
    }
    let mut range = range_index(s.range);
    grid_row(ui, width, names::RANGE, control, |ui| {
        if ui
            .add(Segmented::new(names::RANGE, &mut range, &ranges))
            .changed()
        {
            s.range = [ExportRange::Song, ExportRange::Loop, ExportRange::Selection][range];
        }
    });

    grid_row(ui, width, names::FROM_TO, control, |ui| {
        ui.spacing_mut().item_spacing.x = m::RANGE_GAP;
        let ink = color32(color::TEXT_1);
        value_box(ui, m::FIELD_WIDTH, &view.from, names::FROM, ink);
        text(ui, "–", &typography::UI, color32(color::TEXT_3));
        value_box(ui, m::FIELD_WIDTH, &view.to, names::TO, ink);
        let r = text(ui, &view.length, &MONO_SMALL, color32(color::TEXT_3));
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, names::LENGTH));
    });

    grid_row(ui, width, names::TAIL, control, |ui| {
        ui.spacing_mut().item_spacing.x = m::HINT_GAP;
        ui.add(
            NumberField::new(&mut s.tail_seconds, names::TAIL)
                .unit("s")
                .range(0.0..=30.0)
                .step(0.1)
                .decimals(1)
                .default_value(f64::from(komponist_engine::export::DEFAULT_TAIL_SECONDS))
                .width(m::FIELD_WIDTH),
        );
        text(
            ui,
            names::TAIL_HINT,
            &typography::SMALL,
            color32(color::TEXT_3),
        );
    });

    divider_row(ui, width);

    let rate_labels: Vec<String> = RATES.iter().map(|r| format!("{r} Hz")).collect();
    let rate_items: Vec<Item<'_>> = rate_labels.iter().map(|l| Item::new(l)).collect();
    let mut rate = RATES.iter().position(|r| *r == s.sample_rate).unwrap_or(1);
    grid_row(ui, width, names::RATE, control, |ui| {
        if ui
            .add(Dropdown::new(names::RATE, &mut rate, &rate_items).width(m::RATE_WIDTH))
            .changed()
        {
            s.sample_rate = RATES[rate];
        }
    });

    let bit_items = names::BIT_LABELS.map(Item::new);
    let mut bits = bits_index(s.bit_depth);
    grid_row(ui, width, names::BIT_DEPTH, control, |ui| {
        if ui
            .add(Segmented::new(names::BIT_DEPTH, &mut bits, &bit_items))
            .changed()
        {
            s.bit_depth = [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32][bits];
        }
    });

    let switch = size::SWITCH[1].max(line);
    grid_row(ui, width, names::NORMALIZE, switch, |ui| {
        ui.spacing_mut().item_spacing.x = m::HINT_GAP;
        ui.add(Switch::new(&mut s.normalize, names::NORMALIZE));
        let info = if s.normalize {
            names::NORMALIZE_ON
        } else {
            names::NORMALIZE_OFF
        };
        text(ui, info, &MONO_SMALL, color32(color::TEXT_2));
    });

    grid_row(ui, width, names::DITHER, switch, |ui| {
        // 32 bit float is never dithered.
        let float = s.bit_depth == BitDepth::Float32;
        let mut on = s.dither && !float;
        if ui
            .add_enabled(!float, Switch::new(&mut on, names::DITHER))
            .changed()
        {
            s.dither = on;
        }
    });

    divider_row(ui, width);

    let enter = grid_row(ui, width, names::FILE, control, |ui| {
        let r = ui.add(
            TextField::new(&mut s.file_name, names::FILE)
                .id(egui::Id::unique("komponist_export_file"))
                .width(field_width),
        );
        r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
    });

    // The last row has no gap below (the body's padding follows).
    let folder = row(ui, width, names::FOLDER, control, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = m::FOLDER_GAP;
            let clicked = ui.add(Button::new(names::CHANGE)).clicked();
            let rest = ui.available_width();
            value_box(
                ui,
                rest,
                &view.folder,
                names::FOLDER,
                color32(color::TEXT_2),
            );
            clicked
        })
        .inner
    });
    if folder {
        out.push(Output::ChooseFolder);
    }
    enter
}

// ------------------------------------------------------------------------------------------------
// Progress and devices
// ------------------------------------------------------------------------------------------------

/// The progress dialog's header: 44 high, `surface.3`, the title in `type.title` with 20 px
/// padding on both sides, no close button (`padding:0 20px`).
fn plain_header(ui: &mut Ui, width: f32, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, d::HEADER_HEIGHT), Sense::hover());
    let r = radius::L as u8;
    ui.painter().rect_filled(
        rect,
        egui::epaint::CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        },
        color32(color::SURFACE_3),
    );
    let g = super::styled_line(
        ui,
        title,
        &typography::TITLE,
        color32(color::TEXT_1),
        width - 2.0 * d::PADDING,
    );
    let pos = w::text_pos(rect, &g, rect.left() + d::PADDING, ui.pixels_per_point());
    let title_rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color32(color::TEXT_1));
    label_node(ui, title_rect, title, Role::Label);
}

/// The frame of the progress and device dialogs: header, `surface.1` body with 20 px padding,
/// footer with the buttons right-aligned. `buttons` adds them right to left.
fn small_dialog(
    ctx: &Context,
    name: &str,
    title: &str,
    body: impl FnOnce(&mut Ui, f32),
    buttons: impl FnOnce(&mut Ui),
) -> egui::ModalResponse<()> {
    modal(ctx, name, color32(color::SURFACE_2), |ui| {
        let width = super::fit(ctx, vec2(m::PROGRESS_WIDTH, 0.0)).x;
        ui.set_width(width);
        plain_header(ui, width, title);
        let frame = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(Margin::same(d::PADDING as i8));
        frame.show(ui, |ui| {
            let inner = width - 2.0 * d::PADDING;
            ui.set_width(inner);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            body(ui, inner);
        });
        let footer = egui::Frame::new().inner_margin(Margin::symmetric(
            d::PADDING as i8,
            d::FOOTER_PADDING_Y as i8,
        ));
        footer.show(ui, |ui| {
            let row = Rect::from_min_size(
                ui.cursor().min,
                vec2(width - 2.0 * d::PADDING, d::BUTTON_HEIGHT),
            );
            ui.allocate_rect(row, Sense::hover());
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(row)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            child.spacing_mut().item_spacing.x = d::BUTTON_GAP;
            buttons(&mut child);
        });
        ui.allocate_space(vec2(width, 0.0));
    })
}

/// "Abbrechen  Esc" (`background:#303236`, the shortcut in Mono 10, `text.3`).
fn cancel_button(ui: &mut Ui) -> Response {
    let mut cancel = ActionButton::dialog(names::CANCEL, Kind::Secondary, 3);
    cancel.trailing = Some((
        names::ESC,
        fonts::font_id(&typography::VALUE_S),
        Some(color32(color::TEXT_3)),
    ));
    cancel.show(ui)
}

fn progress_dialog(ctx: &Context, model: &Model, view: &ProgressView) -> Vec<Output> {
    let mut out = Vec::new();
    let file = file_name(&model.settings.file_name).unwrap_or_default();
    let title = names::exporting(&file);
    small_dialog(
        ctx,
        "export-progress",
        &title,
        |ui, width| progress_body(ui, width, view),
        |ui| {
            if ui
                .add_enabled_ui(!view.cancelling, cancel_button)
                .inner
                .clicked()
            {
                out.push(Output::Cancel);
            }
        },
    );
    out
}

/// The baseline of a one-line galley, from its top.
fn baseline(g: &Galley) -> f32 {
    g.rows.first().map_or(g.size().y, |row| {
        row.pos.y + row.row.glyphs.first().map_or(row.size.y, |gl| gl.pos.y)
    })
}

fn progress_body(ui: &mut Ui, width: f32, view: &ProgressView) {
    let ppp = ui.pixels_per_point();
    // "62 %" and "00:19.6 von 00:33.0" on one baseline (`align-items:baseline`).
    let percent = format!(
        "{} %",
        (view.fraction.clamp(0.0, 1.0) * 100.0).round() as u32
    );
    let big = styled(
        ui,
        &percent,
        &typography::VALUE_L,
        color32(color::TEXT_1),
        None,
    );
    let times = names::of(&view.done, &view.total);
    let small = styled(ui, &times, &MONO_SMALL, color32(color::TEXT_3), None);
    let (row, _) = ui.allocate_exact_size(vec2(width, big.size().y), Sense::hover());
    let big_pos = px::snap_rect(Rect::from_min_size(row.min, big.size()), ppp).min;
    let line = row.top() + baseline(&big);
    let small_pos = pos2(row.right() - small.size().x, line - baseline(&small));
    let small_pos = px::snap_rect(Rect::from_min_size(small_pos, small.size()), ppp).min;
    let (big_size, small_size) = (big.size(), small.size());
    ui.painter().galley(big_pos, big, Color32::PLACEHOLDER);
    ui.painter().galley(small_pos, small, Color32::PLACEHOLDER);
    label_node(
        ui,
        Rect::from_min_size(big_pos, big_size),
        &percent,
        Role::Label,
    );
    label_node(
        ui,
        Rect::from_min_size(small_pos, small_size),
        &times,
        Role::Label,
    );
    ui.add_space(m::PROGRESS_GAP);

    let (bar, response) = ui.allocate_exact_size(vec2(width, m::BAR_HEIGHT), Sense::hover());
    let fraction = view.fraction.clamp(0.0, 1.0);
    px::fill(ui.painter(), bar, radius::S, color32(color::SURFACE_3));
    if fraction > 0.0 {
        let done = Rect::from_min_size(bar.min, vec2(bar.width() * fraction, bar.height()));
        // `overflow:hidden` on the rounded track: the fill keeps the track's corners.
        let clip = ui.painter().with_clip_rect(done.intersect(ui.clip_rect()));
        px::fill(&clip, bar, radius::S, color32(color::ACCENT));
    }
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ProgressIndicator, true, names::PROGRESS);
        info.value = Some(f64::from(fraction));
        info
    });
    w::accesskit(&response, |node| {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(1.0);
        node.set_numeric_value(f64::from(fraction));
        node.set_value(percent.clone());
    });
    ui.add_space(m::PROGRESS_GAP);

    let speed = view.speed.map_or_else(|| "–".to_owned(), names::speed);
    let peak = names::peak(view.peak_db);
    let remaining = view
        .remaining
        .map_or_else(|| "–".to_owned(), names::remaining);
    let rows = [
        (names::SPEED, speed),
        (names::PEAK, peak),
        (names::REMAINING, remaining),
    ];
    for (i, (label, value)) in rows.iter().enumerate() {
        let l = styled(ui, label, &typography::UI, color32(color::TEXT_2), None);
        let v = styled(
            ui,
            value,
            &typography::VALUE_M,
            color32(color::TEXT_1),
            None,
        );
        let height = l.size().y.max(v.size().y);
        let (row, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let l_pos = w::text_pos(row, &l, row.left(), ppp);
        let v_pos = w::text_pos(row, &v, row.right() - v.size().x, ppp);
        let (l_size, v_size) = (l.size(), v.size());
        ui.painter().galley(l_pos, l, Color32::PLACEHOLDER);
        ui.painter().galley(v_pos, v, Color32::PLACEHOLDER);
        label_node(ui, Rect::from_min_size(l_pos, l_size), label, Role::Label);
        let r = label_node(ui, Rect::from_min_size(v_pos, v_size), value, Role::Label);
        let name = format!("{label}: {value}");
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
        if i + 1 < rows.len() {
            ui.add_space(m::VALUES_GAP[1]);
        }
    }
}

fn devices_dialog(ctx: &Context, model: &Model, rows: &[DeviceRow]) -> Vec<Output> {
    let mut out = Vec::new();
    let file = file_name(&model.settings.file_name).unwrap_or_default();
    let title = names::exporting(&file);
    small_dialog(
        ctx,
        "export-devices",
        &title,
        |ui, width| devices_body(ui, width, rows),
        |ui| {
            if ActionButton::dialog(names::WITHOUT, Kind::Primary, 2)
                .show(ui)
                .clicked()
            {
                out.push(Output::ExportWithout);
            }
            if cancel_button(ui).clicked() {
                out.push(Output::Cancel);
            }
        },
    );
    out
}

fn devices_body(ui: &mut Ui, width: f32, rows: &[DeviceRow]) {
    let g = styled(
        ui,
        names::WAITING,
        &typography::UI,
        color32(color::TEXT_2),
        Some(width),
    );
    super::paint_text(ui, g, names::WAITING);
    ui.add_space(m::PROGRESS_GAP);
    egui::ScrollArea::vertical()
        .id_salt("komponist_export_devices")
        .max_height(m::DEVICE_LIST_MAX_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            device_rows(ui, width, rows);
        });
}

fn device_rows(ui: &mut Ui, width: f32, rows: &[DeviceRow]) {
    let ppp = ui.pixels_per_point();
    for (i, row) in rows.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(width, m::DEVICE_ROW), Sense::hover());
        px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_2));
        let inner = rect.shrink2(vec2(m::DEVICE_PADDING_X, 0.0));
        let (state, ink) = match row.state {
            DeviceState::Loading => (names::LOADING, color::TEXT_2),
            DeviceState::Missing => (names::MISSING, color::STATE_ERROR),
            DeviceState::Crashed => (names::CRASHED, color::STATE_ERROR),
        };
        let st = styled(ui, state, &typography::UI, color32(ink), None);
        let st_pos = w::text_pos(inner, &st, inner.right() - st.size().x, ppp);
        let room = st_pos.x - inner.left() - space::SPACE_3;
        let name = super::styled_line(ui, &row.name, &typography::UI, color32(color::TEXT_1), room);
        let name_pos = w::text_pos(inner, &name, inner.left(), ppp);
        let track_x = name_pos.x + name.size().x + space::SPACE_2;
        let track = super::styled_line(
            ui,
            &row.track,
            &typography::UI,
            color32(color::TEXT_3),
            (st_pos.x - track_x - space::SPACE_3).max(0.0),
        );
        let track_pos = w::text_pos(inner, &track, track_x, ppp);
        ui.painter().galley(name_pos, name, Color32::PLACEHOLDER);
        ui.painter().galley(track_pos, track, Color32::PLACEHOLDER);
        ui.painter().galley(st_pos, st, Color32::PLACEHOLDER);
        let label = format!("{} · {} · {state}", row.name, row.track);
        label_node(ui, rect, &label, Role::ListItem);
        if i + 1 < rows.len() {
            ui.add_space(m::DEVICE_GAP);
        }
    }
}

/// `mm:ss.t`, rounded to tenths: "00:31.0" for the 30.97 s of S08's 16 bars at 124 BPM (the
/// screen rounds; the prototype's `K.fmtTime(s).slice(0,7)` would cut to "00:30.9").
pub fn tenths(seconds: f64) -> String {
    let full = fmt_time((seconds * 10.0).round() / 10.0);
    full[..7].to_owned()
}

/// `mm:ss`, rounded to seconds: "00:33" for 32.97 s (S08's status bar message).
pub fn minutes_seconds(seconds: f64) -> String {
    let full = fmt_time(seconds.round());
    full[..5].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_get_the_extension_once() {
        assert_eq!(file_name("Nachtfahrt").as_deref(), Some("Nachtfahrt.wav"));
        assert_eq!(
            file_name(" Nachtfahrt.wav ").as_deref(),
            Some("Nachtfahrt.wav")
        );
        assert_eq!(file_name("Mix.WAV").as_deref(), Some("Mix.WAV"));
        assert_eq!(file_name(".wav").as_deref(), Some(".wav.wav"));
        assert_eq!(file_name("  "), None);
        assert_eq!(file_name("a/b.wav"), None);
        assert_eq!(file_name(".."), None);
    }

    #[test]
    fn texts_follow_the_prototype() {
        assert_eq!(tenths(30.968), "00:31.0");
        assert_eq!(tenths(19.64), "00:19.6");
        assert_eq!(minutes_seconds(32.968), "00:33");
        assert_eq!(minutes_seconds(59.6), "01:00");
        assert_eq!(names::loop_bars(9, 13), "Loop 9–13");
        assert_eq!(
            names::done("Nachtfahrt.wav", "00:33", 24),
            "Nachtfahrt.wav exportiert · 00:33 · 24 bit"
        );
        assert_eq!(names::speed(6.24), "6.2 × Echtzeit");
        assert_eq!(names::remaining(1.2), "ca. 2 s");
        assert_eq!(names::remaining(150.0), "ca. 3 min");
        assert_eq!(names::peak(-2.4), "\u{2212}2.4 dBFS");
        assert_eq!(names::of("00:19.6", "00:33.0"), "00:19.6 von 00:33.0");
        assert_eq!(names::dropped(&[]), "");
        assert_eq!(
            names::dropped(&["Surge XT".into()]),
            " · Surge XT ist ausgefallen"
        );
        assert_eq!(
            names::dropped(&["Surge XT".into(), "Dexed".into()]),
            " · Surge XT, Dexed sind ausgefallen"
        );
    }
}
