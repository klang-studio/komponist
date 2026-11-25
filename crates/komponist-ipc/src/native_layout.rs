//! Owned metadata for the native CLAP device-layout contract. Used on control threads only.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::control::{ParamInfo, param_flags};

pub const EXTENSION_V1: &CStr = c"org.klang-studio.komponist.device-layout/1";
pub const EXTENSION_V2: &CStr = c"org.klang-studio.komponist.device-layout/2";
/// Required capability for layouts using source-fraction trim.
pub const CAP_SOURCE_FRACTION_TRIM: &str = "source-fraction-trim";
/// Required capability for panels whose pages are selected from a pad grid.
pub const CAP_PAD_BANK: &str = "pad-bank";
/// Required capability for panels that show physical values for normalized parameters.
pub const CAP_PARAMETER_DISPLAY: &str = "parameter-display";
/// Required capability for Boolean-controlled knobs.
pub const CAP_CONDITIONAL_CONTROLS: &str = "conditional-controls";
/// Required capability for controls following the selected band of an EQ graph.
pub const CAP_EQ_BAND_CONTROLS: &str = "eq-band-controls";
/// Layout selections that change the bindings of other cells without editing a parameter.
pub const CAP_CELL_SELECTIONS: &str = "cell-selections";
/// Maximum number of pad-selected pages.
pub const MAX_PADS: usize = 16;

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

/// Mapping from a continuous CLAP parameter's normalized value to its display range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayScale {
    Linear,
    Logarithmic,
}

/// Unit used when describing the physical display value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayUnit {
    None,
    Hertz,
    Milliseconds,
    Percent,
    Decibels,
    DecibelWidth,
    Ratio,
}

/// Presentation of a normalized CLAP parameter within a layout v2 panel.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParameterDisplay {
    min: Number,
    max: Number,
    default: Number,
    pub scale: DisplayScale,
    pub unit: DisplayUnit,
}

impl ParameterDisplay {
    pub fn new(
        min: f64,
        max: f64,
        default: f64,
        scale: DisplayScale,
        unit: DisplayUnit,
    ) -> Option<Self> {
        let display = Self {
            min: Number::from_f64(min)?,
            max: Number::from_f64(max)?,
            default: Number::from_f64(default)?,
            scale,
            unit,
        };
        display.valid().then_some(display)
    }

    pub fn range(&self) -> (f64, f64) {
        (
            self.min.as_f64().expect("JSON number"),
            self.max.as_f64().expect("JSON number"),
        )
    }

    pub fn default_value(&self) -> f64 {
        self.default.as_f64().expect("JSON number")
    }

    fn valid(&self) -> bool {
        let (min, max) = self.range();
        min < max
            && (max - min).is_finite()
            && (min..=max).contains(&self.default_value())
            && (self.scale != DisplayScale::Logarithmic || min > 0.0 && (max / min).is_finite())
    }

    fn normalized_default(&self) -> f64 {
        let (min, max) = self.range();
        let default = self.default_value();
        match self.scale {
            DisplayScale::Linear => (default - min) / (max - min),
            DisplayScale::Logarithmic => (default / min).ln() / (max / min).ln(),
        }
    }
}

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
    Selector,
    PadGrid,
    Graph,
    Canvas,
    MeterGr,
}

/// One pad, selecting the corresponding page in the document.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Pad {
    pub note: u8,
    pub asset_slot: u32,
}

/// The pad grid shared by all pages of a pad-selected panel.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PadBank {
    pub pads: Vec<Pad>,
    /// Optional named pad-activity signal.
    pub activity_tap: Option<String>,
    /// Optional toggle controlling selection by incoming notes.
    pub follow_param: Option<u32>,
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
    FilterVa,
    LfoShape,
    Wavetable,
}

/// Units of the two waveform trim bindings as sent to the CLAP parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimUnit {
    Milliseconds,
    SourceFraction,
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
    /// Named streaming data used by this graph's host-rendered layers.
    #[serde(default)]
    pub signal_taps: BTreeMap<String, String>,
    /// Host-managed audio file slot shown by a waveform graph.
    pub asset_slot: Option<u32>,
    /// Show a marker list supplied for the graph's audio asset slot.
    #[serde(default)]
    pub waveform_markers: bool,
    /// Show file and zoom controls for the graph's audio asset slot.
    #[serde(default)]
    pub waveform_toolbar: bool,
    /// Three fixed parameter controls replaced by the options of the selected slice.
    pub waveform_slice_controls: Option<[u32; 3]>,
    /// Parameter supplying a MIDI note for press-and-release audition.
    pub audition_note: Option<u32>,
    /// Defaults to milliseconds. Source fractions require a bound audio asset.
    pub trim_unit: Option<TrimUnit>,
    pub bands: Option<u8>,
    #[serde(default)]
    pub select_band: bool,
    pub points: Option<u8>,
    #[serde(default)]
    pub variants: Vec<GraphVariant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct GraphVariant {
    pub name: Option<Labels>,
    pub slots: BTreeMap<String, u32>,
}

impl Graph {
    fn has_slot(&self, slot: &str) -> bool {
        match self.kind {
            GraphKind::Envelope => {
                matches!(slot, "attack" | "decay" | "sustain" | "release" | "curve")
            }
            GraphKind::Eq => {
                slot == "spectrum_enabled"
                    || slot.split_once('_').is_some_and(|(band, suffix)| {
                        let Some(band) = band.strip_prefix('b') else {
                            return false;
                        };
                        let Ok(index) = band.parse::<u8>() else {
                            return false;
                        };
                        band == index.to_string()
                            && (1..=self.bands.unwrap_or_default()).contains(&index)
                            && matches!(suffix, "freq" | "gain" | "q" | "type" | "on")
                    })
            }
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
            GraphKind::FilterVa => {
                matches!(slot, "cutoff" | "resonance" | "type" | "at_cut")
                    || synth_modulation_slot(slot)
            }
            GraphKind::Wavetable => {
                matches!(slot, "table" | "position") || synth_modulation_slot(slot)
            }
            GraphKind::LfoShape => slot == "shape",
        }
    }
}

