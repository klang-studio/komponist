//! In-memory `IBStream` and komponist's VST3 state blob.
//!
//! # State blob (format version 1)
//!
//! A VST3 instance has two states: the component's (`IComponent::getState`, the processing side)
//! and the edit controller's (`IEditController::getState`, GUI-only settings). komponist keeps
//! both in one blob, all integers little-endian:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 4 | magic `KPV3` |
//! | 4 | 2 | format version, `1` |
//! | 6 | 2 | flags, `0` (reserved) |
//! | 8 | 16 | class id of the plugin that saved it, in the canonical byte order (`module::cid_hex`; a Windows `TUID` is byte-swapped, so a state moves between OSes) |
//! | 24 | 8 | component state length `C` |
//! | 32 | 8 | controller state length `K` |
//! | 40 | `C` | component state |
//! | 40 + `C` | `K` | controller state (empty for single-component plugins) |
//!
//! The blob is exactly `40 + C + K` bytes. Loading checks magic, version, lengths and class id.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
use vst3::Steinberg::{
    IBStream, IBStreamTrait, ISizeableStream, ISizeableStreamTrait, TUID, int32, int64, tresult,
};
use vst3::Steinberg::{kInvalidArgument, kResultOk};
use vst3::{Class, ComWrapper};

/// Magic at the start of a state blob.
pub(crate) const STATE_MAGIC: [u8; 4] = *b"KPV3";
/// Current state blob format version.
pub(crate) const STATE_VERSION: u16 = 1;
/// Bytes before the component state.
pub(crate) const STATE_HEADER: usize = 40;

/// A growable in-memory stream (like the SDK's `MemoryStream`). Used on one thread at a time, for
/// the duration of one `getState` / `setState` call.
#[derive(Debug, Default)]
pub(crate) struct MemoryStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl MemoryStream {
    /// A stream positioned at the start of `bytes`.
    pub(crate) fn with_bytes(bytes: &[u8]) -> MemoryStream {
        MemoryStream {
            data: RefCell::new(bytes.to_vec()),
            pos: Cell::new(0),
        }
    }
}

/// Runs `call` with a fresh empty stream and returns the bytes it wrote, or the failing result.
pub(crate) fn write_with(call: impl FnOnce(*mut IBStream) -> tresult) -> Result<Vec<u8>, tresult> {
    let stream = ComWrapper::new(MemoryStream::default());
    let ptr = stream.as_com_ref::<IBStream>().expect("IBStream").as_ptr();
    let result = call(ptr);
    if result != kResultOk {
        return Err(result);
    }
    Ok(stream.data.borrow().clone())
}

/// Runs `call` with a stream over `bytes`; its result.
pub(crate) fn read_with(bytes: &[u8], call: impl FnOnce(*mut IBStream) -> tresult) -> tresult {
    let stream = ComWrapper::new(MemoryStream::with_bytes(bytes));
    let ptr = stream.as_com_ref::<IBStream>().expect("IBStream").as_ptr();
    call(ptr)
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        if buffer.is_null() && num_bytes > 0 {
            return kInvalidArgument;
        }
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let take = (num_bytes.max(0) as usize).min(data.len() - pos);
        // SAFETY: the plugin's buffer holds `num_bytes` ≥ `take` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer.cast(), take) };
        self.pos.set(pos + take);
        if !num_read.is_null() {
            // SAFETY: valid out-pointer (checked for NULL).
            unsafe { *num_read = take as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_written: *mut int32,
    ) -> tresult {
        if buffer.is_null() && num_bytes > 0 {
            return kInvalidArgument;
        }
        let n = num_bytes.max(0) as usize;
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        // SAFETY: the plugin's buffer holds `n` bytes; `data` has room after the resize.
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), data.as_mut_ptr().add(pos), n)
        };
        self.pos.set(pos + n);
        if !num_written.is_null() {
            // SAFETY: valid out-pointer (checked for NULL).
            unsafe { *num_written = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        // The seek modes are `c_uint` constants outside Windows: compare as i64.
        let base = match i64::from(mode) {
            m if m == kIBSeekSet as i64 => 0,
            m if m == kIBSeekCur as i64 => self.pos.get() as i64,
            m if m == kIBSeekEnd as i64 => len,
            _ => return kInvalidArgument,
        };
        let new = base.saturating_add(pos).max(0);
        self.pos.set(new as usize);
        if !result.is_null() {
            // SAFETY: valid out-pointer (checked for NULL).
            unsafe { *result = new };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: valid out-pointer (checked for NULL).
        unsafe { *pos = self.pos.get() as int64 };
        kResultOk
    }
}

impl ISizeableStreamTrait for MemoryStream {
    unsafe fn getStreamSize(&self, size: *mut int64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: valid out-pointer (checked for NULL).
        unsafe { *size = self.data.borrow().len() as int64 };
        kResultOk
    }

    unsafe fn setStreamSize(&self, size: int64) -> tresult {
        if size < 0 {
            return kInvalidArgument;
        }
        self.data.borrow_mut().resize(size as usize, 0);
        kResultOk
    }
}

/// The parts of a decoded state blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StateParts<'a> {
    pub(crate) cid: TUID,
    pub(crate) component: &'a [u8],
    pub(crate) controller: &'a [u8],
}

/// Builds a state blob (format in the module docs).
pub(crate) fn encode_state(cid: &TUID, component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(STATE_HEADER + component.len() + controller.len());
    blob.extend_from_slice(&STATE_MAGIC);
    blob.extend_from_slice(&STATE_VERSION.to_le_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes());
    blob.extend_from_slice(&super::module::cid_to_canonical(cid));
    blob.extend_from_slice(&(component.len() as u64).to_le_bytes());
    blob.extend_from_slice(&(controller.len() as u64).to_le_bytes());
    blob.extend_from_slice(component);
    blob.extend_from_slice(controller);
    blob
}

