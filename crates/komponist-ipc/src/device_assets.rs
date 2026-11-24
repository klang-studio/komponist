//! Bounded file-slot inventory, binding and offline preparation for CLAP devices.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::marker::PhantomData;

use serde::de::{Error, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.device-assets/1";
pub const MAX_SLOTS: usize = 32;
pub const MAX_LABEL_BYTES: usize = 128;
pub const MAX_ROLE_BYTES: usize = 32;
pub const MAX_ERROR_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024;
pub const MAX_BINDINGS_BYTES: usize = 132 * 1024;

/// Plugin-owned, immutable CLAP extension table. The opaque argument is a CLAP plugin pointer.
#[repr(C)]
pub struct Extension {
    pub snapshot: Option<unsafe extern "C" fn(*const c_void, *mut u8, u32) -> u32>,
    pub set_bindings: Option<unsafe extern "C" fn(*const c_void, *const u8, u32) -> u32>,
    pub prepare_offline: Option<unsafe extern "C" fn(*const c_void, u32, f64, f64) -> u32>,
}

/// Complete inventory in the plugin's display order. Slot IDs, roles and kinds are stable for
/// the initialized instance. Status may change.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    #[serde(deserialize_with = "bounded_slots")]
    pub slots: Vec<Slot>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Slot {
    /// Stable across sessions and compatible versions of the plugin.
    pub id: u32,
    /// Machine-readable role, such as `sample` or `impulse-response`.
    pub role: String,
    /// Display label supplied by the plugin.
    pub label: String,
    /// File kind used to choose the host's picker or accept file drops.
    pub kind: AssetKind,
    pub status: AssetStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum AssetKind {
    AudioSample,
    File,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum AssetStatus {
    Empty,
    Loading,
    Ready,
    Missing,
    Failed(String),
}

/// An atomic update to one or more existing slots. Omitted slots retain their bindings.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Bindings {
    #[serde(deserialize_with = "bounded_slots")]
    pub slots: Vec<Binding>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub id: u32,
    /// The host's resolved absolute file path, or `None` to clear the slot.
    pub path: Option<String>,
    /// Reload a source explicitly selected again by the user.
    pub reload: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct OfflineRequest {
    /// Nonzero identifier. Use a new identifier for each render, even at the same settings.
    pub id: u32,
    pub sample_rate: f64,
    pub tempo: f64,
}

impl OfflineRequest {
    pub fn valid(self) -> bool {
        self.id != 0
            && self.sample_rate.is_finite()
            && self.sample_rate > 0.0
            && self.tempo.is_finite()
            && self.tempo > 0.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum OfflineStatus {
    Ready = 1,
    Preparing = 2,
    Failed = 3,
}

impl OfflineStatus {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Ready,
            2 => Self::Preparing,
            _ => Self::Failed,
        }
    }
}

impl Snapshot {
    fn valid(&self) -> bool {
        unique_ids(&self.slots, |slot| slot.id)
            && self.slots.iter().all(|slot| {
                let role = slot.role.as_bytes();
                role.len() <= MAX_ROLE_BYTES
                    && role.first().is_some_and(u8::is_ascii_lowercase)
                    && role.iter().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'.' | b'_' | b'-')
                    })
                    && label_valid(&slot.label, MAX_LABEL_BYTES)
                    && match &slot.status {
                        AssetStatus::Failed(reason) => label_valid(reason, MAX_ERROR_BYTES),
                        _ => true,
                    }
            })
    }

    pub fn encode(&self, output: &mut [u8]) -> Option<usize> {
        encode(self, self.valid(), MAX_SNAPSHOT_BYTES, output)
    }

    pub fn decode(input: &[u8]) -> Option<Self> {
        decode(input, MAX_SNAPSHOT_BYTES, Self::valid)
    }
}

impl Bindings {
    pub fn valid(&self) -> bool {
        unique_ids(&self.slots, |slot| slot.id)
            && self.slots.iter().all(|slot| {
                (!slot.reload || slot.path.is_some())
                    && slot.path.as_ref().is_none_or(|path| {
                        !path.is_empty() && path.len() <= MAX_PATH_BYTES && !path.contains('\0')
                    })
            })
    }

    pub fn encode(&self, output: &mut [u8]) -> Option<usize> {
        encode(self, self.valid(), MAX_BINDINGS_BYTES, output)
    }

    pub fn decode(input: &[u8]) -> Option<Self> {
        decode(input, MAX_BINDINGS_BYTES, Self::valid)
    }
}

