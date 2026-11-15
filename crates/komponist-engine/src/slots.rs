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

    /// Hands every queued change to `f` in queue order and forgets it (MIDI effect stages take
    /// their parameters this way, before their next call).
    pub(crate) fn drain_with(&mut self, mut f: impl FnMut(u32, f64)) {
        for &(id, value) in &self.items[..self.len] {
            f(id, value);
        }
        self.len = 0;
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

/// Access to slots by index, for [`run_chain`]: the engine's whole table on the calling thread, or
/// a [`SharedSlots`] view in a parallel job.
pub(crate) trait SlotTable {
    /// The slot at `index`, if the table has one.
    fn slot_mut(&mut self, index: usize) -> Option<&mut Slot>;
}

impl SlotTable for [Slot] {
    fn slot_mut(&mut self, index: usize) -> Option<&mut Slot> {
        self.get_mut(index)
    }
}

/// The engine's slot table seen by several parallel jobs at once (WP-65). Each job may only touch
/// the slots of its own chain; the engine checks when it installs a program that no slot appears
/// in two track chains ([`chains_exclusive`]) and runs the tracks one after another otherwise.
#[derive(Clone, Copy)]
pub(crate) struct SharedSlots {
    ptr: *mut Slot,
    len: usize,
}

// SAFETY: a `SharedSlots` is only created by `SharedSlots::new`, whose contract makes the slots a
// job reaches through it disjoint from every other job's; `Slot` is `Send`.
unsafe impl Send for SharedSlots {}
// SAFETY: as above.
unsafe impl Sync for SharedSlots {}

impl SharedSlots {
    /// # Safety
    /// While the view is used, `slots` must not be accessed otherwise, and every slot must be
    /// reached through at most one thread at a time (each parallel job only uses the slots of its
    /// own chain, and no slot is in two chains).
    pub(crate) unsafe fn new(slots: &mut [Slot]) -> SharedSlots {
        SharedSlots {
            ptr: slots.as_mut_ptr(),
            len: slots.len(),
        }
    }
}

impl SlotTable for SharedSlots {
    fn slot_mut(&mut self, index: usize) -> Option<&mut Slot> {
        // SAFETY: in bounds; exclusive per the contract of `SharedSlots::new`.
        (index < self.len).then(|| unsafe { &mut *self.ptr.add(index) })
    }
}

/// Whether no slot appears twice in the chains of `chains` (within one chain or across chains):
/// then their chains can run in parallel. Slots outside the table are ignored (never run).
pub(crate) fn chains_exclusive<'a>(chains: impl Iterator<Item = &'a [ChainSlot]>) -> bool {
    let mut seen = [0u64; crate::MAX_SLOTS / 64];
    for device in chains.flatten() {
        let index = device.slot.0 as usize;
        if index >= crate::MAX_SLOTS {
            continue;
        }
        let (word, bit) = (index / 64, 1u64 << (index % 64));
        if seen[word] & bit != 0 {
            return false;
        }
        seen[word] |= bit;
    }
    true
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

/// What a chain needs from the routing graph (WP-102), asked by [`run_chain`] while it runs.
pub(crate) trait ChainHook {
    /// Called once per call with the chain's signal in front of the first audio effect (or behind
    /// the last device when there is none): a MIDI track's head delay (§6.8.5).
    fn head(&mut self, l: &mut [f32], r: &mut [f32]);

    /// The sidechain input of the device at chain position `index` for this call, if it has one.
    /// Asked for every device that processes audio, running or not, so delay lines keep flowing.
    fn sidechain(&mut self, index: usize) -> Option<[&[f32]; 2]>;
}

/// RT. Runs `chain` over the first `n` frames of `bufs.a` and returns the buffer with the result
/// and whether a processor failed in this call.
///
/// On a track (`track`), the first device with the instrument role (behind the MIDI effects, which
/// are skipped) is the instrument: it gets
/// `instrument_events` and silence as input. Its `AllNotesOff` and parameters are already in
/// `instrument_events`. Every other device is an effect and gets its own pending `AllNotesOff` and
/// parameters as events. A device for which `hook` has a sidechain input gets it
/// ([`Processor::process_sidechain`](crate::Processor::process_sidechain)).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_chain<T: SlotTable + ?Sized, H: ChainHook + ?Sized>(
    chain: &[ChainSlot],
    slots: &mut T,
    ctx: &ProcessContext,
    instrument_events: &[Event],
    fx_events: &mut EventList,
    bufs: &mut Bufs,
    n: usize,
    track: bool,
    hook: &mut H,
) -> (Buf, bool) {
    let mut current = Buf::A;
    let mut failed = false;
    let mut head = false;
    for (index, device) in chain.iter().enumerate() {
        // MIDI effects run in front of the instrument (`crate::stage`), never as audio.
        if device.role == DeviceRole::MidiEffect {
            continue;
        }
        if device.role == DeviceRole::Effect && !head {
            head = true;
            let (l, r) = bufs.pair(current, n);
            hook.head(l, r);
        }
        let sidechain = hook.sidechain(index);
        let Some(slot) = slots.slot_mut(device.slot.0 as usize) else {
            continue;
        };
        let instrument = track
            && device.role == DeviceRole::Instrument
            && chain[..index]
                .iter()
                .all(|d| d.role == DeviceRole::MidiEffect);
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
        let status = match sidechain {
            Some(sidechain) => processor.process_sidechain(ctx, events, &mut io, sidechain),
            None => processor.process(ctx, events, &mut io),
        };
        if status == ProcessStatus::Failed {
            slot.failed = true;
            failed = true;
        }
        current = match current {
            Buf::A => Buf::B,
            Buf::B => Buf::A,
        };
    }
    if !head {
        let (l, r) = bufs.pair(current, n);
        hook.head(l, r);
    }
    (current, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::SlotId;

    fn chain(slots: &[u32]) -> Vec<ChainSlot> {
        slots
            .iter()
            .map(|&s| ChainSlot {
                device: komponist_model::DeviceId(u64::from(s)),
                slot: SlotId(s),
                role: DeviceRole::Effect,
                bypass: false,
            })
            .collect()
    }

    #[test]
    fn exclusive_chains() {
        let (a, b) = (chain(&[0, 5, 1023]), chain(&[6, 70, 5000, 5000]));
        assert!(chains_exclusive([&a[..], &b[..]].into_iter()));
        let c = chain(&[7, 70]);
        assert!(!chains_exclusive([&a[..], &b[..], &c[..]].into_iter()));
        let d = chain(&[3, 3]);
        assert!(!chains_exclusive([&d[..]].into_iter()));
    }

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
