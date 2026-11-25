//! Bounded display lists from a plugin control thread. Coordinates are relative to the canvas.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.device-display/1";
pub const MAX_BYTES: usize = 65_536;
pub const REFRESH: Duration = Duration::from_millis(80);
pub(crate) const MAX_CANVASES: usize = 16;
const MAX_PRIMITIVES: usize = 512;
const MAX_POINTS: usize = 1024;

pub type Point = [f32; 2];

/// Host design token roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Text,
    Muted,
    Accent,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Primitive {
    Line {
        from: Point,
        to: Point,
        tone: Tone,
    },
    Polyline {
        points: Vec<Point>,
        tone: Tone,
    },
    Rectangle {
        from: Point,
        to: Point,
        tone: Tone,
        filled: bool,
    },
    Circle {
        center: Point,
        radius: f32,
        tone: Tone,
        filled: bool,
    },
    Area {
        points: Vec<Point>,
        baseline: f32,
        tone: Tone,
    },
    Text {
        at: Point,
        text: String,
        alignment: Alignment,
        tone: Tone,
    },
}

impl Primitive {
    fn valid(&self) -> bool {
        fn unit(value: f32) -> bool {
            (0.0..=1.0).contains(&value)
        }
        fn point(p: Point) -> bool {
            p.into_iter().all(unit)
        }
        fn path(points: &[Point]) -> bool {
            (2..=MAX_POINTS).contains(&points.len()) && points.iter().copied().all(point)
        }
        match self {
            Self::Line { from, to, .. } | Self::Rectangle { from, to, .. } => {
                point(*from) && point(*to)
            }
            Self::Polyline { points, .. } => path(points),
            Self::Circle { center, radius, .. } => point(*center) && unit(*radius),
            Self::Area {
                points, baseline, ..
            } => unit(*baseline) && path(points) && points.windows(2).all(|p| p[0][0] <= p[1][0]),
            Self::Text { at, text, .. } => {
                point(*at) && text.len() <= 128 && !text.chars().any(char::is_control)
            }
        }
    }
}

pub(crate) fn canvas_key(key: &str) -> bool {
    (1..=32).contains(&key.len())
        && key.as_bytes()[0].is_ascii_lowercase()
        && key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Frame {
    pub key: String,
    pub primitives: Vec<Primitive>,
}

/// The widest routing the host FM diagram can represent.
pub const MAX_FM_OPERATORS: usize = 8;

/// A plugin's current FM routing for one named host-rendered diagram.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct FmFrame {
    pub key: String,
    /// A numbered algorithm, or `None` for free routing.
    pub algorithm: Option<u8>,
    /// One mask per operator. Bit `n` marks an edge to operator index `n`.
    pub modulates: Vec<u8>,
    /// Bit mask of the operators sent to the output, independent of modulation edges.
    pub outputs: u8,
    /// The operator with the feedback loop, if any, as a zero-based index.
    pub feedback: Option<u8>,
}

impl FmFrame {
    /// An operator contributes to the audible output.
    pub fn is_carrier(&self, index: usize) -> bool {
        index < self.modulates.len().min(MAX_FM_OPERATORS) && self.outputs & (1 << index) != 0
    }

    fn valid(&self) -> bool {
        let count = self.modulates.len();
        if !(1..=MAX_FM_OPERATORS).contains(&count) {
            return false;
        }
        let outside = !(u8::MAX >> (MAX_FM_OPERATORS - count));
        self.algorithm != Some(0)
            && self.feedback.is_none_or(|index| usize::from(index) < count)
            && self.outputs & outside == 0
            && self
                .modulates
                .iter()
                .enumerate()
                .all(|(index, &mask)| mask & (outside | (1 << index)) == 0)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct DisplayList {
    pub frames: Vec<Frame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fm_topologies: Vec<FmFrame>,
}

impl DisplayList {
    /// Validate limits after decoding plugin or bridge output.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return None;
        }
        let list: Self = serde_json::from_slice(bytes).ok()?;
        list.valid().then_some(list)
    }

    /// Serialize a valid list using the public wire format.
    pub fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= MAX_BYTES).then_some(bytes)
    }

    fn valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        self.frames.len().saturating_add(self.fm_topologies.len()) <= MAX_CANVASES
            && self
                .frames
                .iter()
                .map(|frame| frame.primitives.len())
                .sum::<usize>()
                <= MAX_PRIMITIVES
            && self.frames.iter().all(|frame| {
                canvas_key(&frame.key)
                    && keys.insert(&frame.key)
                    && frame.primitives.iter().all(Primitive::valid)
            })
            && self
                .fm_topologies
                .iter()
                .all(|frame| canvas_key(&frame.key) && keys.insert(&frame.key) && frame.valid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_list_round_trip_and_rejects_bad_geometry() {
        let mut list = DisplayList {
            frames: vec![Frame {
                key: "scope".to_owned(),
                primitives: vec![Primitive::Circle {
                    center: [0.5, 0.5],
                    radius: 0.25,
                    tone: Tone::Accent,
                    filled: true,
                }],
            }],
            fm_topologies: vec![],
        };
        assert_eq!(
            DisplayList::decode(&list.encode().unwrap()),
            Some(list.clone())
        );
        list.frames[0].primitives.push(Primitive::Line {
            from: [0.0, 0.1],
            to: [f32::NAN, 0.5],
            tone: Tone::Text,
        });
        assert!(list.encode().is_none());
        assert!(DisplayList::decode(&vec![b' '; MAX_BYTES + 1]).is_none());
        list.frames[0].primitives.pop();
        let duplicate = list.frames[0].clone();
        list.frames.push(duplicate);
        assert!(list.encode().is_none());
    }

    #[test]
    fn fm_routing_round_trips_and_rejects_malformed_operator_masks() {
        let mut list = DisplayList::decode(br#"{"frames":[]}"#).unwrap();
        assert!(list.fm_topologies.is_empty());
        list.fm_topologies.push(FmFrame {
            key: "routing".to_owned(),
            algorithm: None,
            modulates: vec![2, 1, 0],
            outputs: 5,
            feedback: Some(1),
        });
        assert_eq!(
            DisplayList::decode(&list.encode().unwrap()),
            Some(list.clone())
        );
        let frame = &list.fm_topologies[0];
        assert!(frame.is_carrier(0));
        assert!(!frame.is_carrier(1));
        assert!(frame.is_carrier(2));
        assert!(!frame.is_carrier(3));

        for fields in [
            r#""modulates":[],"outputs":0"#,
            r#""modulates":[0,0,0,0,0,0,0,0,0],"outputs":0"#,
            r#""modulates":[0,0],"outputs":4"#,
            r#""modulates":[4,0],"outputs":2"#,
            r#""modulates":[1,0],"outputs":2"#,
            r#""modulates":[0,0],"outputs":2,"feedback":2"#,
            r#""modulates":[0,0],"outputs":2,"algorithm":0"#,
        ] {
            let wire = format!(r#"{{"frames":[],"fm_topologies":[{{"key":"routing",{fields}}}]}}"#);
            assert!(DisplayList::decode(wire.as_bytes()).is_none(), "{wire}");
        }

        list.frames.push(Frame {
            key: "routing".into(),
            primitives: vec![],
        });
        assert!(list.encode().is_none());
    }
}