/// Named inputs of the four-slot modulation matrix and the two LFO polarities.
fn synth_modulation_slot(slot: &str) -> bool {
    matches!(slot, "lfo1_uni" | "lfo2_uni")
        || slot
            .strip_prefix("mod")
            .and_then(|slot| slot.split_once('_'))
            .is_some_and(|(entry, role)| {
                matches!(entry, "1" | "2" | "3" | "4") && matches!(role, "src" | "dst" | "amt")
            })
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
    pub full_labels: Option<Labels>,
}

/// A control's binding for one selection, in the selection's order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ControlBinding {
    pub param_id: u32,
    pub labels: Option<Labels>,
    pub choices: Option<Vec<Choice>>,
}

/// A page-local segmented control that picks which bindings its followers use.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Selector {
    pub key: String,
    pub labels: Labels,
    pub options: Vec<Labels>,
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
    /// Boolean toggle on the same page that disables this knob when on.
    pub disabled_by: Option<u32>,
    pub labels: Option<Labels>,
    pub choices: Option<Vec<Choice>>,
    #[serde(default)]
    pub band_controls: Vec<ControlBinding>,
    pub selector: Option<Selector>,
    pub follows: Option<String>,
    #[serde(default)]
    pub variants: Vec<ControlBinding>,
    pub graph: Option<Graph>,
    pub canvas: Option<Canvas>,
    /// Signal tap key providing the latest gain reduction in positive dB.
    pub reduction_tap: Option<String>,
}

