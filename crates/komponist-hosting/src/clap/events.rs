//! Conversion between komponist's [`WireEvent`]s and CLAP events, without allocation.
//!
//! - [`decode`] / [`encode`] follow the table on [`WireEvent`]. They mirror `WireEvent::decode` /
//!   `WireEvent::new`, which WP-16 implements in parallel; once those land, these two can forward
//!   to them.
//! - [`InputEventList`]: a fixed-capacity CLAP input event list built per block.
//! - [`WireSink`]: a CLAP output event list that turns parameter values and gestures into
//!   [`WireEvent`]s as the plugin pushes them.

use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamGestureBeginEvent, ParamGestureEndEvent,
    ParamValueEvent,
};
use clack_host::events::io::{InputEventBuffer, OutputEventBuffer, TryPushError};
use clack_host::prelude::*;
use komponist_ipc::{WireEvent, WireEventKind};

/// [`WireEvent::kind`] values (the table on [`WireEvent`]).
pub(crate) mod kind {
    pub const NOTE_ON: u32 = 1;
    pub const NOTE_OFF: u32 = 2;
    pub const MIDI: u32 = 3;
    pub const PARAM_VALUE: u32 = 4;
    pub const PARAM_GESTURE: u32 = 5;
    pub const ALL_NOTES_OFF: u32 = 6;
}

/// "No note id" in [`WireEvent::note_id`].
pub(crate) const NO_NOTE_ID: u32 = u32::MAX;

/// Decodes a wire event; `None` for an unknown kind or out-of-range MIDI data.
pub(crate) fn decode(e: &WireEvent) -> Option<WireEventKind> {
    let [a, b, c, _] = e.data;
    Some(match e.kind {
        kind::NOTE_ON if a < 16 && b < 128 && (1..128).contains(&c) => WireEventKind::NoteOn {
            channel: a,
            key: b,
            velocity: c,
            note_id: e.note_id,
        },
        kind::NOTE_OFF if a < 16 && b < 128 => WireEventKind::NoteOff {
            channel: a,
            key: b,
            note_id: e.note_id,
        },
        kind::MIDI => WireEventKind::Midi([a, b, c]),
        kind::PARAM_VALUE => WireEventKind::ParamValue {
            id: e.param_id,
            value: e.value,
        },
        kind::PARAM_GESTURE => WireEventKind::ParamGesture {
            id: e.param_id,
            begin: a != 0,
        },
        kind::ALL_NOTES_OFF => WireEventKind::AllNotesOff,
        _ => return None,
    })
}

/// Encodes `event` at frame `offset`.
pub(crate) fn encode(offset: u32, event: WireEventKind) -> WireEvent {
    let mut e = WireEvent {
        offset,
        note_id: NO_NOTE_ID,
        ..WireEvent::default()
    };
    match event {
        WireEventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        } => {
            e.kind = kind::NOTE_ON;
            e.data = [channel, key, velocity, 0];
            e.note_id = note_id;
        }
        WireEventKind::NoteOff {
            channel,
            key,
            note_id,
        } => {
            e.kind = kind::NOTE_OFF;
            e.data = [channel, key, 0, 0];
            e.note_id = note_id;
        }
        WireEventKind::Midi([a, b, c]) => {
            e.kind = kind::MIDI;
            e.data = [a, b, c, 0];
        }
        WireEventKind::ParamValue { id, value } => {
            e.kind = kind::PARAM_VALUE;
            e.param_id = id;
            e.value = value;
        }
        WireEventKind::ParamGesture { id, begin } => {
            e.kind = kind::PARAM_GESTURE;
            e.param_id = id;
            e.data = [u8::from(begin), 0, 0, 0];
        }
        WireEventKind::AllNotesOff => e.kind = kind::ALL_NOTES_OFF,
    }
    e
}

/// One CLAP input event of the kinds komponist sends.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InEvent {
    NoteOn(NoteOnEvent),
    NoteOff(NoteOffEvent),
    Midi(MidiEvent),
    Param(ParamValueEvent),
}

impl InEvent {
    fn as_unknown(&self) -> &UnknownEvent {
        match self {
            InEvent::NoteOn(e) => e.as_ref(),
            InEvent::NoteOff(e) => e.as_ref(),
            InEvent::Midi(e) => e.as_ref(),
            InEvent::Param(e) => e.as_ref(),
        }
    }
}

/// A CLAP input event list with a fixed capacity, filled per block without allocating. The
/// capacity covers a full input ring plus an expanded all-notes-off, so nothing is dropped in
/// practice.
#[derive(Debug)]
pub(crate) struct InputEventList {
    events: Vec<InEvent>,
}

impl InputEventList {
    pub(crate) fn with_capacity(capacity: usize) -> InputEventList {
        InputEventList {
            events: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.events.clear();
    }

    /// Appends `event` unless the list is full (then drops it).
    pub(crate) fn push(&mut self, event: InEvent) {
        if self.events.len() < self.events.capacity() {
            self.events.push(event);
        }
    }
}

impl InputEventBuffer for InputEventList {
    fn len(&self) -> u32 {
        self.events.len() as u32
    }