/// Splits a state blob; the error says what is wrong with it.
pub(crate) fn decode_state(blob: &[u8]) -> Result<StateParts<'_>, String> {
    let header = blob
        .get(..STATE_HEADER)
        .ok_or_else(|| format!("{} bytes are too short for a VST3 state", blob.len()))?;
    if header[..4] != STATE_MAGIC {
        return Err("not a komponist VST3 state (magic)".into());
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != STATE_VERSION {
        return Err(format!("unsupported VST3 state version {version}"));
    }
    let cid = super::module::cid_from_canonical(header[8..24].try_into().unwrap());
    let len = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
    let (component_len, controller_len) = (len(24), len(32));
    let total = (STATE_HEADER as u64)
        .checked_add(component_len)
        .and_then(|n| n.checked_add(controller_len));
    if total != Some(blob.len() as u64) {
        return Err(format!(
            "VST3 state lengths {component_len} + {controller_len} do not match {} bytes",
            blob.len()
        ));
    }
    let (component, controller) = blob[STATE_HEADER..].split_at(component_len as usize);
    Ok(StateParts {
        cid,
        component,
        controller,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_char;
    use vst3::ComPtr;

    #[test]
    fn stream_reads_writes_and_seeks() {
        let stream = ComWrapper::new(MemoryStream::default());
        let s: ComPtr<IBStream> = stream.to_com_ptr().unwrap();
        let mut n = 0;
        let mut bytes = *b"hello";
        unsafe {
            assert_eq!(s.write(bytes.as_mut_ptr().cast(), 5, &mut n), kResultOk);
            assert_eq!(n, 5);
            let mut pos = 0;
            assert_eq!(
                s.seek(1, crate::vst3::enum_i32(kIBSeekSet), &mut pos),
                kResultOk
            );
            assert_eq!(pos, 1);
            let mut out = [0u8; 8];
            assert_eq!(s.read(out.as_mut_ptr().cast(), 8, &mut n), kResultOk);
            assert_eq!((n, &out[..4]), (4, &b"ello"[..]));
            // At the end: Ok with 0 bytes, like the SDK's MemoryStream.
            assert_eq!(s.read(out.as_mut_ptr().cast(), 8, &mut n), kResultOk);
            assert_eq!(n, 0);
            // Seek past the end, write: the gap is zero-filled.
            s.seek(2, crate::vst3::enum_i32(kIBSeekEnd), std::ptr::null_mut());
            let mut x = *b"x";
            s.write(x.as_mut_ptr().cast(), 1, std::ptr::null_mut());
            s.tell(&mut pos);
            assert_eq!(pos, 8);
            assert_eq!(
                s.seek(-3, crate::vst3::enum_i32(kIBSeekCur), &mut pos),
                kResultOk
            );
            assert_eq!(pos, 5);
            assert_eq!(s.seek(0, 7, &mut pos), kInvalidArgument);
            let sized: ComPtr<ISizeableStream> = s.cast().unwrap();
            let mut size = 0;
            sized.getStreamSize(&mut size);
            assert_eq!(size, 8);
        }
        drop(s);
        assert_eq!(stream.data.borrow().as_slice(), b"hello\0\0x");
    }

    #[test]
    fn write_and_read_helpers() {
        let bytes = write_with(|s| unsafe {
            let mut data = *b"state";
            let s = vst3::ComRef::from_raw(s).unwrap();
            s.write(data.as_mut_ptr().cast(), 5, std::ptr::null_mut())
        })
        .unwrap();
        assert_eq!(bytes, b"state");
        assert_eq!(
            write_with(|_| vst3::Steinberg::kResultFalse),
            Err(vst3::Steinberg::kResultFalse)
        );
        let mut seen = Vec::new();
        let result = read_with(b"abc", |s| unsafe {
            let mut out = [0u8; 4];
            let mut n = 0;
            let s = vst3::ComRef::from_raw(s).unwrap();
            s.read(out.as_mut_ptr().cast(), 4, &mut n);
            seen.extend_from_slice(&out[..n as usize]);
            kResultOk
        });
        assert_eq!((result, seen.as_slice()), (kResultOk, &b"abc"[..]));
    }

    #[test]
    fn state_blob_round_trip_and_errors() {
        let cid: TUID = b"kpTestSynth00001".map(|b| b as c_char);
        let blob = encode_state(&cid, b"component", b"ctl");
        assert_eq!(blob.len(), STATE_HEADER + 12);
        assert_eq!(&blob[..4], b"KPV3");
        let parts = decode_state(&blob).unwrap();
        assert_eq!(
            parts,
            StateParts {
                cid,
                component: b"component",
                controller: b"ctl"
            }
        );
        let empty = encode_state(&cid, b"", b"");
        assert_eq!(decode_state(&empty).unwrap().component, b"");

        assert!(decode_state(b"KPV3").is_err());
        assert!(decode_state(b"not a state at all, but long enough to have a header").is_err());
        let mut wrong_version = blob.clone();
        wrong_version[4] = 9;
        assert!(
            decode_state(&wrong_version)
                .unwrap_err()
                .contains("version")
        );
        assert!(decode_state(&blob[..blob.len() - 1]).is_err());
        let mut longer = blob.clone();
        longer.push(0);
        assert!(decode_state(&longer).is_err());
        let mut huge = blob;
        huge[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_state(&huge).is_err());
    }
}
