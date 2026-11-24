//! Owned metadata for the native CLAP device-layout v1 contract. Used on control threads only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::control::{ParamInfo, param_flags};

/// The v1 maximum JSON document size, in bytes.
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

/// The supported v1 controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Widget {
    Knob,
    KnobLarge,
    Number,
    ToggleButton,
    Segment,
    Dropdown,
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
    pub param_id: u32,
    pub labels: Option<Labels>,
    pub choices: Option<Vec<Choice>>,
}

/// A page of native parameter controls.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Page {
    pub key: String,
    pub labels: Labels,
    pub columns: u8,
    pub cells: Vec<Cell>,
}

/// A parsed native v1 descriptor. The raw IPC payload is bounded and parsed independently on
/// both sides before use. Parameter bindings must also pass `validate_params` for the inventory.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NativeLayout {
    pub default_locale: String,
    locales: Vec<String>,
    pub pages: Vec<Page>,
    #[serde(default)]
    author_ids: Option<BTreeMap<String, u32>>,
    #[serde(skip)]
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
        if version != 1 {
            return Err(LayoutError::Unsupported);
        }
        let metadata = value
            .as_object_mut()
            .and_then(|root| root.remove("metadata"))
            .filter(|metadata| {
                metadata.is_object()
                    && metadata["names"].is_object()
                    && metadata.get("descriptions").is_none_or(Value::is_object)
            })
            .and_then(|metadata| serde_json::from_value::<Metadata>(metadata).ok());
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
            if !caps.is_empty() {
                return Err(LayoutError::Unsupported);
            }
        }
        // Serde also accepts positional structs and externally tagged enum objects. V1
        // requires named objects and string tags, while allowing unknown object members.
        if let Some(pages) = value["pages"].as_array() {
            for page in pages {
                check(page.is_object(), "page object")?;
                if let Some(cells) = page["cells"].as_array() {
                    for cell in cells {
                        check(cell.is_object(), "cell object")?;
                        check(cell["widget"].is_string(), "widget tag")?;
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
        let locales = layout
            .locales
            .iter()
            .map(|tag| tag.to_ascii_lowercase())
            .collect();
        layout.validate_geometry(&locales)?;
        layout.metadata = metadata.filter(|metadata| {
            layout
                .validate_labels(&metadata.names, &locales, true)
                .is_ok()
                && (metadata.descriptions.is_empty()
                    || layout
                        .validate_labels(&metadata.descriptions, &locales, true)
                        .is_ok())
        });
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

    /// Device names and descriptions for the scan catalog, when the layout provides valid labels.
    pub fn localized_plugin(&self) -> Option<LocalizedPlugin> {
        let metadata = self.metadata.as_ref()?;
        Some(LocalizedPlugin {
            default_locale: self.default_locale.clone(),
            names: metadata.names.clone(),
            descriptions: metadata.descriptions.clone(),
        })
    }

    fn validate_geometry(&self, locales: &BTreeSet<String>) -> Result<(), LayoutError> {
        check(
            (1..=8).contains(&self.locales.len())
                && self.locales.iter().all(|tag| locale_tag(tag))
                && locales.len() == self.locales.len()
                && locale_tag(&self.default_locale)
                && locales.contains(&self.default_locale.to_ascii_lowercase()),
            "locale tags",
        )?;
        check((1..=4).contains(&self.pages.len()), "page count")?;
        let mut keys = BTreeSet::new();
        for page in &self.pages {
            check(
                identifier(&page.key, false) && keys.insert(&page.key),
                "page key",
            )?;
            self.validate_labels(&page.labels, locales, true)?;
            check(
                (1..=12).contains(&page.columns) && (1..=72).contains(&page.cells.len()),
                "page dimensions",
            )?;
            let mut slots = [[0u8; 12]; 3];
            for cell in &page.cells {
                let (min, max, half) = match cell.widget {
                    Widget::Knob => (1, 1, false),
                    Widget::KnobLarge => (2, 2, false),
                    Widget::Number => (1, 1, true),
                    Widget::Dropdown => (2, 2, true),
                    Widget::Segment | Widget::ToggleButton => (1, 12, true),
                };
                check(
                    cell.rows == 1
                        && cell.row < 3
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
                for col in cell.col..cell.col + cell.cols {
                    let slot = &mut slots[usize::from(cell.row)][usize::from(col)];
                    check(*slot & mask == 0, "overlapping cells")?;
                    *slot |= mask;
                }
                if let Some(labels) = &cell.labels {
                    self.validate_labels(labels, locales, false)?;
                }
                if matches!(cell.widget, Widget::Segment | Widget::Dropdown) {
                    let choices = cell
                        .choices
                        .as_ref()
                        .ok_or(LayoutError::Invalid("missing choices"))?;
                    check((2..=16).contains(&choices.len()), "choice count")?;
                    for choice in choices {
                        self.validate_labels(&choice.labels, locales, true)?;
                    }
                } else {
                    check(cell.choices.is_none(), "unexpected choices")?;
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
            let p = ids
                .get(&cell.param_id)
                .ok_or(LayoutError::Invalid("unknown parameter ID"))?;
            check(
                p.flags & (param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS) == 0
                    && p.min.is_finite()
                    && p.max.is_finite()
                    && p.min < p.max,
                "unsupported parameter",
            )?;
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