    fn get(&self, index: u32) -> Option<&UnknownEvent> {
        self.events.get(index as usize).map(InEvent::as_unknown)
    }
}

/// A CLAP parameter value event for wire parameter `id` with plain `value`; `None` for the
/// invalid id `u32::MAX`.
pub(crate) fn param_value_event(time: u32, id: u32, value: f64) -> Option<InEvent> {
    let id = ClapId::from_raw(id)?;
    Some(InEvent::Param(ParamValueEvent::new(
        time,
        id,
        Pckn::match_all(),
        value,
    )))
}

/// A CLAP output event list that hands every parameter value and gesture to a closure as a
/// [`WireEvent`] and accepts (ignores) everything else, such as note-end events.
pub(crate) struct WireSink<F: FnMut(WireEvent)>(pub F);

impl<F: FnMut(WireEvent)> OutputEventBuffer for WireSink<F> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let time = event.header().time();
        let wire = if let Some(e) = event.as_event::<ParamValueEvent>() {
            e.param_id().map(|id| {
                encode(
                    time,
                    WireEventKind::ParamValue {
                        id: id.get(),
                        value: e.value(),
                    },
                )
            })
        } else if let Some(e) = event.as_event::<ParamGestureBeginEvent>() {
            e.param_id().map(|id| {
                encode(
                    time,
                    WireEventKind::ParamGesture {
                        id: id.get(),
                        begin: true,
                    },
                )
            })
        } else if let Some(e) = event.as_event::<ParamGestureEndEvent>() {
            e.param_id().map(|id| {
                encode(
                    time,
                    WireEventKind::ParamGesture {
                        id: id.get(),
                        begin: false,
                    },
                )
            })
        } else {
            None
        };
        if let Some(wire) = wire {
            (self.0)(wire);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip() {
        let kinds = [
            WireEventKind::NoteOn {
                channel: 3,
                key: 60,
                velocity: 100,
                note_id: 7,
            },
            WireEventKind::NoteOff {
                channel: 3,
                key: 60,
                note_id: NO_NOTE_ID,
            },
            WireEventKind::Midi([0xB0, 7, 99]),
            WireEventKind::ParamValue {
                id: 42,
                value: 0.25,
            },
            WireEventKind::ParamGesture {
                id: 42,
                begin: true,
            },
            WireEventKind::ParamGesture {
                id: 42,
                begin: false,
            },
            WireEventKind::AllNotesOff,
        ];
        for (i, k) in kinds.into_iter().enumerate() {
            let e = encode(i as u32, k);
            assert_eq!(e.offset, i as u32);
            assert_eq!(decode(&e), Some(k));
        }
    }

    #[test]
    fn invalid_events_do_not_decode() {
        assert_eq!(decode(&WireEvent::default()), None);
        let bad_channel = WireEvent {
            kind: kind::NOTE_ON,
            data: [16, 60, 100, 0],
            ..WireEvent::default()
        };
        assert_eq!(decode(&bad_channel), None);
        let zero_velocity = WireEvent {
            kind: kind::NOTE_ON,
            data: [0, 60, 0, 0],
            ..WireEvent::default()
        };
        assert_eq!(decode(&zero_velocity), None);
    }

    #[test]
    fn input_list_drops_beyond_capacity() {
        let mut list = InputEventList::with_capacity(2);
        for _ in 0..3 {
            list.push(param_value_event(0, 1, 0.5).unwrap());
        }
        assert_eq!(InputEventBuffer::len(&list), 2);
        assert!(list.get(1).is_some() && list.get(2).is_none());
        assert!(param_value_event(0, u32::MAX, 0.0).is_none());
    }

    #[test]
    fn sink_converts_params_and_gestures() {
        let mut out = Vec::new();
        let mut sink = WireSink(|e| out.push(e));
        let id = ClapId::new(9);
        sink.try_push(ParamGestureBeginEvent::new(1, id).as_ref())
            .unwrap();
        sink.try_push(ParamValueEvent::new(2, id, Pckn::match_all(), 0.75).as_ref())
            .unwrap();
        sink.try_push(ParamGestureEndEvent::new(3, id).as_ref())
            .unwrap();
        sink.try_push(MidiEvent::new(0, 0, [0x90, 60, 1]).as_ref())
            .unwrap();
        let decoded: Vec<_> = out.iter().map(|e| (e.offset, decode(e).unwrap())).collect();
        assert_eq!(
            decoded,
            vec![
                (1, WireEventKind::ParamGesture { id: 9, begin: true }),
                (2, WireEventKind::ParamValue { id: 9, value: 0.75 }),
                (
                    3,
                    WireEventKind::ParamGesture {
                        id: 9,
                        begin: false
                    }
                ),
            ]
        );
    }
}
