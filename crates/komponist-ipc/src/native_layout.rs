//! Owned metadata for the native CLAP device-layout contract. Used on control threads only.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::control::{ParamInfo, param_flags};

pub const EXTENSION_V1: &CStr = c"org.klang-studio.komponist.device-layout/1";
pub const EXTENSION_V2: &CStr = c"org.klang-studio.komponist.device-layout/2";
/// The maximum JSON document size, in bytes.
pub const MAX_BYTES: u32 = 65_536;
/// The maximum number of optional author IDs in a layout.
pub const MAX_AUTHOR_IDS: usize = 256;

/// Whether a key fits the v1 optional author-ID map.
pub fn valid_author_id(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 128 && !key.chars().any(char::is_control)
}

/// A descriptor rejected by the host. Diagnostics contain only host-defined text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// A malformed v1 descriptor or transfer.
    Invalid(&'static str),
    /// An unsupported schema or required capability.
    Unsupported,
}
impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid native CLAP layout: {reason}"),
            Self::Unsupported => {
                f.write_str("unsupported native CLAP layout version or capability")
            }
        }
    }
}

/// Localized labels keyed by a document locale tag.
pub type Labels = BTreeMap<String, String>;

/// Display metadata from a validated layout, carried with a scanned descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct LocalizedPlugin {
    pub default_locale: String,
    pub names: Labels,
    pub descriptions: Labels,
}

impl LocalizedPlugin {
    pub fn name(&self, language: &str) -> Option<&str> {
        localized_label(&self.names, language, &self.default_locale)
    }

    pub fn description(&self, language: &str) -> Option<&str> {
        localized_label(&self.descriptions, language, &self.default_locale)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Metadata {
    names: Labels,
    #[serde(default)]
    descriptions: Labels,
}

/// The supported controls and display widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Widget {
    Knob,
    KnobLarge,
    Number,
    ToggleButton,
    Segment,
    Dropdown,
    MiniFader,
    Graph,
    Canvas,
}

/// The localized presentation of a display-only canvas.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Canvas {
    pub key: String,
    pub name: Labels,
    pub description: Labels,
    /// Paint the localized canvas name in the same style as a graph title.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub show_title: bool,
    /// Allow the host to zoom and pan the display along its horizontal axis.
    #[serde(default)]
    pub horizontal_viewport: bool,
}

/// The host's declarative graph configurations available to plugins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphKind {
    Envelope,
    Eq,
    Filter,
    Transfer,
    Waveform,
    Xy,
    Lfo,
    Compressor,
    GrHistory,
}

/// Localized presentation and numeric CLAP parameter IDs for one graph.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Graph {
    pub kind: GraphKind,
    pub name: Labels,
    pub description: Labels,
    /// Keep the accessible name but omit the painted title.
    #[serde(default)]
    pub hide_title: bool,
    #[serde(default)]
    pub slots: BTreeMap<String, u32>,
    pub bands: Option<u8>,
    pub points: Option<u8>,
}

impl Graph {
    fn has_slot(&self, slot: &str) -> bool {
        match self.kind {
            GraphKind::Envelope => {
                matches!(slot, "attack" | "decay" | "sustain" | "release" | "curve")
            }
            GraphKind::Eq => slot.split_once('_').is_some_and(|(band, suffix)| {
                let Some(band) = band.strip_prefix('b') else {
                    return false;
                };
                let Ok(index) = band.parse::<u8>() else {
                    return false;
                };
                band == index.to_string()
                    && (1..=self.bands.unwrap_or_default()).contains(&index)
                    && matches!(suffix, "freq" | "gain" | "q" | "type" | "on")
            }),
            GraphKind::Filter => matches!(slot, "cutoff" | "resonance" | "type"),
            GraphKind::Transfer | GraphKind::Compressor => {
                matches!(slot, "threshold" | "ratio" | "knee")
            }
            GraphKind::Waveform => matches!(slot, "start" | "end"),
            GraphKind::Xy => matches!(slot, "x" | "y"),
            GraphKind::Lfo => slot.strip_prefix(['x', 'y']).is_some_and(|point| {
                (1..=self.points.unwrap_or_default()).any(|index| point == index.to_string())
            }),
            GraphKind::GrHistory => false,
        }
    }
}

