//! The browser's rows as data: what the list shows for a segment, a search text, the catalog,
//! the favourites, the open folders and the project, and what activating a row does. Pure, so
//! the list's structure is tested without pixels.
//!
//! | Segment | Without search (`browserAll`, `browserView`) | With search (`browserSearch`) |
//! |---|---|---|
//! | Alle | Favoriten · Instrumente · Effekte (category folders, then uncategorised effects) · Presets · hexa (category folders) · Projekt (Takes, Exporte) | „n Treffer für „q““ · Presets mit „q“ · Kategorie … |
//! | Instr. | Instrumente · Presets · hexa | hits among the instruments · presets |
//! | Effekte | Effekte with a sub-header per category (prototype) | hits among the effects · Kategorie … |
//! | Dateien | Takes · Exporte | the takes and exports whose name contains the text |
//!
//! Folders start collapsed ([`default_collapsed`], S01 shows ▸); their keys are
//! "section/label" ("Effekte/Dynamik"), so a hexa category cannot collide with an effect
//! category.

use std::collections::BTreeSet;

use komponist_ipc::PluginKind;
use komponist_model::{ClipContent, ClipId, DeviceRole, PluginRef, Project, TrackId};
use komponist_plugins::{Catalog, CatalogEntry, EffectCategory};

use crate::actions::{Action, DropTarget, EditAction, PluginAction, UiAction};
use crate::device_host::is_hexa;
use crate::settings::Favorites;
use crate::ui_state::{BrowserSegment, DockTab};
use crate::views::format_tag;

/// German texts of the browser (verbatim from `Screens.dc.html` and `ks-view.js`).
pub mod names {
    /// Placeholder and accessible name of the search field (`br()`: `search:'Suchen'`).
    pub const SEARCH: &str = "Suchen";
    /// Hint in the search field while it has text (`searchHint`).
    pub const SEARCH_CLEAR: &str = "Esc";
    /// Accessible name of the segmented control.
    pub const SEGMENTS: &str = "Browser-Filter";
    /// The segments (`segs`).
    pub const SEGMENT_LABELS: [&str; 4] = ["Alle", "Instr.", "Effekte", "Dateien"];
    /// Accessible name of the row list.
    pub const LIST: &str = "Browser-Liste";
    /// Section of the favourites.
    pub const FAVORITES: &str = "Favoriten";
    /// Section of the instruments.
    pub const INSTRUMENTS: &str = "Instrumente";
    /// Section of the effects.
    pub const EFFECTS: &str = "Effekte";
    /// Section of the MIDI effects (WP-88): they sit in front of the instrument.
    pub const MIDI_EFFECTS: &str = "MIDI-Effekte";
    /// Section of hexa's factory presets.
    pub const PRESETS: &str = "Presets · hexa";
    /// Section of the project's files (segment Alle).
    pub const PROJECT: &str = "Projekt";
    /// Folder and section of the recorded takes.
    pub const TAKES: &str = "Takes";
    /// Folder and section of the exports.
    pub const EXPORTS: &str = "Exporte";
    /// No takes yet (`browserView`).
    pub const NO_TAKES: &str = "Noch keine Aufnahmen";
    /// No exports yet (`browserView`).
    pub const NO_EXPORTS: &str = "Noch keine Exporte";
    /// Tag of a take row.
    pub const TAKE_TAG: &str = "MIDI";
    /// Tag of an export row (`browserView`: `plain(n,'WAV')`).
    pub const EXPORT_TAG: &str = "WAV";
    /// Footer without search (`br()` `foot`, S01).
    pub const FOOT: &str =
        "Auf eine Spur ziehen lädt das Instrument. Auf leere Fläche ziehen legt eine neue Spur an.";
    /// Footer while searching (S05 `foot`).
    pub const FOOT_SEARCH: &str =
        "Enter fügt den markierten Effekt an die ausgewählte Spur an. Stern: Favorit.";

    /// „3 Treffer für „hall““.
    pub fn hits(count: usize, query: &str) -> String {
        format!("{count} Treffer für „{query}“")
    }

    /// „Presets mit „hall““.
    pub fn presets_with(query: &str) -> String {
        format!("Presets mit „{query}“")
    }

    /// „Kategorie Hall und Delay“.
    pub fn category(label: &str) -> String {
        format!("Kategorie {label}")
    }

    /// A preset in search results: „Glas · hexa“.
    pub fn preset_hit(name: &str) -> String {
        format!("{name} · hexa")
    }

    /// A take: „Take 2 · Lead“.
    pub fn take(take: &str, track: &str) -> String {
        format!("{take} · {track}")
    }