impl Cell {
    /// Every numeric parameter referenced by this cell.
    pub fn parameter_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.param_id
            .into_iter()
            .chain(self.disabled_by)
            .chain(self.variant_bindings().map(|binding| binding.param_id))
            .chain(self.graph.iter().flat_map(|graph| {
                graph
                    .slots
                    .values()
                    .copied()
                    .chain(graph.audition_note)
                    .chain(
                        graph
                            .variants
                            .iter()
                            .flat_map(|variant| variant.slots.values().copied()),
                    )
            }))
    }

    pub fn variant_bindings(&self) -> impl Iterator<Item = &ControlBinding> {
        self.band_controls.iter().chain(&self.variants)
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
    pub pad_bank: Option<PadBank>,
    #[serde(default)]
    author_ids: Option<BTreeMap<String, u32>>,
    #[serde(default)]
    parameter_display: BTreeMap<u32, ParameterDisplay>,
    #[serde(skip)]
    eq_band_controls: bool,
    #[serde(skip)]
    cell_selections: bool,
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
    pub fn parameter_display(&self, id: u32) -> Option<&ParameterDisplay> {
        self.parameter_display.get(&id)
    }

    pub fn has_parameter_display(&self) -> bool {
        !self.parameter_display.is_empty()
    }

    pub fn has_eq_band_controls(&self) -> bool {
        self.eq_band_controls
    }

    pub fn has_cell_selections(&self) -> bool {
        self.cell_selections
    }

    /// The editor uses host-managed audio files for a waveform or its pad bank.
    pub fn has_audio_asset_editor(&self) -> bool {
        self.pad_bank.is_some()
            || self.pages.iter().flat_map(|page| &page.cells).any(|cell| {
                cell.graph
                    .as_ref()
                    .is_some_and(|graph| graph.asset_slot.is_some())
            })
    }

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
            if let Some(root) = value.as_object_mut() {
                root.remove("pad_bank");
                root.remove("parameter_display");
            }
            value
                .as_object_mut()
                .and_then(|root| root.remove("metadata"))
        } else {
            None
        };
        if let Some(metadata) = value.get("metadata") {
            check(metadata_object(metadata), "plugin metadata")?;
        }
        let mut fractional_trim_supported = false;
        let mut pad_bank_supported = false;
        let mut parameter_display_supported = false;
        let mut conditional_controls_supported = false;
        let mut eq_band_controls_supported = false;
        let mut cell_selections_supported = false;
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
            if seen.iter().any(|cap| {
                version != 2
                    || !matches!(
                        *cap,
                        CAP_SOURCE_FRACTION_TRIM
                            | CAP_PAD_BANK
                            | CAP_PARAMETER_DISPLAY
                            | CAP_CONDITIONAL_CONTROLS
                            | CAP_EQ_BAND_CONTROLS
                            | CAP_CELL_SELECTIONS
                    )
            }) {
                return Err(LayoutError::Unsupported);
            }
            fractional_trim_supported = seen.contains(CAP_SOURCE_FRACTION_TRIM);
            pad_bank_supported = seen.contains(CAP_PAD_BANK);
            parameter_display_supported = seen.contains(CAP_PARAMETER_DISPLAY);
            conditional_controls_supported = seen.contains(CAP_CONDITIONAL_CONTROLS);
            eq_band_controls_supported = seen.contains(CAP_EQ_BAND_CONTROLS);
            cell_selections_supported = seen.contains(CAP_CELL_SELECTIONS);
        }
        let display = value.get("parameter_display");
        check(
            display.is_some() == parameter_display_supported
                && display.is_none_or(|display| {
                    display.as_object().is_some_and(|entries| {
                        (1..=MAX_AUTHOR_IDS).contains(&entries.len())
                            && entries.iter().all(|(key, entry)| {
                                key.parse::<u32>()
                                    .is_ok_and(|id| id != u32::MAX && id.to_string() == *key)
                                    && entry.is_object()
                            })
                    })
                }),
            "parameter display map",
        )?;
        if let Some(bank) = value.get("pad_bank") {
            check(
                bank.is_object()
                    && bank["pads"].as_array().is_some_and(|pads| {
                        (1..=MAX_PADS).contains(&pads.len()) && pads.iter().all(Value::is_object)
                    }),
                "pad bank object",
            )?;
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
                            cell.remove("reduction_tap");
                            cell.remove("disabled_by");
                            cell.remove("band_controls");
                            cell.remove("selector");
                            cell.remove("follows");
                            cell.remove("variants");
                        }
                        if !eq_band_controls_supported
                            && let Some(choices) =
                                cell.get_mut("choices").and_then(Value::as_array_mut)
                        {
                            for choice in choices.iter_mut().filter_map(Value::as_object_mut) {
                                choice.remove("full_labels");
                            }
                        }
                        let widget = cell
                            .get("widget")
                            .and_then(Value::as_str)
                            .ok_or(LayoutError::Invalid("widget tag"))?;
                        let band_controls = cell.get("band_controls");
                        check(
                            band_controls.is_none_or(|bindings| {
                                eq_band_controls_supported
                                    && bindings.as_array().is_some_and(|bindings| {
                                        (1..=8).contains(&bindings.len())
                                            && bindings.iter().all(control_binding_object)
                                    })
                            }),
                            "EQ band controls",
                        )?;
                        let variants = cell.get("variants");
                        check(
                            cell.get("selector").is_none_or(|selector| {
                                cell_selections_supported && selector.is_object()
                            }) && cell.get("follows").is_none_or(|follows| {
                                cell_selections_supported && follows.is_string()
                            }) && variants.is_none_or(|bindings| {
                                cell_selections_supported
                                    && bindings.as_array().is_some_and(|bindings| {
                                        (2..=4).contains(&bindings.len())
                                            && bindings.iter().all(control_binding_object)
                                    })
                            }),
                            "cell selections",
                        )?;
                        check(
                            if band_controls.is_some()
                                || variants.is_some()
                                || version == 2
                                    && matches!(
                                        widget,
                                        "graph" | "canvas" | "meter_gr" | "pad_grid" | "selector"
                                    )
                            {
                                !cell.contains_key("param_id")
                            } else {
                                cell.get("param_id").is_some_and(Value::is_u64)
                            },
                            "parameter ID",
                        )?;
                        check(
                            cell.get("reduction_tap").is_none_or(Value::is_string),
                            "reduction tap key",
                        )?;
                        check(
                            cell.get("disabled_by").is_none_or(|binding| {
                                conditional_controls_supported && binding.is_u64()
                            }),
                            "conditional control binding",
                        )?;
                        if let Some(graph) = cell.get("graph") {
                            check(
                                graph.is_object()
                                    && graph["kind"].is_string()
                                    && graph["name"].is_object()
                                    && graph["description"].is_object()
                                    && graph.get("slots").is_none_or(Value::is_object)
                                    && graph.get("signal_taps").is_none_or(Value::is_object),
                                "graph object",
                            )?;
                            check(
                                graph.get("variants").is_none_or(|variants| {
                                    cell_selections_supported
                                        && variants.as_array().is_some_and(|variants| {
                                            (2..=4).contains(&variants.len())
                                                && variants.iter().all(|variant| {
                                                    variant.is_object()
                                                        && variant["slots"].is_object()
                                                })
                                        })
                                }),
                                "graph binding variants",
                            )?;
                            check(
                                eq_band_controls_supported
                                    || (graph
                                        .get("select_band")
                                        .is_none_or(|select| select.as_bool() == Some(false))
                                        && graph["slots"].get("spectrum_enabled").is_none()),
                                "EQ band controls capability",
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
                            check(choice_objects(choices), "choice objects")?;
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
        layout.eq_band_controls = eq_band_controls_supported;
        layout.cell_selections = cell_selections_supported;
        check(
            layout.pad_bank.is_some() == pad_bank_supported,
            "pad bank capability",
        )?;
        layout.validate_geometry()?;
        check(
            fractional_trim_supported
                || !layout
                    .pages
                    .iter()
                    .flat_map(|page| &page.cells)
                    .filter_map(|cell| cell.graph.as_ref())
                    .any(|graph| graph.trim_unit == Some(TrimUnit::SourceFraction)),
            "waveform source fraction capability",
        )?;
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
        check(
            self.parameter_display.values().all(ParameterDisplay::valid),
            "parameter display range",
        )?;
        let locales = self.locale_set();
        check(
            (1..=8).contains(&self.locales.len())
                && self.locales.iter().all(|tag| locale_tag(tag))
                && locales.len() == self.locales.len()
                && locale_tag(&self.default_locale)
                && locales.contains(&self.default_locale.to_ascii_lowercase()),
            "locale tags",
        )?;
        if let Some(bank) = &self.pad_bank {
            self.validate_pad_bank(bank)?;
        } else {
            check((1..=4).contains(&self.pages.len()), "page count")?;
        }
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
            let mut selections = page
                .cells
                .iter()
                .filter_map(|cell| cell.graph.as_ref().filter(|graph| graph.select_band));
            let selection = selections.next();
            check(
                selections.next().is_none(),
                "one selectable EQ graph per page",
            )?;
            let mut selectors = BTreeMap::new();
            for selector in page.cells.iter().filter_map(|cell| cell.selector.as_ref()) {
                check(
                    identifier(&selector.key, false)
                        && (2..=4).contains(&selector.options.len())
                        && selectors
                            .insert(selector.key.as_str(), selector.options.len())
                            .is_none(),
                    "selection key or options",
                )?;
                self.validate_labels(&selector.labels, &locales, true)?;
                for labels in &selector.options {
                    self.validate_labels(labels, &locales, true)?;
                }
            }
            for cell in &page.cells {
                let (min, max, half, rows) = match cell.widget {
                    Widget::Knob => (1, 1, false, 1..=1),
                    Widget::KnobLarge => (2, 2, false, 1..=1),
                    Widget::Number => (1, if v2 { 2 } else { 1 }, true, 1..=1),
                    Widget::Dropdown => (2, 2, true, 1..=1),
                    Widget::Segment | Widget::ToggleButton | Widget::Selector => {
                        (1, max_columns, true, 1..=1)
                    }
                    Widget::Graph | Widget::Canvas => (1, max_columns, false, 1..=3),
                    Widget::MeterGr => (1, 1, false, 3..=3),
                    Widget::MiniFader => (1, 1, false, 1..=3),
                    Widget::PadGrid => (1, max_columns, false, 1..=3),
                };
                let half_graph = cell.widget == Widget::Graph && cell.half == Some(Half::Top);
                check(
                    rows.contains(&cell.rows)
                        && cell.row.saturating_add(cell.rows) <= 3
                        && (half_graph || cell.half.is_some() == half)
                        && (min..=max).contains(&cell.cols)
                        && cell.col.saturating_add(cell.cols) <= page.columns,
                    "cell geometry",
                )?;
                if let Some(driver) = cell.disabled_by {
                    check(
                        matches!(cell.widget, Widget::Knob | Widget::KnobLarge)
                            && cell.param_id != Some(driver)
                            && page.cells.iter().any(|control| {
                                control.widget == Widget::ToggleButton
                                    && control.param_id == Some(driver)
                            }),
                        "conditional control requires a separate toggle on the same page",
                    )?;
                }
                let rows = &mut slots[usize::from(cell.row)..usize::from(cell.row + cell.rows)];
                let last = rows.len() - 1;
                for (index, row) in rows.iter_mut().enumerate() {
                    let mask = match cell.half {
                        Some(Half::Top) if index == last => 1,
                        Some(Half::Bottom) => 2,
                        _ => 3,
                    };
                    for slot in &mut row[usize::from(cell.col)..usize::from(cell.col + cell.cols)] {
                        check(*slot & mask == 0, "overlapping cells")?;
                        *slot |= mask;
                    }
                }
                if let Some(labels) = &cell.labels {
                    self.validate_labels(labels, &locales, false)?;
                }
                self.validate_selection(cell, &selectors, &locales)?;
                if !cell.band_controls.is_empty() {
                    self.validate_band_controls(cell, selection, &locales)?;
                } else if cell.variants.is_empty() {
                    self.validate_control_choices(cell.widget, cell.choices.as_deref(), &locales)?;
                }
                check(
                    if cell.widget == Widget::MeterGr {
                        cell.reduction_tap
                            .as_deref()
                            .is_some_and(crate::signal_taps::valid_key)
                    } else {
                        cell.reduction_tap.is_none()
                    },
                    "reduction tap binding",
                )?;
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
                    if graph.select_band {
                        check(
                            graph.kind == GraphKind::Eq
                                && (1..=graph.bands.unwrap_or_default()).all(|band| {
                                    ["freq", "gain", "q", "on", "type"].into_iter().all(|slot| {
                                        graph.slots.contains_key(&format!("b{band}_{slot}"))
                                    })
                                }),
                            "selectable EQ graph slots",
                        )?;
                    }
                    if graph.slots.contains_key("spectrum_enabled") {
                        check(
                            graph.signal_taps.contains_key("spectrum"),
                            "spectrum switch without tap",
                        )?;
                    }
                    check(
                        graph.asset_slot.is_none() || graph.kind == GraphKind::Waveform,
                        "graph asset slot",
                    )?;
                    check(
                        !graph.waveform_markers
                            || graph.kind == GraphKind::Waveform && graph.asset_slot.is_some(),
                        "waveform markers require an asset slot",
                    )?;
                    check(
                        !graph.waveform_toolbar
                            || graph.kind == GraphKind::Waveform
                                && graph.asset_slot.is_some()
                                && cell.cols >= 6,
                        "waveform toolbar requires an asset slot and six columns",
                    )?;
                    if let Some(hidden) = graph.waveform_slice_controls {
                        check(
                            graph.waveform_markers
                                && graph.waveform_toolbar
                                && graph.audition_note.is_some()
                                && graph.slots.contains_key("start")
                                && graph.slots.contains_key("end")
                                && cell.row == 0
                                && cell.rows == 2
                                && cell.cols == 10
                                && hidden.iter().copied().collect::<BTreeSet<_>>().len() == 3
                                && hidden.iter().all(|&id| {
                                    page.cells.iter().any(|other| other.param_id == Some(id))
                                })
                                && page.cells.iter().all(|other| {
                                    let horizontal = (cell.col..cell.col + cell.cols).any(|col| {
                                        (other.col..other.col.saturating_add(other.cols))
                                            .contains(&col)
                                    });
                                    let row_after = other.row.saturating_add(other.rows) > 2;
                                    let is_trim = other.row == 2
                                        && other.half == Some(Half::Top)
                                        && other.col.saturating_add(other.cols) <= cell.col + 2;
                                    !horizontal
                                        || !row_after
                                        || is_trim
                                        || other.param_id.is_some_and(|id| hidden.contains(&id))
                                }),
                            "waveform slice control geometry",
                        )?;
                    }
                    check(
                        graph.audition_note.is_none()
                            || graph.kind == GraphKind::Waveform && graph.asset_slot.is_some(),
                        "waveform audition requires an asset slot",
                    )?;
                    check(
                        graph.trim_unit.is_none()
                            || graph.kind == GraphKind::Waveform
                                && (graph.trim_unit != Some(TrimUnit::SourceFraction)
                                    || graph.asset_slot.is_some()),
                        "waveform trim unit",
                    )?;
                    check(
                        graph.signal_taps.iter().all(|(role, name)| {
                            crate::signal_taps::valid_key(name)
                                && match (graph.kind, role.as_str()) {
                                    (GraphKind::Eq, "spectrum")
                                    | (GraphKind::Compressor | GraphKind::GrHistory, "level")
                                    | (GraphKind::GrHistory, "reduction") => true,
                                    (GraphKind::Waveform, "playhead") => graph.asset_slot.is_some(),
                                    _ => false,
                                }
                        }),
                        "graph signal taps",
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
                } else if matches!(
                    cell.widget,
                    Widget::MeterGr | Widget::PadGrid | Widget::Selector
                ) {
                    check(
                        self.schema_version == 2
                            && (cell.widget != Widget::PadGrid || self.pad_bank.is_some())
                            && cell.param_id.is_none()
                            && cell.labels.is_none()
                            && cell.graph.is_none()
                            && cell.canvas.is_none(),
                        "display binding",
                    )?;
                } else {
                    check(
                        (cell.param_id.is_some() || cell.variant_bindings().next().is_some())
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

    fn validate_selection(
        &self,
        cell: &Cell,
        selectors: &BTreeMap<&str, usize>,
        locales: &BTreeSet<String>,
    ) -> Result<(), LayoutError> {
        let graph_variants = cell
            .graph
            .as_ref()
            .map_or(&[][..], |graph| graph.variants.as_slice());
        check(
            cell.selector.is_some() == (cell.widget == Widget::Selector),
            "selector binding",
        )?;
        let Some(key) = &cell.follows else {
            return check(
                cell.variants.is_empty() && graph_variants.is_empty(),
                "variants require a selector",
            );
        };
        let count = selectors
            .get(key.as_str())
            .ok_or(LayoutError::Invalid("missing selector"))?;
        check(
            cell.selector.is_none()
                && cell.param_id.is_none()
                && cell.labels.is_none()
                && cell.choices.is_none()
                && cell.disabled_by.is_none()
                && cell.band_controls.is_empty(),
            "mixed selection bindings",
        )?;
        if let Some(graph) = &cell.graph {
            check(
                cell.variants.is_empty()
                    && matches!(
                        graph.kind,
                        GraphKind::Envelope | GraphKind::LfoShape | GraphKind::Wavetable
                    )
                    && (graph.kind != GraphKind::Wavetable || *count == 2)
                    && graph_variants.len() == *count
                    && graph_variants.iter().all(|variant| {
                        !variant.slots.is_empty()
                            && variant
                                .slots
                                .keys()
                                .all(|slot| graph.has_slot(slot) && !graph.slots.contains_key(slot))
                    })
                    && graph_variants
                        .windows(2)
                        .all(|pair| pair[0].slots.keys().eq(pair[1].slots.keys())),
                "graph variant slots",
            )?;
            for variant in graph_variants {
                if let Some(name) = &variant.name {
                    self.validate_labels(name, locales, true)?;
                }
            }
        } else {
            check(cell.variants.len() == *count, "control variants")?;
            for binding in &cell.variants {
                if let Some(labels) = &binding.labels {
                    self.validate_labels(labels, locales, false)?;
                }
                self.validate_control_choices(cell.widget, binding.choices.as_deref(), locales)?;
            }
        }
        Ok(())
    }

    fn validate_control_choices(
        &self,
        widget: Widget,
        choices: Option<&[Choice]>,
        locales: &BTreeSet<String>,
    ) -> Result<(), LayoutError> {
        if matches!(widget, Widget::Segment | Widget::Dropdown) {
            let choices = choices.ok_or(LayoutError::Invalid("missing choices"))?;
            check((2..=16).contains(&choices.len()), "choice count")?;
            for choice in choices {
                self.validate_labels(&choice.labels, locales, true)?;
                if let Some(labels) = &choice.full_labels {
                    self.validate_labels(labels, locales, true)?;
                }
            }
        } else {
            check(choices.is_none(), "unexpected choices")?;
        }
        Ok(())
    }

    fn validate_band_controls(
        &self,
        cell: &Cell,
        graph: Option<&Graph>,
        locales: &BTreeSet<String>,
    ) -> Result<(), LayoutError> {
        let graph = graph.ok_or(LayoutError::Invalid(
            "band controls require a selectable EQ graph",
        ))?;
        let roles: &[&str] = match cell.widget {
            Widget::Knob | Widget::KnobLarge => &["freq", "gain", "q"],
            Widget::ToggleButton => &["on"],
            Widget::Segment => &["type"],
            _ => &[],
        };
        check(
            cell.param_id.is_none()
                && cell.labels.is_none()
                && cell.choices.is_none()
                && cell.disabled_by.is_none()
                && cell.band_controls.len() == usize::from(graph.bands.unwrap_or_default())
                && roles.iter().any(|role| {
                    cell.band_controls
                        .iter()
                        .enumerate()
                        .all(|(index, binding)| {
                            graph.slots.get(&format!("b{}_{role}", index + 1))
                                == Some(&binding.param_id)
                        })
                }),
            "invalid EQ band control binding",
        )?;
        for binding in &cell.band_controls {
            if let Some(labels) = &binding.labels {
                self.validate_labels(labels, locales, false)?;
            }
            self.validate_control_choices(cell.widget, binding.choices.as_deref(), locales)?;
            if let Some(choices) = &binding.choices {
                check(
                    choices.len() == 5 && choices[0].value == 0,
                    "EQ band type choices",
                )?;
            }
        }
        Ok(())
    }

    fn validate_pad_bank(&self, bank: &PadBank) -> Result<(), LayoutError> {
        let mut notes = BTreeSet::new();
        let mut slots = BTreeSet::new();
        check(
            self.pages.len() == bank.pads.len()
                && (1..=MAX_PADS).contains(&bank.pads.len())
                && bank.pads.iter().all(|pad| {
                    pad.note < 128
                        && notes.insert(pad.note)
                        && pad.asset_slot != u32::MAX
                        && slots.insert(pad.asset_slot)
                })
                && bank
                    .activity_tap
                    .as_deref()
                    .is_none_or(crate::signal_taps::valid_key)
                && (bank.follow_param.is_none() || bank.activity_tap.is_some()),
            "pad bank bindings",
        )?;
        let mut geometry = None;
        for page in &self.pages {
            let mut grids = page
                .cells
                .iter()
                .filter(|cell| cell.widget == Widget::PadGrid);
            let grid = grids
                .next()
                .ok_or(LayoutError::Invalid("missing pad grid"))?;
            let position = (grid.col, grid.row, grid.cols, grid.rows);
            let columns = usize::from(grid.cols);
            let rows = usize::from(grid.rows);
            check(
                grids.next().is_none()
                    && geometry.is_none_or(|previous| previous == position)
                    && columns > 0
                    && bank.pads.len().div_ceil(columns) == rows
                    && bank.follow_param.is_none_or(|id| {
                        page.cells.iter().any(|cell| {
                            cell.widget == Widget::ToggleButton && cell.param_id == Some(id)
                        })
                    }),
                "pad grid bindings",
            )?;
            geometry = Some(position);
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
        for (&id, display) in &self.parameter_display {
            let param = ids
                .get(&id)
                .ok_or(LayoutError::Invalid("unknown parameter display ID"))?;
            check(
                (param.min, param.max, param.step_count) == (0.0, 1.0, 0)
                    && (param.default - display.normalized_default()).abs()
                        <= f64::from(f32::EPSILON)
                    && self
                        .pages
                        .iter()
                        .flat_map(|page| &page.cells)
                        .any(|cell| cell.parameter_ids().any(|bound| bound == id)),
                "normalized parameter display",
            )?;
        }
        for cell in self.pages.iter().flat_map(|page| &page.cells) {
            let audition_note = cell.graph.as_ref().and_then(|graph| graph.audition_note);
            let spectrum_enabled = cell
                .graph
                .as_ref()
                .and_then(|graph| graph.slots.get("spectrum_enabled"))
                .copied();
            let fractional_trim = cell
                .graph
                .as_ref()
                .is_some_and(|graph| graph.trim_unit == Some(TrimUnit::SourceFraction));
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
                if Some(id) == audition_note {
                    check(
                        p.min == 0.0
                            && p.max == 127.0
                            && p.step_count == 127
                            && p.flags & param_flags::STEPPED != 0,
                        "waveform audition note range",
                    )?;
                }
                if Some(id) == spectrum_enabled {
                    check(
                        is_toggle(p) && p.flags & param_flags::ENUM == 0,
                        "spectrum switch requires a Boolean parameter",
                    )?;
                }
                if fractional_trim
                    && cell
                        .graph
                        .as_ref()
                        .is_some_and(|graph| graph.slots.values().any(|&slot| slot == id))
                {
                    check(
                        p.min == 0.0 && p.max == 1.0 && p.flags & param_flags::STEPPED == 0,
                        "waveform source fraction range",
                    )?;
                }
            }
            if let Some(driver) = cell.disabled_by {
                check(
                    ids[&driver].flags & param_flags::ENUM == 0,
                    "conditional control requires a Boolean parameter",
                )?;
            }
            let bindings = cell
                .param_id
                .into_iter()
                .map(|id| (id, cell.choices.as_deref()))
                .chain(
                    cell.variant_bindings()
                        .map(|binding| (binding.param_id, binding.choices.as_deref())),
                );
            for (id, choices) in bindings {
                let p = ids[&id];
                if cell.widget == Widget::ToggleButton {
                    check(is_toggle(p), "toggle range")?;
                }
                if let Some(choices) = choices {
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
        }
        Ok(())
    }

    /// Resolve exact language, primary language and then the document default.
    pub fn label<'a>(&self, labels: &'a Labels, requested: &str) -> Option<&'a str> {
        localized_label(labels, requested, &self.default_locale)
    }
}

fn is_toggle(p: &ParamInfo) -> bool {
    (p.min, p.max, p.step_count) == (0.0, 1.0, 1) && p.flags & param_flags::STEPPED != 0
}

fn choice_objects(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|choices| choices.iter().all(Value::is_object))
}

fn control_binding_object(value: &Value) -> bool {
    value.is_object()
        && value["param_id"].is_u64()
        && value.get("labels").is_none_or(Value::is_object)
        && value.get("choices").is_none_or(choice_objects)
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
    fn a_selector_switches_graph_and_control_bindings_in_its_page() {
        use serde_json::json;

        let mut value: Value = serde_json::from_slice(EXAMPLE).unwrap();
        value["schema_version"] = json!(2);
        value["pages"][0]["cells"] = json!([
            {"widget":"graph","col":0,"row":0,"cols":2,"rows":2,"half":"top","follows":"osc",
             "graph":{"kind":"wavetable","name":{"en":"Wavetable"},"description":{"en":"Wavetable"},
                      "variants":[{"slots":{"table":1,"position":2}},{"slots":{"table":3,"position":4}}]}},
            {"widget":"selector","col":0,"row":1,"cols":1,"rows":1,"half":"bottom",
             "selector":{"key":"osc","labels":{"en":"Oscillator"},"options":[{"en":"A"},{"en":"B"}]}},
            {"widget":"knob","col":0,"row":2,"cols":1,"rows":1,"follows":"osc",
             "variants":[{"param_id":2},{"param_id":4}]}
        ]);
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        assert!(parse(&value).is_err(), "selectors require a capability");
        value["required_capabilities"] = json!([CAP_CELL_SELECTIONS]);
        let layout = parse(&value).unwrap();
        let params: Vec<_> = (1..=4).map(|id| parameter(id, 1.0, 0, 0)).collect();
        assert!(layout.validate_params(&params).is_ok());
        assert!(layout.validate_params(&params[..3]).is_err());
        value["pages"][0]["cells"][1]["row"] = json!(0);
        assert!(parse(&value).is_err(), "the earlier row is full");
        value["pages"][0]["cells"][1]["row"] = json!(1);
        value["pages"][0]["cells"][2]["follows"] = json!("unknown");
        assert!(parse(&value).is_err(), "the selector belongs to this page");
        value["pages"][0]["cells"][2]["follows"] = json!("osc");
        value["pages"][0]["cells"][0]["graph"]["variants"][1]["slots"] = json!({"position":4});
        assert!(
            parse(&value).is_err(),
            "variants must have the same graph roles"
        );
    }

    #[test]
    fn a_selectable_eq_binds_controls_to_the_same_role_and_gates_its_spectrum() {
        use serde_json::json;

        let mut value: Value = serde_json::from_slice(EXAMPLE).unwrap();
        value["schema_version"] = json!(2);
        value["pages"][0]["columns"] = json!(2);
        value["pages"][0]["cells"] = json!([
            {"widget":"graph","col":0,"row":0,"cols":2,"rows":2,
             "graph":{"kind":"eq","name":{"en":"EQ"},"description":{"en":"EQ"},
                      "bands":2,"select_band":true,"signal_taps":{"spectrum":"output"},
                      "slots":{"b1_on":1,"b1_freq":2,"b1_gain":3,"b1_q":4,"b1_type":5,
                               "b2_on":6,"b2_freq":7,"b2_gain":8,"b2_q":9,"b2_type":10,
                               "spectrum_enabled":11}}},
            {"widget":"knob","col":0,"row":2,"cols":1,"rows":1,
             "band_controls":[{"param_id":2},{"param_id":7}]}
        ]);
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        assert!(parse(&value).is_err(), "selection requires the capability");
        value["required_capabilities"] = json!([CAP_EQ_BAND_CONTROLS]);
        let layout = parse(&value).unwrap();
        let mut params: Vec<_> = (1..=11)
            .map(|id| match id {
                1 | 6 | 11 => parameter(id, 1.0, 1, param_flags::STEPPED),
                5 | 10 => parameter(id, 4.0, 4, param_flags::STEPPED | param_flags::ENUM),
                _ => parameter(id, 1.0, 0, 0),
            })
            .collect();
        assert!(layout.validate_params(&params).is_ok());
        params[10].step_count = 0;
        params[10].flags = 0;
        assert!(layout.validate_params(&params).is_err());
        value["pages"][0]["cells"][1]["band_controls"][1]["param_id"] = json!(8);
        assert!(
            parse(&value).is_err(),
            "each band must use the same slot role"
        );
        value["pages"][0]["cells"][1]["band_controls"][1]["param_id"] = json!(7);
        value["pages"][0]["cells"][0]["graph"]["select_band"] = json!(false);
        assert!(
            parse(&value).is_err(),
            "band controls require a selected graph"
        );
    }

    #[test]
    fn a_conditional_knob_requires_a_distinct_boolean_toggle_on_the_same_page() {
        use serde_json::json;

        let mut value: Value = serde_json::from_slice(EXAMPLE).unwrap();
        value["pages"][0]["cells"] = json!([
            {"widget":"knob","col":0,"row":0,"cols":1,"rows":1,"param_id":42,"disabled_by":7},
            {"widget":"toggle_button","col":1,"row":0,"cols":1,"rows":1,"half":"top","param_id":7}
        ]);
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        assert_eq!(parse(&value).unwrap().pages[0].cells[0].disabled_by, None);
        value["schema_version"] = json!(2);
        assert!(parse(&value).is_err(), "the capability must be declared");
        value["required_capabilities"] = json!(["conditional-controls"]);
        let layout = parse(&value).unwrap();
        assert_eq!(layout.pages[0].cells[0].disabled_by, Some(7));
        let knob = parameter(42, 1.0, 0, 0);
        let mut toggle = parameter(7, 1.0, 1, param_flags::STEPPED);
        assert!(
            layout
                .validate_params(&[knob.clone(), toggle.clone()])
                .is_ok()
        );
        toggle.flags |= param_flags::ENUM;
        assert!(layout.validate_params(&[knob, toggle]).is_err());
        for invalid in [json!(42), json!(8), json!(null)] {
            value["pages"][0]["cells"][0]["disabled_by"] = invalid;
            assert!(parse(&value).is_err());
        }
    }

    #[test]
    fn physical_display_requires_a_normalized_parameter_with_the_same_default() {
        use serde_json::json;

        let mut value: Value = serde_json::from_slice(EXAMPLE).unwrap();
        value["parameter_display"] = json!({
            "42": {
                "min": 1, "max": 2000, "default": 375,
                "scale": "logarithmic", "unit": "milliseconds"
            }
        });
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        assert!(parse(&value).unwrap().parameter_display(42).is_none());
        value["schema_version"] = json!(2);
        assert!(parse(&value).is_err(), "older v2 hosts must fall back");
        value["required_capabilities"] = json!(["parameter-display"]);
        let layout = parse(&value).unwrap();
        let mut time = parameter(42, 1.0, 0, 0);
        time.default = 375.0_f64.ln() / 2000.0_f64.ln();
        let mode = parameter(7, 2.0, 2, param_flags::STEPPED | param_flags::ENUM);
        assert!(
            layout
                .validate_params(&[time.clone(), mode.clone()])
                .is_ok()
        );
        time.default = 0.5;
        assert!(
            layout
                .validate_params(&[time.clone(), mode.clone()])
                .is_err()
        );
        time.default = 375.0_f64.ln() / 2000.0_f64.ln();
        time.step_count = 1;
        assert!(layout.validate_params(&[time, mode]).is_err());
        value["parameter_display"]["42"]["min"] = json!(0);
        assert!(
            parse(&value).is_err(),
            "logarithmic bounds must be positive"
        );
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

    fn layout_with_waveform_asset() -> Value {
        let source = include_bytes!("../../../docs/clap-device-layout/example-v2.json");
        let mut value: Value = serde_json::from_slice(source).unwrap();
        value["pages"][0]["cells"][2]["graph"]["asset_slot"] = 5.into();
        value
    }

    #[test]
    fn asset_slots_are_numeric_waveform_bindings() {
        let mut value = layout_with_waveform_asset();
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            layout.pages[0].cells[2].graph.as_ref().unwrap().asset_slot,
            Some(5)
        );
        value["pages"][0]["cells"][0]["graph"]["asset_slot"] = 5.into();
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value["pages"][0]["cells"][0]["graph"]
            .as_object_mut()
            .unwrap()
            .remove("asset_slot");
        value["pages"][0]["cells"][2]["graph"]["asset_slot"] = (-1).into();
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn tap_bindings_match_the_graph_role_and_key_syntax() {
        let source = include_bytes!("../../../docs/clap-device-layout/example-v2.json");
        let mut value: Value = serde_json::from_slice(source).unwrap();
        let graph = value.pointer_mut("/pages/0/cells/0/graph").unwrap();
        graph["signal_taps"] = serde_json::json!({"spectrum": "main-output"});
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        let graph = layout.pages[0].cells[0].graph.as_ref().unwrap();
        assert_eq!(graph.signal_taps["spectrum"], "main-output");

        value["pages"][0]["cells"][0]["graph"]["signal_taps"] =
            serde_json::json!({"level": "main-output"});
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value["pages"][0]["cells"][0]["graph"]["signal_taps"] =
            serde_json::json!({"spectrum": "invalid key"});
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn gain_reduction_meter_requires_a_full_column_and_a_named_tap() {
        use serde_json::json;

        let document = json!({
            "schema_version": 2, "default_locale": "en", "locales": ["en"],
            "pages": [{"key": "main", "labels": {"en": "Main"}, "columns": 4, "cells": [
                {"widget": "meter_gr", "col": 3, "row": 0, "cols": 1, "rows": 3,
                 "reduction_tap": "limiter-gr"}
            ]}]
        });
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        let layout = parse(&document).unwrap();
        let meter = &layout.pages[0].cells[0];
        assert_eq!(meter.widget, Widget::MeterGr);
        assert_eq!(meter.reduction_tap.as_deref(), Some("limiter-gr"));
        assert!(layout.validate_params(&[]).is_ok());
        for (field, value) in [
            ("col", json!(4)),
            ("row", json!(1)),
            ("rows", json!(2)),
            ("cols", json!(2)),
            ("half", json!("top")),
            ("reduction_tap", json!("invalid key")),
            ("reduction_tap", Value::Null),
            ("param_id", json!(42)),
            ("labels", json!({"en": "Level"})),
        ] {
            let mut invalid = document.clone();
            invalid["pages"][0]["cells"][0][field] = value;
            assert!(parse(&invalid).is_err(), "{field}");
        }
        let mut invalid = document.clone();
        invalid["pages"][0]["cells"][0]
            .as_object_mut()
            .unwrap()
            .remove("reduction_tap");
        assert!(parse(&invalid).is_err());
        let mut invalid = document.clone();
        invalid["schema_version"] = 1.into();
        assert!(parse(&invalid).is_err());
        let mut invalid = document.clone();
        invalid["pages"][0]["cells"].as_array_mut().unwrap().push(
            json!({"widget": "number", "col": 3, "row": 2, "cols": 1, "rows": 1,
                   "half": "bottom", "param_id": 42}),
        );
        assert!(parse(&invalid).is_err());
    }

    #[test]
    fn pad_banks_require_one_shared_grid_and_distinct_bindings() {
        use serde_json::json;
        let grid = json!({"widget": "pad_grid", "col": 0, "row": 0, "cols": 2, "rows": 1});
        let toggle = json!({"widget":"toggle_button", "col": 0, "row": 1, "cols": 2,
                            "rows": 1, "half": "top", "param_id": 7});
        let document = json!({
            "schema_version": 2, "default_locale": "en", "locales": ["en", "de"],
            "required_capabilities": [CAP_PAD_BANK],
            "pad_bank": {
                "pads": [{"note": 36, "asset_slot": 4}, {"note": 42, "asset_slot": 9}],
                "activity_tap": "pad-activity", "follow_param": 7
            },
            "pages": [
                {"key": "left", "columns": 3, "labels": {"en": "Left", "de": "Links"},
                 "cells": [grid, toggle]},
                {"key": "right", "columns": 3, "labels": {"en": "Right", "de": "Rechts"},
                 "cells": [grid, toggle]}
            ]
        });
        let parse = |value: &Value| NativeLayout::parse(&serde_json::to_vec(value).unwrap());
        let layout = parse(&document).unwrap();
        assert!(
            layout
                .validate_params(&[parameter(7, 1.0, 1, param_flags::STEPPED)])
                .is_ok()
        );
        let bank = layout.pad_bank.unwrap();
        assert_eq!(
            bank.pads[1],
            Pad {
                note: 42,
                asset_slot: 9
            }
        );
        for (path, value) in [
            ("/required_capabilities", json!([])),
            ("/pad_bank/pads/1/note", json!(36)),
            ("/pad_bank/pads/1/note", json!(128)),
            ("/pad_bank/pads/1/asset_slot", json!(4)),
            ("/pad_bank/pads/0", json!([36, 4])),
            ("/pad_bank/activity_tap", json!("Invalid key")),
            ("/pad_bank/follow_param", json!(8)),
            ("/pages/1/cells/0/col", json!(1)),
            ("/pages/1/cells/0/rows", json!(2)),
        ] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(parse(&invalid).is_err(), "{path}");
        }
    }

    #[test]
    fn source_fraction_trim_requires_an_asset_and_continuous_unit_parameters() {
        let mut value = layout_with_waveform_asset();
        let graph = &mut value["pages"][0]["cells"][2]["graph"];
        graph["trim_unit"] = "source_fraction".into();
        graph["slots"] = serde_json::json!({"end": 42});
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value["required_capabilities"] = serde_json::json!([CAP_SOURCE_FRACTION_TRIM]);
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(layout.validate_params(&[parameter(42, 1.0, 0, 0)]).is_ok());
        assert!(
            layout
                .validate_params(&[parameter(42, 500.0, 0, 0)])
                .is_err()
        );
        assert!(
            layout
                .validate_params(&[parameter(42, 1.0, 1, param_flags::STEPPED)])
                .is_err()
        );
        value["pages"][0]["cells"][2]["graph"]
            .as_object_mut()
            .unwrap()
            .remove("asset_slot");
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn waveform_audition_requires_a_numeric_midi_note_control() {
        let mut value = layout_with_waveform_asset();
        value["pages"][0]["cells"][2]["graph"]["audition_note"] = 43.into();
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        let trim = parameter(42, 1.0, 0, 0);
        assert!(layout.validate_params(std::slice::from_ref(&trim)).is_err());
        assert!(
            layout
                .validate_params(&[trim.clone(), parameter(43, 127.0, 0, 0)])
                .is_err()
        );
        assert!(
            layout
                .validate_params(&[trim, parameter(43, 127.0, 127, param_flags::STEPPED)])
                .is_ok()
        );
    }

    #[test]
    fn waveform_markers_require_an_asset_slot() {
        let mut value = layout_with_waveform_asset();
        value["pages"][0]["cells"][2]["graph"]["waveform_markers"] = true.into();
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            layout.pages[0].cells[2]
                .graph
                .as_ref()
                .unwrap()
                .waveform_markers
        );
        value["pages"][0]["cells"][2]["graph"]
            .as_object_mut()
            .unwrap()
            .remove("asset_slot");
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn a_playhead_requires_a_waveform_with_an_asset_slot() {
        let mut value = layout_with_waveform_asset();
        value["pages"][0]["cells"][2]["graph"]["signal_taps"] =
            serde_json::json!({"playhead": "sample-position"});
        let layout = NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        let graph = layout.pages[0].cells[2].graph.as_ref().unwrap();
        assert_eq!(graph.signal_taps["playhead"], "sample-position");
        value["pages"][0]["cells"][2]["graph"]
            .as_object_mut()
            .unwrap()
            .remove("asset_slot");
        assert!(NativeLayout::parse(&serde_json::to_vec(&value).unwrap()).is_err());
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
        example["pages"][0]["cells"][1]["band_controls"] = serde_json::json!("ignored by v1");
        example["pages"][0]["cells"][1]["choices"][0]["full_labels"] =
            serde_json::json!("ignored by v1");
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