/// The occupied half of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Half {
    Top,
    Bottom,
}

/// Text for one plain CLAP enumeration value.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Choice {
    pub value: i32,
    pub labels: Labels,
}

/// A single parameter binding and its position.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Cell {
    pub widget: Widget,
    pub col: u8,
    pub row: u8,
    pub cols: u8,
    pub rows: u8,
    pub half: Option<Half>,
    pub param_id: Option<u32>,
    pub labels: Option<Labels>,
    pub choices: Option<Vec<Choice>>,
    pub graph: Option<Graph>,
    pub canvas: Option<Canvas>,
}

impl Cell {
    /// Every numeric parameter referenced by this cell.
    pub fn parameter_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.param_id.into_iter().chain(
            self.graph
                .iter()
                .flat_map(|graph| graph.slots.values().copied()),
        )
    }
}

/// A page of native parameter controls.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Page {
    pub key: String,
    pub labels: Labels,
    pub columns: u8,
    pub cells: Vec<Cell>,
    /// V2 separators identified by the zero-based column before each gap.
    #[serde(default)]
    pub dividers: Vec<u8>,
    /// Read-only labels in an otherwise empty top half of the control grid.
    #[serde(default)]
    pub headings: Vec<Heading>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Heading {
    pub col: u8,
    pub row: u8,
    pub cols: u8,
    pub labels: Labels,
}

/// A parsed native descriptor. The raw IPC payload is bounded and parsed independently on
/// both sides before use. Parameter bindings must also pass `validate_params` for the inventory.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NativeLayout {
    pub schema_version: u8,
    pub default_locale: String,
    locales: Vec<String>,
    pub pages: Vec<Page>,
    #[serde(default)]
    author_ids: Option<BTreeMap<String, u32>>,
    metadata: Option<Metadata>,
}

fn check(condition: bool, reason: &'static str) -> Result<(), LayoutError> {
    if condition {
        Ok(())
    } else {
        Err(LayoutError::Invalid(reason))
    }
}