    /// Accessible name of a row's star.
    pub fn star(name: &str) -> String {
        format!("Favorit: {name}")
    }
}

/// The effect categories with their browser labels, in browser order.
pub const CATEGORIES: [(EffectCategory, &str); 4] = [
    (EffectCategory::EqFilter, "EQ und Filter"),
    (EffectCategory::Dynamics, "Dynamik"),
    (EffectCategory::ReverbDelay, "Hall und Delay"),
    (EffectCategory::Modulation, "Modulation"),
];

/// What a row is.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A section header („FAVORITEN“).
    Section,
    /// Text without action: a category sub-header in the Effekte segment, „Noch keine Aufnahmen“.
    Plain,
    /// A plugin of the catalog.
    Plugin {
        /// Its reference (the drag payload).
        plugin: PluginRef,
        /// Instrument or effect.
        kind: PluginKind,
        /// Starred.
        favorite: bool,
    },
    /// A folder that opens and closes (a category, Takes, Exporte).
    Folder {
        /// Key in `BrowserView::collapsed`.
        key: String,
        /// Open.
        expanded: bool,
    },
    /// A factory preset of hexa (index in `komponist_builtin::presets`).
    Preset(usize),
    /// A recorded take.
    Take {
        /// The clip.
        clip: ClipId,
        /// Take index.
        take: usize,
    },
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// What it is.
    pub kind: RowKind,
    /// The text.
    pub name: String,
    /// The tag on the right: format ("CLAP"), count ("4"), "MIDI"; empty = none.
    pub tag: String,
    /// Folder depth (indent 12 per level).
    pub level: u8,
}

impl Row {
    fn new(kind: RowKind, name: impl Into<String>, tag: impl Into<String>, level: u8) -> Row {
        Row {
            kind,
            name: name.into(),
            tag: tag.into(),
            level,
        }
    }

    fn section(name: impl Into<String>) -> Row {
        Row::new(RowKind::Section, name, "", 0)
    }

    fn plain(name: impl Into<String>, level: u8) -> Row {
        Row::new(RowKind::Plain, name, "", level)
    }

    /// A key unique within its section (egui ids, tests): the same plugin name can come as CLAP
    /// and VST3 ("Dexed"), so plugins are keyed by format and id.
    pub fn key(&self) -> String {
        match &self.kind {
            RowKind::Plugin { plugin, .. } => format!("plugin:{:?}:{}", plugin.format, plugin.id),
            RowKind::Folder { key, .. } => format!("folder:{key}"),
            RowKind::Preset(i) => format!("preset:{i}"),
            RowKind::Take { clip, take } => format!("take:{}:{take}", clip.0),
            RowKind::Section | RowKind::Plain => format!("text:{}", self.name),
        }
    }

    /// Whether ↑ ↓ and a click can highlight it (everything but sections and plain text).
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Section | RowKind::Plain)
    }

    /// Whether Enter or a double-click does something with it (plugins, presets, takes).
    pub fn activatable(&self) -> bool {
        matches!(
            self.kind,
            RowKind::Plugin { .. } | RowKind::Preset(_) | RowKind::Take { .. }
        )
    }
}

/// What the rows are built from.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The plugins.
    pub catalog: &'a Catalog,
    /// Starred plugins.
    pub favorites: &'a Favorites,
    /// Segment.
    pub segment: BrowserSegment,
    /// Search text (trimmed here).
    pub query: &'a str,
    /// Collapsed folder keys.
    pub collapsed: &'a BTreeSet<String>,
    /// The project (takes).
    pub project: &'a Project,
    /// The file names of this session's exports, oldest first.
    pub exports: &'a [String],
}

