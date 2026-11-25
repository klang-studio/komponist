//! Optional, pure CLAP preparation of persisted state for user file edits.

use std::ffi::{CStr, c_void};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.device-asset-edit-state/1";
pub const MAX_EDITS_BYTES: usize = 256;
pub const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;

pub const REJECTED: u32 = 0;
pub const UNCHANGED: u32 = 1;
pub const REPLACED: u32 = 2;

/// Copies one complete replacement state into host-owned storage. Call at most once.
pub type WriteState = unsafe extern "C" fn(*mut c_void, *const u8, u32) -> bool;

/// Main-thread callback for a detached input state. It may run while audio is active.
#[repr(C)]
pub struct Extension {
    pub prepare: Option<
        unsafe extern "C" fn(
            plugin: *const c_void,
            edits: *const u8,
            edits_len: u32,
            state: *const u8,
            state_len: u32,
            output: *mut c_void,
            write: WriteState,
        ) -> u32,
    >,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Intent {
    Assign,
    Relink,
    Clear,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Edit {
    pub slot: u32,
    pub intent: Intent,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Edits {
    #[serde(deserialize_with = "super::bounded_slots")]
    pub slots: Vec<Edit>,
}

impl Edits {
    fn valid(&self) -> bool {
        super::unique_ids(&self.slots, |edit| edit.slot)
    }

    pub fn encode(&self, output: &mut [u8]) -> Option<usize> {
        super::encode(self, self.valid(), MAX_EDITS_BYTES, output)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        super::decode(bytes, MAX_EDITS_BYTES, Self::valid)
    }
}

pub fn valid_state(state: &[u8]) -> bool {
    (1..=MAX_STATE_BYTES).contains(&state.len())
}

/// Preparation was rejected, malformed or superseded.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Rejected;

impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("device asset edit rejected")
    }
}

impl std::error::Error for Rejected {}

/// Ok(None) means unchanged.
pub type Prepared = Result<Option<Vec<u8>>, Rejected>;

/// The bridge's owned state buffer, copied only after its borrowed payload passed the size check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State(Vec<u8>);

impl State {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl TryFrom<Vec<u8>> for State {
    type Error = Rejected;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        valid_state(&bytes).then_some(Self(bytes)).ok_or(Rejected)
    }
}

impl Serialize for State {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for State {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <&[u8]>::deserialize(deserializer)?;
        if !valid_state(bytes) {
            return Err(D::Error::custom("asset edit state size"));
        }
        Ok(Self(bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_require_distinct_slots_and_complete_bounded_input() {
        let edit = Edit {
            slot: u32::MAX,
            intent: Intent::Clear,
        };
        let mut bytes = [0; MAX_EDITS_BYTES];
        let edits = Edits {
            slots: (0..super::super::MAX_SLOTS as u32)
                .map(|slot| Edit {
                    slot: u32::MAX - slot,
                    ..edit
                })
                .collect(),
        };
        let len = edits.encode(&mut bytes).unwrap();
        assert_eq!(Edits::decode(&bytes[..len]), Some(edits));
        assert!(Edits::decode(&bytes[..=len]).is_none());
        assert!(
            Edits {
                slots: vec![edit, edit]
            }
            .encode(&mut bytes)
            .is_none()
        );
        assert!(Edits { slots: Vec::new() }.encode(&mut bytes).is_none());
    }

    #[test]
    fn state_size_is_independent_of_the_transport_limit() {
        assert!(!valid_state(&[]));
        let bytes = vec![0; MAX_STATE_BYTES + 1];
        assert!(valid_state(&bytes[..MAX_STATE_BYTES]));
        assert!(!valid_state(&bytes));
        let oversized = postcard::to_stdvec(&bytes).unwrap();
        assert!(postcard::from_bytes::<State>(&oversized).is_err());
        let empty = postcard::to_stdvec(&Vec::<u8>::new()).unwrap();
        assert!(postcard::from_bytes::<State>(&empty).is_err());
        let valid = State::try_from(vec![1, 2, 3]).unwrap();
        let encoded = postcard::to_stdvec(&valid).unwrap();
        assert_eq!(postcard::from_bytes::<State>(&encoded), Ok(valid));
    }
}