impl NativeLayout {
    /// Decode one bounded document, rejecting duplicate object keys even in optional metadata.
    pub fn parse(bytes: &[u8]) -> Result<Self, LayoutError> {
        check(
            !bytes.is_empty() && bytes.len() <= MAX_BYTES as usize,
            "document size",
        )?;
        let Unique(mut value) = serde_json::from_slice(bytes)
            .map_err(|_| LayoutError::Invalid("JSON syntax or duplicate keys"))?;
        let version = value["schema_version"]
            .as_i64()
            .ok_or(LayoutError::Invalid("schema version"))?;
        if !matches!(version, 1 | 2) {
            return Err(LayoutError::Unsupported);
        }
        let metadata_v1 = if version == 1 {
            value
                .as_object_mut()
                .and_then(|root| root.remove("metadata"))
        } else {
            None
        };
        if let Some(metadata) = value.get("metadata") {
            check(metadata_object(metadata), "plugin metadata")?;
        }
        if let Some(caps) = value.get("required_capabilities") {
            let caps = caps
                .as_array()
                .ok_or(LayoutError::Invalid("capability list"))?;
            let mut seen = BTreeSet::new();
            check(
                caps.len() <= 8
                    && caps.iter().all(|cap| {
                        cap.as_str()
                            .is_some_and(|cap| identifier(cap, true) && seen.insert(cap))
                    }),
                "capability list",
            )?;
            if !seen.is_empty() {
                return Err(LayoutError::Unsupported);
            }
        }
        // Serde also accepts positional structs and externally tagged enum objects. V1
        // requires named objects and string tags, while allowing unknown object members.
        if let Some(pages) = value.get_mut("pages").and_then(Value::as_array_mut) {
            for page in pages {
                let page = page
                    .as_object_mut()
                    .ok_or(LayoutError::Invalid("page object"))?;
                if version == 1 {
                    page.remove("dividers");
                    page.remove("headings");
                }
                if let Some(headings) = page.get("headings") {
                    check(
                        headings.as_array().is_some_and(|items| {
                            items.len() <= 14 && items.iter().all(Value::is_object)
                        }),
                        "page headings",
                    )?;
                }
                if let Some(cells) = page.get_mut("cells").and_then(Value::as_array_mut) {
                    for cell in cells {
                        let cell = cell
                            .as_object_mut()
                            .ok_or(LayoutError::Invalid("cell object"))?;
                        if version == 1 {
                            // This was an unknown object member in v1.
                            cell.remove("graph");
                            cell.remove("canvas");
                        }
                        let widget = cell
                            .get("widget")
                            .and_then(Value::as_str)
                            .ok_or(LayoutError::Invalid("widget tag"))?;
                        check(
                            if version == 2 && matches!(widget, "graph" | "canvas") {
                                !cell.contains_key("param_id")
                            } else {
                                cell.get("param_id").is_some_and(Value::is_u64)
                            },
                            "parameter ID",
                        )?;
                        if let Some(graph) = cell.get("graph") {
                            check(
                                graph.is_object()
                                    && graph["kind"].is_string()
                                    && graph["name"].is_object()
                                    && graph["description"].is_object()
                                    && graph.get("slots").is_none_or(Value::is_object),
                                "graph object",
                            )?;
                        }
                        if let Some(canvas) = cell.get("canvas") {
                            check(
                                canvas.is_object()
                                    && canvas["name"].is_object()
                                    && canvas["description"].is_object(),
                                "canvas object",
                            )?;
                        }
                        check(cell.get("half").is_none_or(Value::is_string), "half tag")?;
                        check(
                            cell.get("labels").is_none_or(Value::is_object),
                            "cell labels",
                        )?;
                        if let Some(choices) = cell.get("choices") {
                            check(
                                choices
                                    .as_array()
                                    .is_some_and(|choices| choices.iter().all(Value::is_object)),
                                "choice objects",
                            )?;
                        }
                    }
                }
            }
        }
        if let Some(author_ids) = value.get("author_ids") {
            check(
                author_ids.as_object().is_some_and(|map| {
                    (1..=MAX_AUTHOR_IDS).contains(&map.len())
                        && map.keys().all(|id| valid_author_id(id))
                }),
                "author ID map",
            )?;
        }
        let mut layout: Self =
            serde_json::from_value(value).map_err(|_| LayoutError::Invalid("document fields"))?;
        layout.validate_geometry()?;
        if let Some(metadata) = metadata_v1.filter(metadata_object) {
            layout.metadata = serde_json::from_value(metadata).ok();
            if layout.validate_metadata(&layout.locale_set()).is_err() {
                layout.metadata = None;
            }
        }
        if let Some(author_ids) = &layout.author_ids {
            let mut seen = BTreeSet::new();
            check(
                author_ids
                    .values()
                    .all(|id| *id != u32::MAX && seen.insert(id)),
                "ambiguous author ID map",
            )?;
        }
        Ok(layout)
    }

    /// Optional author IDs to live CLAP parameter numbers. Call after validating
    /// the layout against the authoritative inventory.
    pub fn author_ids(&self) -> Option<&BTreeMap<String, u32>> {
        self.author_ids.as_ref()
    }

    pub fn has_canvas(&self) -> bool {
        self.pages
            .iter()
            .flat_map(|page| &page.cells)
            .any(|cell| cell.canvas.is_some())
    }

    pub fn localized_plugin(&self) -> Option<LocalizedPlugin> {
        let metadata = self.metadata.as_ref()?;
        Some(LocalizedPlugin {
            default_locale: self.default_locale.clone(),
            names: metadata.names.clone(),
            descriptions: metadata.descriptions.clone(),
        })
    }

