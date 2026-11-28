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
use vst3::Steinberg::{kInvalidArgument, kOutOfMemory, kResultOk};
use vst3::{Class, ComWrapper};

/// Magic at the start of a state blob.
pub(crate) const STATE_MAGIC: [u8; 4] = *b"KPV3";
/// Current state blob format version.
pub(crate) const STATE_VERSION: u16 = 1;
/// Bytes before the component state.
pub(crate) const STATE_HEADER: usize = 40;

/// Largest stream a plugin can write or size (1 GiB): a state travels in one control message,
/// which is bounded the same way. Writes and `setStreamSize` past it fail with `kOutOfMemory`
/// instead of aborting the bridge on a failed allocation.
pub(crate) const MAX_STREAM_BYTES: usize = 1 << 30;

/// A growable in-memory stream (like the SDK's `MemoryStream`). Used on one thread at a time, for
/// the duration of one `getState` / `setState` call. Every argument comes from the plugin and is
/// checked: positions may lie anywhere in `0 ..= i64::MAX`, the data never grows past `limit`.
#[derive(Debug)]
pub(crate) struct MemoryStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
    /// Largest length the data may grow to ([`MAX_STREAM_BYTES`]; smaller in tests).
    limit: usize,
}

impl Default for MemoryStream {
    fn default() -> MemoryStream {
        MemoryStream::with_bytes(&[])
    }
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
            limit: MAX_STREAM_BYTES,
        }
    }

    /// The end of a write of `n` bytes at the current position, if the stream may grow to it.
    fn end_of_write(&self, n: usize) -> Option<usize> {
        self.pos
            .get()
            .checked_add(n)
            .filter(|&end| end <= self.limit)
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
        // A NULL buffer is allowed with nothing to copy, but no pointer argument of
        // `copy_nonoverlapping` may be NULL, not even for zero bytes.
        if take > 0 {
            // SAFETY: the plugin's buffer holds `num_bytes` ≥ `take` bytes and is not NULL
            // (checked above, as `take > 0` needs `num_bytes > 0`).
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer.cast(), take) };
        }
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
        // A position past the limit (a far seek) or a write growing the data past it is refused
        // before anything is allocated or written.
        let Some(end) = self.end_of_write(n) else {
            return kOutOfMemory;
        };
        let pos = self.pos.get();
        let mut data = self.data.borrow_mut();
        if data.len() < end {
            data.resize(end, 0);
        }
        if n > 0 {
            // SAFETY: the plugin's buffer holds `n` bytes and is not NULL (checked above, as
            // `n > 0`); `data` has room for `pos + n` bytes after the resize.
            unsafe {
                std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), data.as_mut_ptr().add(pos), n)
            };
        }
        self.pos.set(end);
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
            m if m == kIBSeekCur as i64 => i64::try_from(self.pos.get()).unwrap_or(i64::MAX),
            m if m == kIBSeekEnd as i64 => len,
            _ => return kInvalidArgument,
        };
        let new = base.saturating_add(pos).max(0);
        // Any non-negative position is kept (reads there give 0 bytes, writes check the limit).
        self.pos.set(usize::try_from(new).unwrap_or(usize::MAX));
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
        unsafe { *pos = i64::try_from(self.pos.get()).unwrap_or(i64::MAX) };
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
        match usize::try_from(size) {
            Ok(size) if size <= self.limit => {
                self.data.borrow_mut().resize(size, 0);
                kResultOk
            }
            _ => kOutOfMemory,
        }
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

/// Decodes `blob` as a state and checks the result: an error, or parts that make up the blob
/// exactly. Panics otherwise. For the proptests and the `vst3_state` fuzz target (#230).
pub(crate) fn check_state(blob: &[u8]) {
    if let Ok(parts) = decode_state(blob) {
        assert_eq!(
            STATE_HEADER + parts.component.len() + parts.controller.len(),
            blob.len()
        );
        // The same bytes again, but for the reserved flags, which readers ignore.
        let again = encode_state(&parts.cid, parts.component, parts.controller);
        assert_eq!(
            (&again[..6], &again[8..]),
            (&blob[..6], &blob[8..]),
            "a decoded state encodes to the same bytes"
        );
    }
}