/// The file names of `exports` for [`Input::exports`].
pub fn export_names(exports: &[std::path::PathBuf]) -> Vec<String> {
    exports
        .iter()
        .map(|p| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |f| f.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// The folder key of `label` in `section`.
pub fn folder_key(section: &str, label: &str) -> String {
    format!("{section}/{label}")
}

/// Every folder key, collapsed when the browser first shows (S01: all folders ▸).
pub fn default_collapsed() -> Vec<String> {
    let mut keys: Vec<String> = CATEGORIES
        .iter()
        .map(|(_, label)| folder_key(names::EFFECTS, label))
        .collect();
    keys.extend(komponist_builtin::categories().map(|c| folder_key(names::PRESETS, c)));
    keys.push(folder_key(names::PROJECT, names::TAKES));
    keys.push(folder_key(names::PROJECT, names::EXPORTS));
    keys
}

/// The browser label of an effect category, `None` for uncategorised effects.
pub fn category_label(category: EffectCategory) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find(|(c, _)| *c == category)
        .map(|(_, label)| *label)
}

/// The rows for `input` (module docs).
pub fn rows(input: &Input<'_>) -> Vec<Row> {
    let query = input.query.trim();
    if input.segment == BrowserSegment::Files {
        files(input, query)
    } else if query.is_empty() {
        browse(input)
    } else {
        search(input, query)
    }
}

fn plugin_row(input: &Input<'_>, entry: &CatalogEntry, level: u8) -> Row {
    Row::new(
        RowKind::Plugin {
            plugin: entry.plugin.clone(),
            kind: entry.kind,
            favorite: input.favorites.contains(&entry.plugin),
        },
        entry.plugin.name.clone(),
        format_tag(entry.plugin.format),
        level,
    )
}

fn folder(
    input: &Input<'_>,
    section: &str,
    label: &str,
    count: usize,
    rows: &mut Vec<Row>,
) -> bool {
    let key = folder_key(section, label);
    let expanded = !input.collapsed.contains(&key);
    rows.push(Row::new(
        RowKind::Folder { key, expanded },
        label,
        count.to_string(),
        0,
    ));
    expanded
}

fn is_instrument(e: &CatalogEntry) -> bool {
    e.kind == PluginKind::Instrument
}

fn is_effect(e: &CatalogEntry) -> bool {
    e.kind == PluginKind::Effect
}

fn is_midi_effect(e: &CatalogEntry) -> bool {
    e.kind == PluginKind::MidiEffect
}

fn in_category(e: &CatalogEntry, category: EffectCategory) -> bool {
    is_effect(e) && e.category.unwrap_or(EffectCategory::Other) == category
}

fn browse(input: &Input<'_>) -> Vec<Row> {
    let entries = input.catalog.entries();
    let all = input.segment == BrowserSegment::All;
    let mut rows = Vec::new();
    if all {
        let favorites: Vec<&CatalogEntry> = entries
            .iter()
            .filter(|e| input.favorites.contains(&e.plugin))
            .collect();
        if !favorites.is_empty() {
            rows.push(Row::section(names::FAVORITES));
            rows.extend(favorites.iter().map(|e| plugin_row(input, e, 0)));
        }
    }
    if all || input.segment == BrowserSegment::Instruments {
        let instruments: Vec<&CatalogEntry> = entries.iter().filter(|e| is_instrument(e)).collect();
        if !instruments.is_empty() {
            rows.push(Row::section(names::INSTRUMENTS));
            rows.extend(instruments.iter().map(|e| plugin_row(input, e, 0)));
        }
    }
    if all || input.segment == BrowserSegment::Effects {
        effects(input, entries, all, &mut rows);
        let midi: Vec<&CatalogEntry> = entries.iter().filter(|e| is_midi_effect(e)).collect();
        if !midi.is_empty() {
            rows.push(Row::section(names::MIDI_EFFECTS));
            rows.extend(midi.iter().map(|e| plugin_row(input, e, 0)));
        }
    }
    if all || input.segment == BrowserSegment::Instruments {
        presets(input, &mut rows);
    }
    if all {
        rows.push(Row::section(names::PROJECT));
        let takes = takes(input.project, "");
        if folder(input, names::PROJECT, names::TAKES, takes.len(), &mut rows) {
            push_or_empty(&mut rows, takes, names::NO_TAKES, 1);
        }
        let exports = exports(input, "");
        if folder(
            input,
            names::PROJECT,
            names::EXPORTS,
            exports.len(),
            &mut rows,
        ) {
            push_or_empty(&mut rows, exports, names::NO_EXPORTS, 1);
        }
    }
    rows
}

/// The effects (audio effects; the MIDI effects follow in their own section): category folders (Alle) or sub-headers (Effekte), then the uncategorised ones.
fn effects(input: &Input<'_>, entries: &[CatalogEntry], folders: bool, rows: &mut Vec<Row>) {
    if !entries.iter().any(is_effect) {
        return;
    }
    rows.push(Row::section(names::EFFECTS));
    for (category, label) in CATEGORIES {
        let members: Vec<&CatalogEntry> = entries
            .iter()
            .filter(|e| in_category(e, category))
            .collect();
        if members.is_empty() {
            continue;
        }
        if folders {
            if folder(input, names::EFFECTS, label, members.len(), rows) {
                rows.extend(members.iter().map(|e| plugin_row(input, e, 1)));
            }
        } else {
            rows.push(Row::plain(label, 0));
            rows.extend(members.iter().map(|e| plugin_row(input, e, 0)));
        }
    }
    rows.extend(
        entries
            .iter()
            .filter(|e| in_category(e, EffectCategory::Other))
            .map(|e| plugin_row(input, e, 0)),
    );
}

/// hexa's factory presets in category folders, if hexa is in the catalog.
fn presets(input: &Input<'_>, rows: &mut Vec<Row>) {
    let has_hexa = input
        .catalog
        .entries()
        .iter()
        .any(|e| e.plugin.id == komponist_model::project::BUILTIN_HEXA);
    if !has_hexa {
        return;
    }
    rows.push(Row::section(names::PRESETS));
    let all: Vec<komponist_builtin::PresetInfo> = komponist_builtin::presets().collect();
    for category in komponist_builtin::categories() {
        let members: Vec<&komponist_builtin::PresetInfo> =
            all.iter().filter(|p| p.category == category).collect();
        if folder(input, names::PRESETS, category, members.len(), rows) {
            rows.extend(
                members
                    .iter()
                    .map(|p| Row::new(RowKind::Preset(p.index), p.name, "", 1)),
            );
        }
    }
}

fn push_or_empty(rows: &mut Vec<Row>, items: Vec<Row>, empty: &str, level: u8) {
    if items.is_empty() {
        rows.push(Row::plain(empty, level));
    } else {
        rows.extend(items.into_iter().map(|r| Row { level, ..r }));
    }
}

/// The named takes of the project whose text contains `query` (lower case), in track order.
fn takes(project: &Project, query: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for track in &project.tracks {
        for clip in &track.clips {
            let ClipContent::Midi(midi) = &clip.content else {
                continue;
            };
            for (index, take) in midi.takes.iter().enumerate() {
                if take.name.is_empty() {
                    continue;
                }
                let name = names::take(&take.name, &track.name);
                if name.to_lowercase().contains(query) {
                    rows.push(Row::new(
                        RowKind::Take {
                            clip: clip.id,
                            take: index,
                        },
                        name,
                        names::TAKE_TAG,
                        0,
                    ));
                }
            }
        }
    }
    rows
}

fn files(input: &Input<'_>, query: &str) -> Vec<Row> {
    let mut rows = vec![Row::section(names::TAKES)];
    push_or_empty(
        &mut rows,
        takes(input.project, &query.to_lowercase()),
        names::NO_TAKES,
        0,
    );
    rows.push(Row::section(names::EXPORTS));
    push_or_empty(
        &mut rows,
        exports(input, &query.to_lowercase()),
        names::NO_EXPORTS,
        0,
    );
    rows
}

/// The exports whose name contains `query` (lower case), oldest first.
fn exports(input: &Input<'_>, query: &str) -> Vec<Row> {
    input
        .exports
        .iter()
        .filter(|name| name.to_lowercase().contains(query))
        .map(|name| Row::new(RowKind::Plain, name.as_str(), names::EXPORT_TAG, 0))
        .collect()
}

fn search(input: &Input<'_>, query: &str) -> Vec<Row> {
    let segment_has = |e: &CatalogEntry| match input.segment {
        BrowserSegment::Instruments => is_instrument(e),
        BrowserSegment::Effects => is_effect(e) || is_midi_effect(e),
        _ => true,
    };
    let hits: Vec<&CatalogEntry> = input
        .catalog
        .search(query)
        .into_iter()
        .filter(|e| segment_has(e))
        .collect();
    let mut rows = vec![Row::section(names::hits(hits.len(), query))];
    rows.extend(hits.iter().map(|e| plugin_row(input, e, 0)));

    let lower = query.to_lowercase();
    if input.segment != BrowserSegment::Effects {
        let presets: Vec<Row> = komponist_builtin::presets()
            .filter(|p| p.name.to_lowercase().contains(&lower))
            .map(|p| Row::new(RowKind::Preset(p.index), names::preset_hit(p.name), "", 0))
            .collect();
        if !presets.is_empty() {
            rows.push(Row::section(names::presets_with(query)));
            rows.extend(presets);
        }
    }
    if input.segment != BrowserSegment::Instruments {
        for (category, label) in CATEGORIES {
            if !label.to_lowercase().contains(&lower) {
                continue;
            }
            let more: Vec<Row> = input
                .catalog
                .entries()
                .iter()
                .filter(|e| in_category(e, category))
                .filter(|e| !hits.iter().any(|h| h.plugin == e.plugin))
                .map(|e| plugin_row(input, e, 0))
                .collect();
            if !more.is_empty() {
                rows.push(Row::section(names::category(label)));
                rows.extend(more);
            }
        }
    }
    rows
}

/// The rows of the chain's add menu: the catalog's instruments, effects (category sub-headers)
/// and MIDI effects as the Effekte and Instr. segments list them, or with a search text the
/// hits of the Alle segment ([`search`], so the matching is the browser's). Presets, folders,
/// takes and files are left out; sections without a plugin vanish. `effects_only` keeps the audio
/// effects (the master chain takes nothing else).
pub fn picker_rows(input: &Input<'_>, effects_only: bool) -> Vec<Row> {
    let none = BTreeSet::new();
    let with = |segment| {
        rows(&Input {
            segment,
            collapsed: &none,
            ..*input
        })
    };
    let all = if input.query.trim().is_empty() {
        let mut all = with(BrowserSegment::Instruments);
        all.extend(with(BrowserSegment::Effects));
        all
    } else {
        with(BrowserSegment::All)
    };
    let is_plugin = |r: &Row| matches!(r.kind, RowKind::Plugin { .. });
    let wanted = |r: &Row| match &r.kind {
        RowKind::Plugin { kind, .. } => !effects_only || *kind == PluginKind::Effect,
        RowKind::Section | RowKind::Plain => true,
        _ => false,
    };
    let kept: Vec<Row> = all.into_iter().filter(wanted).collect();
    // Per section (header up to the next header): keep it when a plugin is in it.
    let mut out = Vec::new();
    let mut start = 0;
    while start < kept.len() {
        let end = kept[start + 1..]
            .iter()
            .position(|r| r.kind == RowKind::Section)
            .map_or(kept.len(), |n| start + 1 + n);
        if kept[start..end].iter().any(is_plugin) {
            out.extend_from_slice(&kept[start..end]);
        }
        start = end;
    }
    out
}

/// The instruments of the catalog for the new-track dialog: the plugin rows of [`picker_rows`]
/// that are instruments (so the search is the browser's), each plugin once, no section headers.
pub fn instrument_rows(input: &Input<'_>) -> Vec<Row> {
    let mut seen: Vec<PluginRef> = Vec::new();
    picker_rows(input, false)
        .into_iter()
        .filter(|r| {
            let RowKind::Plugin { plugin, kind, .. } = &r.kind else {
                return false;
            };
            if *kind != PluginKind::Instrument || seen.contains(plugin) {
                return false;
            }
            seen.push(plugin.clone());
            true
        })
        .collect()
}

/// The row Enter acts on: the highlighted one, or while searching the first activatable row
/// (S05: the first hit is marked, so typing and Enter add it).
pub fn effective_highlight(rows: &[Row], highlighted: Option<usize>, query: &str) -> Option<usize> {
    match highlighted {
        Some(i) if rows.get(i).is_some_and(Row::selectable) => Some(i),
        _ if !query.trim().is_empty() => rows.iter().position(Row::activatable),
        _ => None,
    }
}

/// The track Enter and double-click act on: the first selected track in display order.
pub fn selected_track(project: &Project, selected: &BTreeSet<TrackId>) -> Option<TrackId> {
    project
        .tracks
        .iter()
        .find(|t| selected.contains(&t.id))
        .map(|t| t.id)
}

/// What a double-click or Enter on `row` does (prototype `brActivate`): an instrument gets a new
/// track; an effect goes onto the selected track and the dock shows its chain (prototype
/// `addEffect`), without a selected track the dispatcher says that effects need one; a preset
/// loads into the selected track's hexa; a take opens its clip with that take active.
pub fn activate(row: &Row, project: &Project, track: Option<TrackId>) -> Vec<Action> {
    match &row.kind {
        RowKind::Plugin {
            plugin,
            kind: PluginKind::Instrument,
            ..
        } => vec![Action::Plugin(PluginAction::Add {
            plugin: plugin.clone(),
            target: DropTarget::NewTrack { index: None },
        })],
        RowKind::Plugin { plugin, .. } => match track {
            Some(t) => vec![
                Action::Plugin(PluginAction::Add {
                    plugin: plugin.clone(),
                    target: DropTarget::Track(t),
                }),
                Action::Ui(UiAction::SetDockTab(DockTab::Devices)),
            ],
            None => vec![Action::Plugin(PluginAction::Add {
                plugin: plugin.clone(),
                target: DropTarget::NewTrack { index: None },
            })],
        },
        RowKind::Preset(preset) => vec![Action::Plugin(match hexa_of(project, track) {
            Some(device) => PluginAction::ApplyHexaPreset {
                device,
                preset: *preset,
            },
            None => PluginAction::NewHexaTrack { preset: *preset },
        })],
        RowKind::Take { clip, take } => vec![
            Action::Edit(EditAction::SelectTake {
                clip: *clip,
                take: *take,
            }),
            Action::Ui(UiAction::OpenClip(*clip)),
        ],
        RowKind::Section | RowKind::Plain | RowKind::Folder { .. } => Vec::new(),
    }
}

/// The hexa instrument of `track`, if it has one.
pub fn hexa_of(project: &Project, track: Option<TrackId>) -> Option<komponist_model::DeviceId> {
    let track = project.track(track?)?;
    track
        .channel
        .devices
        .iter()
        .find(|d| d.role == DeviceRole::Instrument && is_hexa(d))
        .map(|d| d.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::{PluginDescriptor, PluginFormat as BundleFormat};
    use komponist_model::fixtures::demo_project;

    fn descriptor(id: &str, name: &str, kind: PluginKind, features: &[&str]) -> PluginDescriptor {
        PluginDescriptor {
            format: BundleFormat::Clap,
            id: id.into(),
            name: name.into(),
            vendor: "Test".into(),
            version: "1.0".into(),
            kind,
            features: features.iter().map(|f| (*f).into()).collect(),
            bundle: format!("/plugins/{id}.clap").into(),
        }
    }

    /// hexa, the sampler and the plugins of S01/S05.
    fn catalog() -> Catalog {
        let mut c = Catalog::builtin();
        c.set_scanned(&[
            descriptor(
                "surge",
                "Surge XT",
                PluginKind::Instrument,
                &["synthesizer"],
            ),
            descriptor("eq", "LSP Para EQ x8", PluginKind::Effect, &["equalizer"]),
            descriptor(
                "comp",
                "LSP Compressor",
                PluginKind::Effect,
                &["compressor"],
            ),
            descriptor(
                "hall",
                "Dragonfly Hall Reverb",
                PluginKind::Effect,
                &["reverb"],
            ),
            descriptor(
                "plate",
                "Dragonfly Plate Reverb",
                PluginKind::Effect,
                &["reverb"],
            ),
            descriptor("delay", "LSP Delay", PluginKind::Effect, &["delay"]),
            descriptor("chorus", "TAL-Chorus-LX", PluginKind::Effect, &["chorus"]),
            descriptor("util", "kp-test-fx", PluginKind::Effect, &["utility"]),
        ]);
        c
    }

    fn names_of(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn input<'a>(
        catalog: &'a Catalog,
        favorites: &'a Favorites,
        collapsed: &'a BTreeSet<String>,
        project: &'a Project,
        segment: BrowserSegment,
        query: &'a str,
    ) -> Input<'a> {
        Input {
            catalog,
            favorites,
            segment,
            query,
            collapsed,
            project,
            exports: &[],
        }
    }

    #[test]
    fn all_lists_sections_like_s01() {
        let (c, f, p) = (catalog(), Favorites::default(), demo_project());
        let collapsed: BTreeSet<String> = default_collapsed().into_iter().collect();
        let rows = rows(&input(&c, &f, &collapsed, &p, BrowserSegment::All, ""));
        let sections: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == RowKind::Section)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            sections,
            [
                "Favoriten",
                "Instrumente",
                "Effekte",
                "MIDI-Effekte",
                "Presets · hexa",
                "Projekt"
            ]
        );
        let n = names_of(&rows);
        assert_eq!(
            &n[..8],
            [
                "Favoriten",
                "hexa",
                "Sampler",
                "Instrumente",
                "hexa",
                "Sampler",
                "volt",
                "Surge XT"
            ]
        );
        assert_eq!(rows[1].tag, "INTERN");
        // Category folders with counts, collapsed; the uncategorised effect after them.
        let eq = rows.iter().find(|r| r.name == "EQ und Filter").unwrap();
        assert_eq!(eq.tag, "2", "the internal EQ and LSP Para EQ");
        assert!(matches!(
            eq.kind,
            RowKind::Folder {
                expanded: false,
                ..
            }
        ));
        let hall = rows.iter().find(|r| r.name == "Hall und Delay").unwrap();
        assert_eq!(hall.tag, "5", "Delay and Hall (INTERN) and three scanned");
        assert!(n.contains(&"kp-test-fx"));
        assert!(!n.contains(&"Dragonfly Hall Reverb"));
        // Projekt: Takes and Exporte folders.
        assert_eq!(&n[n.len() - 2..], ["Takes", "Exporte"]);
        assert!(n.contains(&"Tasten"), "hexa categories as folders");
    }

    #[test]
    fn open_folders_show_their_members_indented() {
        let (c, f, p) = (catalog(), Favorites::default(), demo_project());
        let mut collapsed: BTreeSet<String> = default_collapsed().into_iter().collect();
        collapsed.remove(&folder_key(names::EFFECTS, "Hall und Delay"));
        collapsed.remove(&folder_key(names::PROJECT, names::EXPORTS));
        let rows = rows(&input(&c, &f, &collapsed, &p, BrowserSegment::All, ""));
        let i = rows
            .iter()
            .position(|r| r.name == "Hall und Delay")
            .unwrap();
        assert_eq!(
            names_of(&rows[i + 1..i + 6]),
            [
                "Delay",
                "Hall",
                "Dragonfly Hall Reverb",
                "Dragonfly Plate Reverb",
                "LSP Delay"
            ]
        );
        assert!(rows[i + 1..i + 6].iter().all(|r| r.level == 1));
        assert_eq!(rows[i + 1].tag, "INTERN");
        assert_eq!(rows.last().unwrap().name, names::NO_EXPORTS);
    }

    #[test]
    fn segments_filter() {
        let (c, f, p) = (catalog(), Favorites::default(), demo_project());
        let collapsed = BTreeSet::new();
        let inst = rows(&input(
            &c,
            &f,
            &collapsed,
            &p,
            BrowserSegment::Instruments,
            "",
        ));
        assert!(inst.iter().all(|r| !matches!(
            r.kind,
            RowKind::Plugin {
                kind: PluginKind::Effect,
                ..
            }
        )));
        assert_eq!(inst[0].name, "Instrumente");
        let fx = rows(&input(&c, &f, &collapsed, &p, BrowserSegment::Effects, ""));
        assert_eq!(
            names_of(&fx[..5]),
            [
                "Effekte",
                "EQ und Filter",
                "EQ",
                "LSP Para EQ x8",
                "Dynamik"
            ]
        );
        assert_eq!(fx[1].kind, RowKind::Plain);
        assert!(!fx.iter().any(|r| r.name == names::PRESETS));
        let files = rows(&input(&c, &f, &collapsed, &p, BrowserSegment::Files, ""));
        assert_eq!(
            names_of(&files),
            [
                "Takes",
                "Noch keine Aufnahmen",
                "Exporte",
                "Noch keine Exporte"
            ]
        );
    }

    #[test]
    fn exports_are_listed_with_their_tag_and_filtered() {
        let (c, f, p) = (catalog(), Favorites::default(), demo_project());
        let collapsed = BTreeSet::new();
        let paths = [
            std::path::PathBuf::from("/out/Nachtfahrt.wav"),
            std::path::PathBuf::from("/out/Loop.wav"),
        ];
        let exports = export_names(&paths);
        assert_eq!(exports, ["Nachtfahrt.wav", "Loop.wav"]);
        let with = |segment, query| {
            rows(&Input {
                exports: &exports,
                ..input(&c, &f, &collapsed, &p, segment, query)
            })
        };
        let files = with(BrowserSegment::Files, "");
        assert_eq!(
            names_of(&files[2..]),
            ["Exporte", "Nachtfahrt.wav", "Loop.wav"]
        );
        assert_eq!(files[3].tag, names::EXPORT_TAG);
        assert_eq!(files[3].kind, RowKind::Plain);
        let hits = with(BrowserSegment::Files, "loop");
        assert_eq!(names_of(&hits[2..]), ["Exporte", "Loop.wav"]);
        // Alle: the folder counts them and lists them inside.
        let all = with(BrowserSegment::All, "");
        let folder = all.iter().position(|r| r.name == "Exporte").unwrap();
        assert_eq!(all[folder].tag, "2");
        assert_eq!(all[folder + 1].name, "Nachtfahrt.wav");
        assert_eq!(all[folder + 1].level, 1);
    }

    #[test]
    fn search_is_built_like_s05() {
        let (c, f, p) = (catalog(), Favorites::default(), demo_project());
        let collapsed = BTreeSet::new();
        let rows = rows(&input(
            &c,
            &f,
            &collapsed,
            &p,
            BrowserSegment::Effects,
            "hall",
        ));
        assert_eq!(rows[0].name, "2 Treffer für „hall“");
        assert_eq!(names_of(&rows[1..3]), ["Hall", "Dragonfly Hall Reverb"]);
        let k = rows
            .iter()
            .position(|r| r.name == "Kategorie Hall und Delay")
            .unwrap();
        assert_eq!(
            names_of(&rows[k + 1..]),
            ["Delay", "Dragonfly Plate Reverb", "LSP Delay"]
        );
        assert_eq!(effective_highlight(&rows, None, "hall"), Some(1));
        assert_eq!(effective_highlight(&rows, Some(k + 1), "hall"), Some(k + 1));
        assert_eq!(effective_highlight(&rows, Some(0), ""), None, "a section");
        // Instruments: no categories, but presets.
        let rows = rows_for(&c, &f, &p, BrowserSegment::All, "glocke");
        assert_eq!(rows[0].name, "0 Treffer für „glocke“");
        assert!(rows.iter().any(|r| r.name == "Presets mit „glocke“"));
        assert!(rows.iter().any(|r| r.name.ends_with(" · hexa")));
    }

    fn rows_for(
        c: &Catalog,
        f: &Favorites,
        p: &Project,
        segment: BrowserSegment,
        q: &str,
    ) -> Vec<Row> {
        let collapsed = BTreeSet::new();
        rows(&input(c, f, &collapsed, p, segment, q))
    }

    #[test]
    fn row_keys_are_unique_within_a_section() {
        let mut c = catalog();
        let mut vst3 = descriptor("surge-vst3", "Surge XT", PluginKind::Instrument, &[]);
        vst3.format = BundleFormat::Vst3;
        let mut descriptors = vec![vst3];
        descriptors.push(descriptor("surge", "Surge XT", PluginKind::Instrument, &[]));
        c.set_scanned(&descriptors);
        let p = demo_project();
        let collapsed = BTreeSet::new();
        let f = Favorites::default();
        let rows = rows(&input(&c, &f, &collapsed, &p, BrowserSegment::All, ""));
        let mut seen = std::collections::HashSet::new();
        let mut section = String::new();
        for r in &rows {
            if r.kind == RowKind::Section {
                section = r.name.clone();
            }
            assert!(
                seen.insert((section.clone(), r.key())),
                "{section} {}",
                r.name
            );
        }
        assert_eq!(rows.iter().filter(|r| r.name == "Surge XT").count(), 2);
    }

    #[test]
    fn favourites_follow_the_settings() {
        let (c, p) = (catalog(), demo_project());
        let mut f = Favorites(Vec::new());
        let rows = rows_for(&c, &f, &p, BrowserSegment::All, "");
        assert_eq!(rows[0].name, "Instrumente", "no favourites, no section");
        let hall = c
            .entries()
            .iter()
            .find(|e| e.plugin.id == "hall" && !e.is_builtin())
            .unwrap();
        f.toggle(&hall.plugin);
        let rows = rows_for(&c, &f, &p, BrowserSegment::All, "");
        assert_eq!(names_of(&rows[..2]), ["Favoriten", "Dragonfly Hall Reverb"]);
        assert!(matches!(
            rows[1].kind,
            RowKind::Plugin { favorite: true, .. }
        ));
    }

    #[test]
    fn activating_rows() {
        let c = catalog();
        let p = demo_project();
        let lead = p.tracks[2].id;
        let entry = |id: &str| c.entries().iter().find(|e| e.plugin.id == id).unwrap();
        let f = Favorites::default();
        let collapsed = BTreeSet::new();
        let i = input(&c, &f, &collapsed, &p, BrowserSegment::All, "");
        let surge = plugin_row(&i, entry("surge"), 0);
        assert_eq!(
            activate(&surge, &p, Some(lead)),
            [Action::Plugin(PluginAction::Add {
                plugin: entry("surge").plugin.clone(),
                target: DropTarget::NewTrack { index: None }
            })]
        );
        let hall = plugin_row(&i, entry("hall"), 0);
        assert_eq!(
            activate(&hall, &p, Some(lead)),
            [
                Action::Plugin(PluginAction::Add {
                    plugin: entry("hall").plugin.clone(),
                    target: DropTarget::Track(lead)
                }),
                Action::Ui(UiAction::SetDockTab(DockTab::Devices)),
            ]
        );
        assert_eq!(
            activate(&hall, &p, None),
            [Action::Plugin(PluginAction::Add {
                plugin: entry("hall").plugin.clone(),
                target: DropTarget::NewTrack { index: None }
            })]
        );
        // Lead plays hexa; Pad plays Surge XT (no hexa there).
        let preset = Row::new(RowKind::Preset(3), "x", "", 1);
        let device = p.tracks[2].channel.devices[0].id;
        assert_eq!(
            activate(&preset, &p, Some(lead)),
            [Action::Plugin(PluginAction::ApplyHexaPreset {
                device,
                preset: 3
            })]
        );
        // No hexa on the selected track (or no track): a new hexa track with the preset.
        let new = [Action::Plugin(PluginAction::NewHexaTrack { preset: 3 })];
        assert_eq!(activate(&preset, &p, Some(p.tracks[3].id)), new);
        assert_eq!(activate(&preset, &p, None), new);
        let mut sel = BTreeSet::new();
        sel.insert(p.tracks[4].id);
        sel.insert(p.tracks[1].id);
        assert_eq!(selected_track(&p, &sel), Some(p.tracks[1].id));
    }
}