fn label_valid(value: &str, max: usize) -> bool {
    value.len() <= max && !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn unique_ids<T>(slots: &[T], id: impl Fn(&T) -> u32) -> bool {
    (1..=MAX_SLOTS).contains(&slots.len())
        && slots
            .iter()
            .enumerate()
            .all(|(index, slot)| slots[..index].iter().all(|other| id(slot) != id(other)))
}

fn encode<T: Serialize>(value: &T, valid: bool, max: usize, output: &mut [u8]) -> Option<usize> {
    if !valid {
        return None;
    }
    let len = output.len().min(max);
    postcard::to_slice(value, &mut output[..len])
        .ok()
        .map(|encoded| encoded.len())
}

fn decode<'a, T: Deserialize<'a>>(
    input: &'a [u8],
    max: usize,
    valid: impl FnOnce(&T) -> bool,
) -> Option<T> {
    if input.len() > max {
        return None;
    }
    let (value, remaining) = postcard::take_from_bytes(input).ok()?;
    (remaining.is_empty() && valid(&value)).then_some(value)
}

fn bounded_slots<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "at most {MAX_SLOTS} asset slots")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let capacity = seq.size_hint().unwrap_or_default();
            if capacity > MAX_SLOTS {
                return Err(A::Error::invalid_length(capacity, &self));
            }
            let mut slots = Vec::with_capacity(capacity);
            while let Some(slot) = seq.next_element()? {
                if slots.len() == MAX_SLOTS {
                    return Err(A::Error::invalid_length(MAX_SLOTS + 1, &self));
                }
                slots.push(slot);
            }
            Ok(slots)
        }
    }

    deserializer.deserialize_seq(Bounded(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(id: u32) -> Slot {
        Slot {
            id,
            role: "impulse-response".into(),
            label: "Impulse response".into(),
            kind: AssetKind::AudioSample,
            status: AssetStatus::Missing,
        }
    }

    #[test]
    fn snapshot_round_trip_rejects_trailing_bytes_and_duplicate_ids() {
        let mut snapshot = Snapshot {
            slots: vec![slot(42), slot(3)],
        };
        let mut output = [0; MAX_SNAPSHOT_BYTES + 1];
        let len = snapshot.encode(&mut output).unwrap();
        assert_eq!(Snapshot::decode(&output[..len]), Some(snapshot.clone()));
        assert_eq!(Snapshot::decode(&output[..len + 1]), None);

        snapshot.slots[1].id = 42;
        assert_eq!(snapshot.encode(&mut output), None);
        assert_eq!(
            Snapshot::decode(&postcard::to_stdvec(&snapshot).unwrap()),
            None
        );
    }

    #[test]
    fn binding_codec_bounds_slots_and_paths() {
        let mut bindings = Bindings {
            slots: vec![
                Binding {
                    id: 1,
                    path: Some("/asset/file.wav".into()),
                    reload: true,
                },
                Binding {
                    id: 2,
                    path: None,
                    reload: false,
                },
            ],
        };
        let mut output = [0; 128];
        let len = bindings.encode(&mut output).unwrap();
        assert_eq!(Bindings::decode(&output[..len]), Some(bindings.clone()));
        bindings.slots[1].reload = true;
        assert_eq!(bindings.encode(&mut output), None);
        bindings.slots[1].reload = false;
        bindings.slots[0].path = Some("invalid\0path".into());
        assert_eq!(bindings.encode(&mut output), None);

        bindings.slots = (0..=MAX_SLOTS)
            .map(|id| Binding {
                id: id as u32,
                path: None,
                reload: false,
            })
            .collect();
        assert_eq!(
            Bindings::decode(&postcard::to_stdvec(&bindings).unwrap()),
            None
        );
        assert_eq!(Bindings::decode(&vec![0; MAX_BINDINGS_BYTES + 1]), None);
    }

    #[test]
    fn preparation_arguments_and_unknown_status_are_validated() {
        let request = OfflineRequest {
            id: 9,
            sample_rate: 48_000.0,
            tempo: 110.0,
        };
        assert!(request.valid());
        assert!(!OfflineRequest { id: 0, ..request }.valid());
        assert!(
            !OfflineRequest {
                tempo: f64::NAN,
                ..request
            }
            .valid()
        );
        assert!(
            !OfflineRequest {
                sample_rate: -1.0,
                ..request
            }
            .valid()
        );
        assert_eq!(OfflineStatus::from_raw(0), OfflineStatus::Failed);
        assert_eq!(OfflineStatus::from_raw(2), OfflineStatus::Preparing);
        assert_eq!(OfflineStatus::from_raw(u32::MAX), OfflineStatus::Failed);
    }
}