/// Stream limit of [`exercise`]: small, so a run never allocates much.
const EXERCISE_LIMIT: usize = 64 * 1024;

/// Makes the `IBStream` / `ISizeableStream` calls encoded in `ops` on a stream over `initial`, as a
/// hostile plugin could make them (NULL buffers and out-pointers, negative counts, any seek), and
/// checks after each call that the stream stays within its limit and reports consistent positions.
/// Panics on a broken invariant. For the proptests and the `vst3_stream` fuzz target (#230).
pub(crate) fn exercise(initial: &[u8], ops: &[u8]) {
    let stream = MemoryStream {
        limit: EXERCISE_LIMIT,
        ..MemoryStream::with_bytes(&initial[..initial.len().min(EXERCISE_LIMIT)])
    };
    let mut ops = ops.iter().copied();
    let mut buffer = [0u8; 1024];
    let null = std::ptr::null_mut();
    while let Some(op) = ops.next() {
        let mut next = || ops.next().unwrap_or(0);
        // A count in -1024 ..= 1024 (the buffer is only this long: a plugin that passes a
        // shorter buffer than it claims is broken in a way no host can detect).
        let count = i32::from(i16::from_le_bytes([next(), next()])) % 1025;
        let before = stream.pos.get();
        // SAFETY: valid buffers of at least `count` bytes, or NULL; valid or NULL out-pointers.
        let result = unsafe {
            let mut n = 0;
            let mut pos = 0;
            match op % 9 {
                0 => stream.read(buffer.as_mut_ptr().cast(), count, &mut n),
                1 => stream.read(null, count, null.cast()),
                2 => stream.write(buffer.as_mut_ptr().cast(), count, &mut n),
                3 => stream.write(null, count, null.cast()),
                4 | 5 => {
                    let to = i64::from_le_bytes(std::array::from_fn(|_| next()));
                    let to = if op % 9 == 4 {
                        to
                    } else {
                        to % (2 * EXERCISE_LIMIT as i64)
                    };
                    let mode = i32::from(next() % 4);
                    let out = if next() & 1 == 0 {
                        &raw mut pos
                    } else {
                        null.cast()
                    };
                    stream.seek(to, mode, out)
                }
                6 => stream.tell(if count & 1 == 0 {
                    &raw mut pos
                } else {
                    null.cast()
                }),
                7 => {
                    let size = i64::from_le_bytes(std::array::from_fn(|_| next()));
                    let size = if count & 1 == 0 {
                        size
                    } else {
                        size % (2 * EXERCISE_LIMIT as i64)
                    };
                    stream.setStreamSize(size)
                }
                _ => stream.getStreamSize(if count & 1 == 0 {
                    &raw mut pos
                } else {
                    null.cast()
                }),
            }
        };
        let len = stream.data.borrow().len();
        assert!(len <= EXERCISE_LIMIT, "the stream grew to {len} bytes");
        match op % 9 {
            0 if count > 0 => {
                assert_eq!(result, kResultOk);
                let from = before.min(len);
                let expected = from + (count as usize).min(len - from);
                assert_eq!(stream.pos.get(), expected, "a read ends after what it read");
            }
            1 | 3 if count > 0 => assert_eq!(result, kInvalidArgument),
            2 if count > 0 => match before.checked_add(count as usize) {
                Some(end) if end <= EXERCISE_LIMIT => {
                    assert_eq!((result, stream.pos.get()), (kResultOk, end));
                }
                _ => assert_eq!((result, stream.pos.get()), (kOutOfMemory, before)),
            },
            _ => {}
        }
    }
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

    /// #230: a write after a seek to `i64::MAX` overflowed `pos + n` (wrapping in release, then a
    /// copy past the buffer); a size or write past the limit tried to allocate it.
    #[test]
    fn far_seeks_and_huge_sizes_are_refused() {
        let stream = MemoryStream::with_bytes(b"abc");
        let mut byte = *b"x";
        let mut pos = 0;
        unsafe {
            assert_eq!(
                stream.seek(i64::MAX, crate::vst3::enum_i32(kIBSeekSet), &mut pos),
                kResultOk
            );
            assert_eq!(pos, i64::MAX);
            assert_eq!(
                stream.write(byte.as_mut_ptr().cast(), 1, std::ptr::null_mut()),
                kOutOfMemory
            );
            // Relative to a far position, too: saturates instead of overflowing.
            assert_eq!(
                stream.seek(i64::MAX, crate::vst3::enum_i32(kIBSeekCur), &mut pos),
                kResultOk
            );
            assert_eq!(pos, i64::MAX);
            let mut out = [0u8; 4];
            let mut n = -1;
            assert_eq!(stream.read(out.as_mut_ptr().cast(), 4, &mut n), kResultOk);
            assert_eq!(n, 0);
            assert_eq!(stream.setStreamSize(i64::MAX), kOutOfMemory);
            assert_eq!(
                stream.setStreamSize(MAX_STREAM_BYTES as i64 + 1),
                kOutOfMemory
            );
            assert_eq!(stream.setStreamSize(-1), kInvalidArgument);
            assert_eq!(
                stream.seek(
                    MAX_STREAM_BYTES as i64,
                    crate::vst3::enum_i32(kIBSeekSet),
                    &mut pos
                ),
                kResultOk
            );
            assert_eq!(
                stream.write(byte.as_mut_ptr().cast(), 1, std::ptr::null_mut()),
                kOutOfMemory
            );
        }
        assert_eq!(stream.data.borrow().as_slice(), b"abc");
    }

    /// #230: a NULL buffer with a count of 0 (or less) reads and writes nothing; it used to reach
    /// `copy_nonoverlapping` with a NULL pointer, which is undefined behaviour even for 0 bytes.
    #[test]
    fn null_buffers_without_bytes_are_fine() {
        let stream = MemoryStream::with_bytes(b"abc");
        let null = std::ptr::null_mut();
        let mut n = -1;
        unsafe {
            assert_eq!(stream.read(null, 0, &mut n), kResultOk);
            assert_eq!(n, 0);
            assert_eq!(stream.read(null, -5, &mut n), kResultOk);
            assert_eq!(stream.write(null, 0, &mut n), kResultOk);
            assert_eq!(n, 0);
            assert_eq!(stream.read(null, 1, &mut n), kInvalidArgument);
            assert_eq!(stream.write(null, 1, &mut n), kInvalidArgument);
        }
        assert_eq!(stream.data.borrow().as_slice(), b"abc");
        assert_eq!(stream.pos.get(), 0);
    }

    proptest::proptest! {
        /// Whatever a plugin calls, the stream stays in bounds (the `vst3_stream` fuzz target).
        #[test]
        fn hostile_stream_calls_stay_in_bounds(
            initial in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            ops in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
        ) {
            exercise(&initial, &ops);
        }

        /// Any blob decodes to an error or to parts that fill it exactly (the `vst3_state` target).
        #[test]
        fn any_state_blob_is_checked(blob in proptest::collection::vec(proptest::num::u8::ANY, 0..128)) {
            check_state(&blob);
        }

        /// A valid blob with any lengths in its header: only the exact ones are accepted.
        #[test]
        fn state_lengths_must_add_up(
            component in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            controller in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            claimed in (proptest::num::u64::ANY, proptest::num::u64::ANY),
        ) {
            let cid: TUID = b"kpTestSynth00001".map(|b| b as c_char);
            let mut blob = encode_state(&cid, &component, &controller);
            blob[24..32].copy_from_slice(&claimed.0.to_le_bytes());
            blob[32..40].copy_from_slice(&claimed.1.to_le_bytes());
            // Any partition of the payload is a valid blob; only the total must match.
            let exact = (STATE_HEADER as u64)
                .checked_add(claimed.0)
                .and_then(|n| n.checked_add(claimed.1))
                == Some(blob.len() as u64);
            proptest::prop_assert_eq!(decode_state(&blob).is_ok(), exact);
            check_state(&blob);
        }
    }
}
