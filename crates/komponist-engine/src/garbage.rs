//! The engine's end of the event queue, with a holding list for garbage (ARCHITECTURE §6.1, §6.3).
//!
//! Everything the engine retires (replaced programs, removed processors) travels to the UI thread
//! as [`EngineEvent::Retired`] and is dropped there. When the queue is full, retired objects wait in
//! a preallocated holding list and go out in later blocks; the engine never drops them itself.
//! Commands that retire something are applied only while the holding list has room
//! ([`Outbox::can_retire`]), so the list never overflows. Ordinary events are dropped (and
//! counted) when their capacity is exhausted. An additional reserve in the same FIFO keeps
//! MIDI recording transitions and loss notices ordered with the notes that made it through.

use core::sync::atomic::Ordering;

use komponist_model::{Tick, TrackId};

use crate::api::{EngineEvent, Garbage, Telemetry};

/// Retired objects that can wait for room in the event queue.
pub(crate) const HOLD_CAPACITY: usize = 256;

/// Extra event slots dedicated to capture transitions and loss notices. This fits a notice for
/// every track on two consecutive laps, plus the boundaries and an interruption notice.
pub(crate) const CAPTURE_RESERVE: usize = 2 * crate::MAX_TRACKS + 8;

/// The producer side of the engine → UI queue.
pub(crate) struct Outbox {
    queue: rtrb::Producer<EngineEvent>,
    hold: Vec<Garbage>,
    capture_reserve: usize,
    capture_overrun: Option<u32>,
}

impl Outbox {
    /// Non-RT. Wraps the queue and preallocates the holding list.
    pub(crate) fn new(queue: rtrb::Producer<EngineEvent>, capture_reserve: usize) -> Outbox {
        Outbox {
            queue,
            hold: Vec::with_capacity(HOLD_CAPACITY),
            capture_reserve,
            capture_overrun: None,
        }
    }

    fn has_capacity_above(&self, reserve: usize) -> bool {
        self.queue.cached_slots() > reserve || self.queue.slots() > reserve
    }

    fn has_ordinary_space(&self) -> bool {
        self.has_capacity_above(self.capture_reserve)
    }

    /// RT. Even on a stalled UI, leave room for a terminal interruption notice.
    pub(crate) fn can_send_capture(&self) -> bool {
        self.has_capacity_above(1)
    }

    /// RT. Gives reliable capture events precedence, in the same FIFO as captured notes.
    pub(crate) fn send_capture(&mut self, event: EngineEvent) -> bool {
        self.can_send_capture() && self.queue.push(event).is_ok()
    }