    fn locale_set(&self) -> BTreeSet<String> {
        self.locales
            .iter()
            .map(|tag| tag.to_ascii_lowercase())
            .collect()
    }

    fn validate_metadata(&self, locales: &BTreeSet<String>) -> Result<(), LayoutError> {
        if let Some(metadata) = &self.metadata {
            self.validate_labels(&metadata.names, locales, true)?;
            if !metadata.descriptions.is_empty() {
                self.validate_labels(&metadata.descriptions, locales, true)?;
            }
        }
        Ok(())
    }

    fn validate_geometry(&self) -> Result<(), LayoutError> {
        let locales = self.locale_set();
        check(
            (1..=8).contains(&self.locales.len())
                && self.locales.iter().all(|tag| locale_tag(tag))
                && locales.len() == self.locales.len()
                && locale_tag(&self.default_locale)
                && locales.contains(&self.default_locale.to_ascii_lowercase()),
            "locale tags",
        )?;
        check((1..=4).contains(&self.pages.len()), "page count")?;
        self.validate_metadata(&locales)?;
        let mut keys = BTreeSet::new();
        let mut canvas_keys = BTreeSet::new();
        let v2 = self.schema_version == 2;
        let max_columns = if v2 { 14 } else { 12 };
        for page in &self.pages {
            check(
                identifier(&page.key, false) && keys.insert(&page.key),
                "page key",
            )?;
            self.validate_labels(&page.labels, &locales, true)?;
            check(
                (1..=max_columns).contains(&page.columns)
                    && (1..=6 * usize::from(max_columns)).contains(&page.cells.len()),
                "page dimensions",
            )?;
            let mut dividers = BTreeSet::new();
            check(
                page.dividers
                    .iter()
                    .all(|&col| col < page.columns - 1 && dividers.insert(col)),
                "page dividers",
            )?;
            let mut slots = [[0u8; 14]; 3];
            for heading in &page.headings {
                self.validate_labels(&heading.labels, &locales, true)?;
                check(
                    heading.row < 3
                        && heading.cols > 0
                        && heading.col.saturating_add(heading.cols) <= page.columns,
                    "heading geometry",
                )?;
                let start = usize::from(heading.col);
                let end = start + usize::from(heading.cols);
                for slot in &mut slots[usize::from(heading.row)][start..end] {
                    check(*slot == 0, "overlapping headings")?;
                    *slot = 1;
                }
            }
            for cell in &page.cells {
                let (min, max, half, rows) = match cell.widget {
                    Widget::Knob => (1, 1, false, 1..=1),
                    Widget::KnobLarge => (2, 2, false, 1..=1),
                    Widget::Number => (1, if v2 { 2 } else { 1 }, true, 1..=1),
                    Widget::Dropdown => (2, 2, true, 1..=1),
                    Widget::Segment | Widget::ToggleButton => (1, max_columns, true, 1..=1),
                    Widget::Graph | Widget::Canvas => (1, max_columns, false, 1..=3),
                    Widget::MiniFader => (1, 1, false, 1..=3),
                };
                check(
                    rows.contains(&cell.rows)
                        && cell.row.saturating_add(cell.rows) <= 3
                        && cell.half.is_some() == half
                        && (min..=max).contains(&cell.cols)
                        && cell.col.saturating_add(cell.cols) <= page.columns,
                    "cell geometry",
                )?;
                let mask = match cell.half {
                    None => 3,
                    Some(Half::Top) => 1,
                    Some(Half::Bottom) => 2,
                };
                for row in &mut slots[usize::from(cell.row)..usize::from(cell.row + cell.rows)] {
                    for slot in &mut row[usize::from(cell.col)..usize::from(cell.col + cell.cols)] {
                        check(*slot & mask == 0, "overlapping cells")?;
                        *slot |= mask;
                    }
                }
                if let Some(labels) = &cell.labels {
                    self.validate_labels(labels, &locales, false)?;
                }
                if matches!(cell.widget, Widget::Segment | Widget::Dropdown) {
                    let choices = cell
                        .choices
                        .as_ref()
                        .ok_or(LayoutError::Invalid("missing choices"))?;
                    check((2..=16).contains(&choices.len()), "choice count")?;
                    for choice in choices {
                        self.validate_labels(&choice.labels, &locales, true)?;
                    }
                } else {
                    check(cell.choices.is_none(), "unexpected choices")?;
                }
                if cell.widget == Widget::Graph {
                    check(
                        self.schema_version == 2
                            && cell.param_id.is_none()
                            && cell.labels.is_none(),
                        "graph binding",
                    )?;
                    check(cell.canvas.is_none(), "unexpected canvas")?;
                    let graph = cell
                        .graph
                        .as_ref()
                        .ok_or(LayoutError::Invalid("missing graph"))?;
                    self.validate_labels(&graph.name, &locales, true)?;
                    self.validate_labels(&graph.description, &locales, true)?;
                    check(
                        match graph.kind {
                            GraphKind::Eq => {
                                graph.bands.is_some_and(|count| (1..=8).contains(&count))
                            }
                            _ => graph.bands.is_none(),
                        } && match graph.kind {
                            GraphKind::Lfo => {
                                graph.points.is_some_and(|count| (2..=16).contains(&count))
                            }
                            _ => graph.points.is_none(),
                        },
                        "graph dimensions",
                    )?;
                    check(
                        (graph.kind != GraphKind::Xy || graph.slots.len() == 2)
                            && graph.slots.keys().all(|slot| graph.has_slot(slot)),
                        "graph slots",
                    )?;
                } else if cell.widget == Widget::Canvas {
                    check(
                        self.schema_version == 2
                            && cell.param_id.is_none()
                            && cell.labels.is_none()
                            && cell.graph.is_none(),
                        "canvas binding",
                    )?;
                    let canvas = cell
                        .canvas
                        .as_ref()
                        .ok_or(LayoutError::Invalid("missing canvas"))?;
                    check(
                        crate::device_canvas::canvas_key(&canvas.key)
                            && canvas_keys.insert(&canvas.key)
                            && canvas_keys.len() <= crate::device_canvas::MAX_CANVASES,
                        "canvas key",
                    )?;
                    self.validate_labels(&canvas.name, &locales, true)?;
                    self.validate_labels(&canvas.description, &locales, true)?;
                } else {
                    check(
                        cell.param_id.is_some()
                            && cell.graph.is_none()
                            && cell.canvas.is_none()
                            && (cell.widget != Widget::MiniFader || self.schema_version == 2),
                        "control binding",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn validate_labels(
        &self,
        labels: &Labels,
        locales: &BTreeSet<String>,
        default: bool,
    ) -> Result<(), LayoutError> {
        let mut seen = BTreeSet::new();
        check(
            !labels.is_empty()
                && labels.iter().all(|(tag, text)| {
                    let tag = tag.to_ascii_lowercase();
                    locales.contains(&tag)
                        && seen.insert(tag)
                        && text.len() <= 128
                        && !text.trim().is_empty()
                        && !text.chars().any(char::is_control)
                })
                && (!default || seen.contains(&self.default_locale.to_ascii_lowercase())),
            "labels",
        )
    }

    /// Validate every binding against the live CLAP inventory, including duplicate IDs.
    pub fn validate_params(&self, params: &[ParamInfo]) -> Result<(), LayoutError> {
        let mut ids = BTreeMap::new();
        for p in params {
            check(
                p.id != u32::MAX && ids.insert(p.id, p).is_none(),
                "ambiguous parameter IDs",
            )?;
        }
        if let Some(author_ids) = &self.author_ids {
            check(
                author_ids.len() == ids.len() && author_ids.values().all(|id| ids.contains_key(id)),
                "author ID map inventory mismatch",
            )?;
        }
        for cell in self.pages.iter().flat_map(|page| &page.cells) {
            for id in cell.parameter_ids() {
                let p = ids
                    .get(&id)
                    .ok_or(LayoutError::Invalid("unknown parameter ID"))?;
                check(
                    p.flags & (param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS)
                        == 0
                        && p.min.is_finite()
                        && p.max.is_finite()
                        && p.min < p.max,
                    "unsupported parameter",
                )?;
            }
            let Some(id) = cell.param_id else {
                continue;
            };
            let p = ids[&id];
            if cell.widget == Widget::ToggleButton {
                check(
                    p.min == 0.0
                        && p.max == 1.0
                        && p.step_count == 1
                        && p.flags & param_flags::STEPPED != 0,
                    "toggle range",
                )?;
            }
            if let Some(choices) = &cell.choices {
                check((2..=16).contains(&choices.len()), "choice count")?;
                let steps = choices.len() as u32 - 1;
                check(
                    p.flags & (param_flags::STEPPED | param_flags::ENUM)
                        == param_flags::STEPPED | param_flags::ENUM
                        && p.step_count == steps
                        && choices
                            .last()
                            .is_some_and(|choice| p.max == f64::from(choice.value))
                        && choices
                            .iter()
                            .enumerate()
                            .all(|(i, choice)| f64::from(choice.value) == p.min + i as f64),
                    "enum values",
                )?;
            }
        }
        Ok(())
    }

    /// Resolve exact language, primary language and then the document default.
    pub fn label<'a>(&self, labels: &'a Labels, requested: &str) -> Option<&'a str> {
        localized_label(labels, requested, &self.default_locale)
    }
}

fn metadata_object(metadata: &Value) -> bool {
    metadata.is_object()
        && metadata["names"].is_object()
        && metadata.get("descriptions").is_none_or(Value::is_object)
}

fn localized_label<'a>(labels: &'a Labels, requested: &str, default: &str) -> Option<&'a str> {
    let find = |tag: &str| {
        labels
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(tag))
            .map(|(_, value)| value.as_str())
    };
    find(requested)
        .or_else(|| {
            requested
                .split_once('-')
                .and_then(|(primary, _)| find(primary))
        })
        .or_else(|| find(default))
}

