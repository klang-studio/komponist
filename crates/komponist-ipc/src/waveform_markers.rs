//! Bounded control-thread snapshots and edits for markers on a device's audio asset.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::marker::PhantomData;

use serde::de::{Error, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.waveform-markers/1";
pub const MAX_MARKERS: usize = 127;
pub const MAX_FADE_MS: f32 = 10_000.0;
pub const MAX_SNAPSHOT_BYTES: usize = 4096;
pub const MAX_EDIT_BYTES: usize = 32;
/// Bound each state in an edit reply so both fit in one message with room for encoding.
pub const MAX_STATE_BYTES: usize = (crate::control::MAX_MESSAGE - 64) / 2;

/// Plugin-owned immutable CLAP extension table. The opaque argument is a CLAP plugin pointer.
#[repr(C)]
pub struct Extension {
    pub snapshot: Option<unsafe extern "C" fn(*const c_void, u32, *mut u8, u32) -> u32>,
    pub edit: Option<unsafe extern "C" fn(*const c_void, u32, *const u8, u32) -> u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Snapshot {
    /// Instance- and slot-local token for the current list, asset binding and restored state.
    pub revision: u64,
    /// Whether the device is in marker editing mode and the source is ready.
    pub editable: bool,
    /// All existing boundaries in strictly increasing source seconds, including any outside
    /// the current trim or file. Indexes are meaningful only with this snapshot's revision.
    #[serde(deserialize_with = "bounded_list::<_, _, MAX_MARKERS>")]
    pub positions: Vec<f64>,
    /// When slice mode is active, one entry for the implicit first slice followed by an
    /// entry for the slice beginning at each stored marker.
    #[serde(deserialize_with = "bounded_slice_options")]
    pub slice_options: Option<Vec<SliceOptions>>,
    /// Resolved sample-wide direction and fades that slices with missing overrides inherit.
    /// Also available outside slice mode for displaying the sample's fades.
    pub default_options: Option<SliceOptions>,
    /// Readiness of a derived playback source, such as tempo-stretched audio.
    pub preparation: Option<Preparation>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Preparation {
    Preparing,
    Unavailable,
}

/// The saved settings of one slice. Missing direction and fades inherit the sample's settings.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SliceOptions {
    pub gate: bool,
    pub reverse: Option<bool>,
    pub fade_in_ms: Option<f32>,
    pub fade_out_ms: Option<f32>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Request {
    pub revision: u64,
    pub operation: Edit,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum Edit {
    Insert {
        seconds: f64,
    },
    Move {
        index: u8,
        seconds: f64,
    },
    Remove {
        index: u8,
    },
    /// Use slice 0 for the implicit first slice and index + 1 for a marker's slice.
    SetSliceOptions {
        slice: u8,
        options: SliceOptions,
    },
    /// Replace the interior markers with the device's detected transients inside the trim.
    DetectSlices,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum EditStatus {
    Invalid = 0,
    Applied = 1,
    Unchanged = 2,
    Stale = 3,
    Unavailable = 4,
}

impl EditStatus {
    pub fn from_raw(status: u32) -> Option<Self> {
        match status {
            0 => Some(Self::Invalid),
            1 => Some(Self::Applied),
            2 => Some(Self::Unchanged),
            3 => Some(Self::Stale),
            4 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Result of a control-plane edit with project state captured on both sides of the operation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum EditResult {
    Applied {
        previous_state: Vec<u8>,
        state: Vec<u8>,
    },
    Rejected(EditStatus),
    Failed {
        message: String,
        rollback_failed: bool,
    },
}

impl Snapshot {
    fn valid(&self) -> bool {
        self.positions.len() <= MAX_MARKERS
            && self.positions.iter().all(|&time| valid_position(time))
            && self.positions.windows(2).all(|pair| pair[0] < pair[1])
            && self.default_options.is_none_or(|defaults| {
                defaults.reverse.is_some()
                    && defaults.fade_in_ms.is_some()
                    && defaults.fade_out_ms.is_some()
                    && defaults.valid()
            })
            && self.slice_options.as_ref().is_none_or(|options| {
                self.default_options.is_some()
                    && options.len() == self.positions.len() + 1
                    && options.iter().all(SliceOptions::valid)
            })
    }

    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        encode(self, self.valid(), MAX_SNAPSHOT_BYTES, out)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        decode(bytes, MAX_SNAPSHOT_BYTES, Self::valid)
    }
}

impl SliceOptions {
    fn valid(&self) -> bool {
        [self.fade_in_ms, self.fade_out_ms]
            .into_iter()
            .flatten()
            .all(|ms| (0.0..=MAX_FADE_MS).contains(&ms))
    }
}

impl Request {
    fn valid(&self) -> bool {
        let valid_index = |index: u8| usize::from(index) < MAX_MARKERS;
        match self.operation {
            Edit::Insert { seconds } => valid_position(seconds),
            Edit::Move { index, seconds } => valid_index(index) && valid_position(seconds),
            Edit::Remove { index } => valid_index(index),
            Edit::SetSliceOptions { slice, options } => {
                usize::from(slice) <= MAX_MARKERS && options.valid()
            }
            Edit::DetectSlices => true,
        }
    }

    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        encode(self, self.valid(), MAX_EDIT_BYTES, out)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        decode(bytes, MAX_EDIT_BYTES, Self::valid)
    }
}

fn valid_position(seconds: f64) -> bool {
    seconds.is_finite() && seconds > 0.0
}

fn encode<T: Serialize>(value: &T, valid: bool, max: usize, out: &mut [u8]) -> Option<usize> {
    if !valid {
        return None;
    }
    let size = out.len().min(max);
    postcard::to_slice(value, &mut out[..size])
        .ok()
        .map(|bytes| bytes.len())
}

fn decode<'de, T: Deserialize<'de>>(
    bytes: &'de [u8],
    max: usize,
    valid: impl FnOnce(&T) -> bool,
) -> Option<T> {
    if bytes.len() > max {
        return None;
    }
    let (value, remaining) = postcard::take_from_bytes(bytes).ok()?;
    (remaining.is_empty() && valid(&value)).then_some(value)
}

fn bounded_slice_options<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<SliceOptions>>, D::Error> {
    const MAX_SLICES: usize = MAX_MARKERS + 1;
    #[derive(Deserialize)]
    struct Options(
        #[serde(deserialize_with = "bounded_list::<_, _, MAX_SLICES>")] Vec<SliceOptions>,
    );
    Option::<Options>::deserialize(deserializer).map(|options| options.map(|options| options.0))
}

fn bounded_list<'de, D, T, const MAX: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct List<T, const MAX: usize>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for List<T, MAX> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "at most {MAX} entries")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let count = seq.size_hint().unwrap_or_default();
            if count > MAX {
                return Err(A::Error::invalid_length(count, &self));
            }
            let mut entries = Vec::with_capacity(count);
            while let Some(entry) = seq.next_element()? {
                if entries.len() == MAX {
                    return Err(A::Error::invalid_length(MAX + 1, &self));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_seq(List::<T, MAX>(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_ordered_and_bounded() {
        let mut snapshot = Snapshot {
            revision: u64::MAX,
            editable: true,
            positions: (1..=MAX_MARKERS).map(|i| i as f64).collect(),
            slice_options: Some(vec![
                SliceOptions {
                    gate: true,
                    reverse: Some(true),
                    fade_in_ms: Some(0.0),
                    fade_out_ms: Some(MAX_FADE_MS),
                };
                MAX_MARKERS + 1
            ]),
            default_options: Some(SliceOptions {
                reverse: Some(false),
                fade_in_ms: Some(0.0),
                fade_out_ms: Some(0.0),
                ..Default::default()
            }),
            preparation: Some(Preparation::Preparing),
        };
        let mut out = [0; MAX_SNAPSHOT_BYTES];
        let len = snapshot.encode(&mut out).unwrap();
        assert_eq!(Snapshot::decode(&out[..len]), Some(snapshot.clone()));
        assert!(Snapshot::decode(&out[..len + 1]).is_none());
        snapshot.positions.push(128.0);
        assert!(Snapshot::decode(&postcard::to_stdvec(&snapshot).unwrap()).is_none());
        for positions in [vec![2.0, 1.0], vec![2.0, 2.0], vec![f64::INFINITY]] {
            snapshot.positions = positions;
            assert!(snapshot.encode(&mut out).is_none());
            assert!(Snapshot::decode(&postcard::to_stdvec(&snapshot).unwrap()).is_none());
        }
    }

    #[test]
    fn slice_settings_match_the_number_of_markers_and_bound_each_fade() {
        let mut snapshot = Snapshot {
            revision: 1,
            editable: false,
            positions: vec![0.5],
            slice_options: None,
            default_options: None,
            preparation: None,
        };
        let mut out = [0; MAX_SNAPSHOT_BYTES];
        let len = snapshot.encode(&mut out).unwrap();
        assert_eq!(Snapshot::decode(&out[..len]), Some(snapshot.clone()));
        snapshot.default_options = Some(SliceOptions {
            reverse: Some(false),
            fade_in_ms: Some(0.0),
            fade_out_ms: Some(MAX_FADE_MS),
            ..Default::default()
        });
        let len = snapshot.encode(&mut out).unwrap();
        assert_eq!(Snapshot::decode(&out[..len]), Some(snapshot.clone()));
        for options in [
            vec![SliceOptions::default(); MAX_MARKERS + 2],
            vec![SliceOptions::default(); 1],
            vec![
                SliceOptions::default(),
                SliceOptions {
                    fade_in_ms: Some(f32::INFINITY),
                    ..Default::default()
                },
            ],
        ] {
            snapshot.slice_options = Some(options);
            assert!(snapshot.encode(&mut out).is_none());
            assert!(Snapshot::decode(&postcard::to_stdvec(&snapshot).unwrap()).is_none());
        }
    }

    #[test]
    fn edit_requires_one_complete_valid_operation() {
        let mut request = Request {
            revision: u64::MAX,
            operation: Edit::Move {
                index: 126,
                seconds: 0.000_000_1,
            },
        };
        let mut out = [0; MAX_EDIT_BYTES];
        let len = request.encode(&mut out).unwrap();
        assert_eq!(Request::decode(&out[..len]), Some(request));
        assert!(Request::decode(&out[..len - 1]).is_none());
        assert!(Request::decode(&out[..len + 1]).is_none());
        for operation in [
            Edit::SetSliceOptions {
                slice: 127,
                options: SliceOptions {
                    gate: true,
                    reverse: Some(false),
                    fade_in_ms: Some(MAX_FADE_MS),
                    fade_out_ms: Some(0.0),
                },
            },
            Edit::DetectSlices,
        ] {
            request.operation = operation;
            let len = request.encode(&mut out).unwrap();
            assert_eq!(Request::decode(&out[..len]), Some(request));
        }
        for operation in [
            Edit::Remove { index: 127 },
            Edit::Insert { seconds: f64::NAN },
            Edit::SetSliceOptions {
                slice: 128,
                options: SliceOptions::default(),
            },
            Edit::SetSliceOptions {
                slice: 0,
                options: SliceOptions {
                    fade_out_ms: Some(MAX_FADE_MS + 1.0),
                    ..Default::default()
                },
            },
            Edit::SetSliceOptions {
                slice: 0,
                options: SliceOptions {
                    fade_in_ms: Some(-0.01),
                    ..Default::default()
                },
            },
        ] {
            request.operation = operation;
            assert!(request.encode(&mut out).is_none());
            assert!(Request::decode(&postcard::to_stdvec(&request).unwrap()).is_none());
        }
    }
}