    /// RT. A dropped note marks its track once per lap. The slot for the previous lap remains
    /// relevant after a wrap because latency correction may place a late input there.
    pub(crate) fn send_recorded(
        &mut self,
        event: EngineEvent,
        track: TrackId,
        lap: u32,
        noted_loss: &mut [Option<u32>; 2],
        telemetry: &Telemetry,
    ) {
        if let Some(first) = &mut self.capture_overrun {
            *first = (*first).min(lap);
            telemetry.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.send(event, telemetry) {
            return;
        }
        let seen = &mut noted_loss[lap as usize % 2];
        if *seen != Some(lap) {
            if self.send_capture(EngineEvent::RecordingNoteLoss { track, lap }) {
                *seen = Some(lap);
            } else {
                self.capture_overrun = Some(lap);
            }
        }
    }

    pub(crate) fn capture_overrun(&self) -> bool {
        self.capture_overrun.is_some()
    }

    /// RT. Called only when a capture event could not fit. The final slot is reserved for this.
    pub(crate) fn interrupt_capture(&mut self, at: Tick, lap: u32) {
        let first_incomplete_lap = self
            .capture_overrun
            .take()
            .map_or(lap, |lost| lost.min(lap));
        let result = self.queue.push(EngineEvent::RecordingInterrupted {
            at,
            first_incomplete_lap,
        });
        debug_assert!(result.is_ok());
    }

    /// RT. Whether one more object can be retired without dropping it.
    pub(crate) fn can_retire(&self) -> bool {
        self.hold.len() < self.hold.capacity()
    }

    /// RT. Hands `garbage` to the UI thread: into the queue, or into the holding list while the
    /// queue is full. Callers check [`Outbox::can_retire`] first.
    pub(crate) fn retire(&mut self, garbage: Garbage) {
        let garbage = if self.has_ordinary_space() {
            match self.queue.push(EngineEvent::Retired(garbage)) {
                Ok(()) => return,
                Err(rtrb::PushError::Full(EngineEvent::Retired(garbage))) => garbage,
                Err(_) => unreachable!(),
            }
        } else {
            garbage
        };
        if self.can_retire() {
            self.hold.push(garbage);
        } else {
            // Unreachable while callers respect `can_retire`. Leaking is the lesser evil: a
            // drop here would free memory on the audio thread.
            debug_assert!(false, "garbage holding list overflow");
            core::mem::forget(garbage);
        }
    }

    /// RT. Sends a non-garbage event. When the queue is full the event is dropped, counted in
    /// [`Telemetry::dropped_events`], and `false` is returned.
    pub(crate) fn send(&mut self, event: EngineEvent, telemetry: &Telemetry) -> bool {
        debug_assert!(!matches!(event, EngineEvent::Retired(_)));
        let event = if self.has_ordinary_space() {
            match self.queue.push(event) {
                Ok(()) => return true,
                Err(rtrb::PushError::Full(event)) => event,
            }
        } else {
            event
        };
        if let EngineEvent::Retired(garbage) = event {
            // Not expected (see the assertion above), but never drop garbage here.
            self.retire(garbage);
        }
        telemetry.dropped_events.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// RT. Moves held garbage into the queue while there is room.
    pub(crate) fn flush(&mut self) {
        while !self.hold.is_empty() && self.has_ordinary_space() {
            if let Some(garbage) = self.hold.pop()
                && let Err(rtrb::PushError::Full(EngineEvent::Retired(garbage))) =
                    self.queue.push(EngineEvent::Retired(garbage))
            {
                self.hold.push(garbage);
                break;
            }
        }
    }

    /// Number of objects waiting in the holding list.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.hold.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::SlotId;
    use crate::testing::TestGain;

    fn processor(slot: u32) -> Garbage {
        Garbage::Processor(SlotId(slot), Box::new(TestGain::new(1.0)))
    }

    #[test]
    fn garbage_waits_while_the_queue_is_full() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(2);
        let telemetry = Telemetry::new();
        let mut outbox = Outbox::new(producer, 0);
        for slot in 0..5 {
            assert!(outbox.can_retire());
            outbox.retire(processor(slot));
        }
        assert_eq!(outbox.held(), 3);
        // A plain event is dropped and counted, garbage never.
        assert!(!outbox.send(
            EngineEvent::LoopWrapped {
                to: Default::default()
            },
            &telemetry
        ));
        assert_eq!(telemetry.dropped_events(), 1);
        let mut slots = Vec::new();
        for _ in 0..4 {
            while let Ok(event) = consumer.pop() {
                match event {
                    EngineEvent::Retired(Garbage::Processor(slot, _)) => slots.push(slot.0),
                    other => panic!("{other:?}"),
                }
            }
            outbox.flush();
        }
        slots.sort_unstable();
        assert_eq!(slots, [0, 1, 2, 3, 4]);
        assert_eq!(outbox.held(), 0);
    }

    #[test]
    fn the_holding_list_reports_when_it_is_full() {
        let (producer, _consumer) = rtrb::RingBuffer::new(1);
        let mut outbox = Outbox::new(producer, 0);
        let mut retired = 0;
        while outbox.can_retire() {
            outbox.retire(processor(retired));
            retired += 1;
        }
        assert_eq!(retired as usize, HOLD_CAPACITY + 1);
        assert_eq!(outbox.held(), HOLD_CAPACITY);
    }
}