fn locale_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default();
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|c| c.is_ascii_alphabetic())
        && parts
            .by_ref()
            .take(2)
            .all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()))
        && parts.next().is_none()
}

fn identifier(id: &str, dot: bool) -> bool {
    let mut chars = id.bytes();
    id.len() <= 32
        && chars.next().is_some_and(|b| b.is_ascii_lowercase())
        && chars.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b) || dot && b == b'.'
        })
}

// serde_json::Value by itself silently keeps the last of duplicate object keys.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut out = Map::new();
                while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                    if out.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate key"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut out = Vec::new();
                while let Some(Unique(value)) = seq.next_element()? {
                    out.push(value);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
        }
        decoder.deserialize_any(UniqueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &[u8] = include_bytes!("../../../docs/clap-device-layout/example.json");

    fn parameter(id: u32, max: f64, step_count: u32, flags: u32) -> ParamInfo {
        ParamInfo {
            id,
            name: String::new(),
            module: String::new(),
            min: 0.0,
            max,
            default: 0.0,
            step_count,
            flags,
        }
    }

    #[test]
    fn the_normative_layout_binds_to_the_live_inventory() {
        let layout = NativeLayout::parse(EXAMPLE).unwrap();
        let gain = parameter(42, 1.0, 0, 0);
        let mode = parameter(7, 2.0, 2, param_flags::STEPPED | param_flags::ENUM);
        assert!(
            layout
                .validate_params(&[gain.clone(), mode.clone()])
                .is_ok()
        );
        assert!(layout.validate_params(std::slice::from_ref(&gain)).is_err());
        let mut hidden = mode.clone();
        hidden.flags |= param_flags::HIDDEN;
        assert!(layout.validate_params(&[gain.clone(), hidden]).is_err());
        assert!(
            layout
                .validate_params(&[gain.clone(), mode.clone(), mode.clone()])
                .is_err()
        );
        assert!(layout.validate_params(&[mode, gain]).is_ok());
        let labels = &layout.pages[0].labels;
        assert_eq!(layout.label(labels, "de-CH"), Some("Übersicht"));
        assert_eq!(layout.label(labels, "fr"), Some("Main"));
    }

    #[test]
    fn device_catalog_labels_are_localized_and_optional() {
        use serde_json::json;

        let mut example: Value = serde_json::from_slice(EXAMPLE).unwrap();
        example["metadata"] = json!({
            "names": {"en": "Test Gain", "de": "Testverstärker"},
            "descriptions": {"en": "Adjust the level"}
        });
        let layout = NativeLayout::parse(&serde_json::to_vec(&example).unwrap()).unwrap();
        let labels = layout.localized_plugin().unwrap();
        assert_eq!(labels.name("de-CH"), Some("Testverstärker"));
        assert_eq!(labels.name("fr"), Some("Test Gain"));
        assert_eq!(labels.description("de"), Some("Adjust the level"));
        for invalid in [
            json!(["Test Gain"]),
            json!({"names": {"de": "No default locale"}}),
            json!({"names": {"en": "Test Gain", "fr": "Undeclared locale"}}),
            json!({"names": {"en": "Test Gain"}, "descriptions": {"en": "Bad\ntext"}}),
        ] {
            example["metadata"] = invalid;
            let layout = NativeLayout::parse(&serde_json::to_vec(&example).unwrap()).unwrap();
            assert!(layout.localized_plugin().is_none());
            assert_eq!(layout.pages[0].key, "main");
        }
    }

    #[test]
    fn version_two_supports_wide_pages_number_fields_and_dividers() {
        let mut value: Value = serde_json::from_slice(EXAMPLE).unwrap();
        value["schema_version"] = 2.into();
        let page = &mut value["pages"][0];
        page["columns"] = 14.into();
        page["dividers"] = serde_json::json!([9, 11]);
        page["headings"] = serde_json::json!([
            {"col": 12, "row": 1, "cols": 2, "labels": {"en": "Level", "de": "Pegel"}}
        ]);
        page["cells"] = serde_json::json!([{
            "widget": "number", "col": 12, "row": 0, "cols": 2, "rows": 1,
            "half": "top", "param_id": 42,
        }]);
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        assert_eq!(parse(&value).unwrap().pages[0].dividers, [9, 11]);
        let layout = parse(&value).unwrap();
        assert_eq!(
            layout.label(&layout.pages[0].headings[0].labels, "de"),
            Some("Pegel")
        );
        let mut overlap = value.clone();
        overlap["pages"][0]["headings"][0]["row"] = 0.into();
        assert!(parse(&overlap).is_err());
        for dividers in [serde_json::json!([9, 9]), serde_json::json!([13])] {
            let mut invalid = value.clone();
            invalid["pages"][0]["dividers"] = dividers;
            assert!(parse(&invalid).is_err());
        }
        value["schema_version"] = 1.into();
        assert!(parse(&value).is_err());
        value["pages"][0]["columns"] = 12.into();
        value["pages"][0]["cells"][0]["col"] = 10.into();
        assert!(parse(&value).is_err());
        value["pages"][0]["cells"][0]["cols"] = 1.into();
        value["pages"][0]["dividers"] = false.into();
        let layout = parse(&value).unwrap();
        assert!(layout.pages[0].dividers.is_empty());
        assert!(layout.pages[0].headings.is_empty());
    }

    #[test]
    fn version_two_limits_graph_and_canvas_bindings() {
        let source = include_bytes!("../../../docs/clap-device-layout/example-v2.json");
        let layout = NativeLayout::parse(source).unwrap();
        assert_eq!(layout.schema_version, 2);
        assert!(layout.has_canvas());
        assert!(layout.validate_params(&[]).is_err());
        assert!(layout.validate_params(&[parameter(42, 1.0, 0, 0)]).is_ok());

        let value: Value = serde_json::from_slice(source).unwrap();
        for (path, invalid) in [
            ("/schema_version", 1.into()),
            ("/pages/0/cells/0/graph/bands", 9.into()),
            (
                "/pages/0/cells/1/graph/slots",
                serde_json::json!({"unknown": 42}),
            ),
            ("/pages/0/cells/1/col", 2.into()),
            ("/pages/1/cells/0/canvas/key", "Needs validation".into()),
            (
                "/pages/1/cells/0/canvas/description",
                serde_json::json!({"de":"Beschreibung"}),
            ),
        ] {
            let mut modified = value.clone();
            *modified.pointer_mut(path).unwrap() = invalid;
            assert!(
                NativeLayout::parse(&serde_json::to_vec(&modified).unwrap()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_positional_objects_and_non_string_tags() {
        use serde_json::json;
        let example: Value = serde_json::from_slice(EXAMPLE).unwrap();
        let mut modified = example.clone();
        modified["pages"][0]["future"] = json!(true);
        modified["pages"][0]["cells"][0]["future"] = json!([]);
        modified["pages"][0]["cells"][1]["choices"][0]["future"] = json!({});
        assert!(NativeLayout::parse(&serde_json::to_vec(&modified).unwrap()).is_ok());

        for (pointer, invalid) in [
            (
                "/pages/0",
                json!(["main", {"en": "Main"}, 4, example["pages"][0]["cells"]]),
            ),
            (
                "/pages/0/cells/0",
                json!(["knob", 0, 0, 1, 1, null, 42, null, null]),
            ),
            ("/pages/0/cells/0/widget", json!({"knob": null})),
            ("/pages/0/cells/1/half", json!({"top": null})),
            ("/pages/0/cells/1/half", Value::Null),
            ("/pages/0/cells/1/choices/0", json!([0, {"en": "Clean"}])),
        ] {
            let mut modified = example.clone();
            *modified.pointer_mut(pointer).unwrap() = invalid;
            assert!(
                NativeLayout::parse(&serde_json::to_vec(&modified).unwrap()).is_err(),
                "{pointer}",
            );
        }
    }

    #[test]
    fn enum_bounds_match_the_choices_exactly() {
        let mut layout = NativeLayout::parse(EXAMPLE).unwrap();
        let choices = layout.pages[0].cells[1].choices.as_mut().unwrap();
        choices.truncate(2);
        choices[0].value = -1;
        choices[1].value = 0;
        let mut mode = parameter(7, 0.0, 1, param_flags::STEPPED | param_flags::ENUM);
        mode.min = -1.0;
        let gain = parameter(42, 1.0, 0, 0);
        assert!(
            layout
                .validate_params(&[gain.clone(), mode.clone()])
                .is_ok()
        );
        mode.max = 1e-20;
        assert!(
            layout
                .validate_params(&[gain.clone(), mode.clone()])
                .is_err()
        );
        mode.max = 0.0;
        layout.pages[0].cells[1].choices.as_mut().unwrap().clear();
        assert_eq!(
            layout.validate_params(&[gain, mode]),
            Err(LayoutError::Invalid("choice count"))
        );
    }

    #[test]
    fn rejects_bad_documents_and_falls_back_for_future_capabilities() {
        assert!(NativeLayout::parse(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
        let mut example: Value = serde_json::from_slice(EXAMPLE).unwrap();
        example["required_capabilities"] = serde_json::json!(["extra"]);
        assert_eq!(
            NativeLayout::parse(&serde_json::to_vec(&example).unwrap()),
            Err(LayoutError::Unsupported)
        );
        example["required_capabilities"] = serde_json::json!([]);
        example["optional"] = serde_json::json!({"forward": true});
        assert!(NativeLayout::parse(&serde_json::to_vec(&example).unwrap()).is_ok());
        example["pages"][0]["cells"][1]["col"] = 0.into();
        assert!(NativeLayout::parse(&serde_json::to_vec(&example).unwrap()).is_err());
        let duplicate = br#"{"schema_version":1,"unknown":{"key":0,"key":1}}"#;
        assert_eq!(
            NativeLayout::parse(duplicate),
            Err(LayoutError::Invalid("JSON syntax or duplicate keys"))
        );
    }
}
