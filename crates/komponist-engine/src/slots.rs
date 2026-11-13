//! Processor slots and chain processing (ARCHITECTURE §6.2, §6.4 step 4).
//!
//! The engine owns a preallocated table of [`crate::MAX_SLOTS`] slots. `InsertProcessor` fills a
//! slot, `RemoveProcessor` empties it; programs refer to slots by [`SlotId`] in their chains. A
//! chain runs device by device on two ping-pong buffers. A device whose slot is empty, whose
//! processor failed, or which is bypassed is skipped: an instrument then leaves silence, an effect
//! passes its input through.

use komponist_model::DeviceRole;

use crate::processor::{Event, EventKind, ProcessContext, ProcessStatus, Processor, StereoIo};
use crate::program::ChainSlot;
use crate::sequencer::EventList;

/// Distinct parameters that can wait for one slot; further `SetParam`s are dropped and counted.
pub(crate) const MAX_PENDING_PARAMS: usize = 16;

/// Parameter changes waiting for the slot's next processor call. A newer value for the same id
/// replaces the queued one.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ParamQueue {
    items: [(u32, f64); MAX_PENDING_PARAMS],
    len: usize,
}

impl ParamQueue {
    /// Queues `id = value`; `false` when the queue is full.
    pub(crate) fn set(&mut self, id: u32, value: f64) -> bool {
        if let Some(item) = self.items[..self.len].iter_mut().find(|(i, _)| *i == id) {
            item.1 = value;
            return true;
        }
        if self.len == MAX_PENDING_PARAMS {
            return false;
        }
        self.items[self.len] = (id, value);
        self.len += 1;
        true
    }

    /// Forgets everything queued.
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    /// Moves the queued changes into `out` at offset 0 if `out` keeps at least `keep` free places
    /// afterwards; otherwise they wait for the next call.
    pub(crate) fn drain_into(&mut self, out: &mut EventList, keep: usize) {
        if self.len == 0 || out.room() < self.len + keep {
            return;
        }
        for &(id, value) in &self.items[..self.len] {
            out.push(0, EventKind::Param { id, value });
        }
        self.len = 0;
    }
}

/// One entry of the slot table.
pub(crate) struct Slot {
    /// The processor, if one is inserted.
    pub(crate) processor: Option<Box<dyn Processor>>,
    /// The processor returned `Failed` (or failed to re-activate): skipped until replaced.
    pub(crate) failed: bool,
    /// `ProcessorFailed` was sent (or the failure was reported otherwise).
    pub(crate) reported: bool,
    /// Bypass state of the slot in the current program (`reset` once when it turns on).
    pub(crate) bypassed: bool,
    /// `AllNotesOff` due at the next call.
    pub(crate) all_off: bool,
    /// `SetParam`s waiting for the next call.
    pub(crate) params: ParamQueue,
}

impl Slot {
    /// An empty slot.
    pub(crate) fn empty() -> Slot {
        Slot {
            processor: None,
            failed: false,
            reported: false,
            bypassed: false,
            all_off: false,
            params: ParamQueue::default(),
        }
    }

    /// Whether the processor runs at a chain position with bypass `bypass`.
    pub(crate) fn runs(&self, bypass: bool) -> bool {
        self.processor.is_some() && !self.failed && !bypass
    }
}

/// Which ping-pong buffer holds a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Buf {
    A,
    B,
}

/// Preallocated audio buffers of [`crate::MAX_BLOCK`] frames.
pub(crate) struct Bufs {
    /// Ping-pong buffers of chain processing, left and right.
    pub(crate) a: [Box<[f32]>; 2],
    pub(crate) b: [Box<[f32]>; 2],
    /// The master bus.
    pub(crate) bus: [Box<[f32]>; 2],
    /// Always zero: the input of instruments.
    pub(crate) silence: Box<[f32]>,
}

impl Bufs {
    /// Non-RT.
    pub(crate) fn new() -> Bufs {
        let buf = || vec![0.0; crate::MAX_BLOCK].into_boxed_slice();
        Bufs {
            a: [buf(), buf()],
            b: [buf(), buf()],
            bus: [buf(), buf()],
            silence: buf(),
        }
    }

    /// The first `n` frames of a ping-pong buffer, left and right.
    pub(crate) fn pair(&mut self, which: Buf, n: usize) -> (&mut [f32], &mut [f32]) {
        let [l, r] = match which {
            Buf::A => &mut self.a,
            Buf::B => &mut self.b,
        };
        (&mut l[..n], &mut r[..n])
    }
}

/// RT. Runs `chain` over the first `n` frames of `bufs.a` and returns the buffer with the result
/// and whether a processor failed in this call.
///
/// On a track (`track`), a first device with the instrument role is the instrument: it gets
/// `instrument_events` and silence as input. Its `AllNotesOff` and parameters are already in
/// `instrument_events`. Every other device is an effect and gets its own pending `AllNotesOff` and
/// parameters as events.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_chain(
    chain: &[ChainSlot],
    slots: &mut [Slot],
    ctx: &ProcessContext,
    instrument_events: &[Event],
    fx_events: &mut EventList,
    bufs: &mut Bufs,
    n: usize,
    track: bool,
) -> (Buf, bool) {
    let mut current = Buf::A;
    let mut failed = false;
    for (index, device) in chain.iter().enumerate() {
        let Some(slot) = slots.get_mut(device.slot.0 as usize) else {
            continue;
        };
        let instrument = track && index == 0 && device.role == DeviceRole::Instrument;
        let all_off = !instrument && core::mem::take(&mut slot.all_off);
        let runs = slot.runs(device.bypass);
        let Some(processor) = slot.processor.as_mut().filter(|_| runs) else {
            continue;
        };
        let events = if instrument {
            instrument_events
        } else {
            fx_events.clear();
            if all_off {
                fx_events.push(0, EventKind::AllNotesOff);
            }
            slot.params.drain_into(fx_events, 0);
            fx_events.finish()
        };
        let Bufs { a, b, silence, .. } = &mut *bufs;
        let (input, output) = match current {
            Buf::A => (&*a, b),
            Buf::B => (&*b, a),
        };
        let input = if instrument {
            [&silence[..n], &silence[..n]]
        } else {
            [&input[0][..n], &input[1][..n]]
        };
        let [out_l, out_r] = output;
        let mut io = StereoIo {
            input,
            output: [&mut out_l[..n], &mut out_r[..n]],
        };
        if processor.process(ctx, events, &mut io) == ProcessStatus::Failed {
            slot.failed = true;
            failed = true;
        }
        current = match current {
            Buf::A => Buf::B,
            Buf::B => Buf::A,
        };
    }
    (current, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_coalesce_per_id_and_wait_for_room() {
        let mut q = ParamQueue::default();
        assert!(q.set(1, 0.5));
        assert!(q.set(2, 1.0));
        assert!(q.set(1, 0.25));
        for id in 3..=MAX_PENDING_PARAMS as u32 {
            assert!(q.set(id, 0.0));
        }
        assert!(!q.set(99, 0.0));
        let mut out = EventList::new(MAX_PENDING_PARAMS + 1);
        // Two places must stay free: not enough room, the changes wait.
        q.drain_into(&mut out, 2);
        assert_eq!(out.room(), MAX_PENDING_PARAMS + 1);
        q.drain_into(&mut out, 1);
        let events = out.finish();
        assert_eq!(events.len(), MAX_PENDING_PARAMS);
        assert_eq!(events[0].kind, EventKind::Param { id: 1, value: 0.25 });
        assert!(q.set(99, 0.0));
    }
}
