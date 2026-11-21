//! Conversion between komponist's [`WireEvent`]s and CLAP events, without allocation.
//!
//! Wire events are decoded and built with komponist-ipc's [`WireEvent::decode`] /
//! [`WireEvent::new`].
//!
//! - [`InputEventList`]: a fixed-capacity CLAP input event list built per block.
//! - [`WireSink`]: a CLAP output event list that turns note, MIDI and parameter events into
//!   [`WireEvent`]s as the plugin pushes them.

use clack_host::events::event_types::{
    MidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent,
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_host::events::io::{InputEventBuffer, OutputEventBuffer, TryPushError};
use clack_host::events::{Match, Pckn};
use clack_host::prelude::*;
use komponist_ipc::{WireEvent, WireEventKind};

/// "No note id" in [`WireEvent::note_id`] (the table on [`WireEvent`]).
pub(crate) const NO_NOTE_ID: u32 = u32::MAX;

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

/// A bounded CLAP output sink. Note and MIDI output is accepted on the first note port of
/// the MIDI effects we host; parameter flush has no downstream note stage.
pub(crate) struct WireSink<F: FnMut(WireEvent) -> bool> {
    sink: F,
    frames: Option<u32>,
    note_output: bool,
    last_offset: u32,
}

impl<F: FnMut(WireEvent) -> bool> WireSink<F> {
    pub(crate) fn process(frames: u32, note_output: bool, sink: F) -> Self {
        Self {
            sink,
            frames: Some(frames),
            note_output,
            last_offset: 0,
        }
    }

    pub(crate) fn flush(sink: F) -> Self {
        Self {
            frames: None,
            ..Self::process(0, false, sink)
        }
    }
}

impl<F: FnMut(WireEvent) -> bool> OutputEventBuffer for WireSink<F> {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let time = self.frames.map_or(0, |_| event.header().time());
        let wire = if let Some(e) = event.as_event::<ParamValueEvent>() {
            e.param_id().map(|id| {
                WireEvent::new(
                    time,
                    WireEventKind::ParamValue {
                        id: id.get(),
                        value: e.value(),
                    },
                )
            })
        } else if let Some(e) = event.as_event::<ParamGestureBeginEvent>() {
            e.param_id().map(|id| {
                WireEvent::new(
                    time,
                    WireEventKind::ParamGesture {
                        id: id.get(),
                        begin: true,
                    },
                )
            })
        } else if let Some(e) = event.as_event::<ParamGestureEndEvent>() {
            e.param_id().map(|id| {
                WireEvent::new(
                    time,
                    WireEventKind::ParamGesture {
                        id: id.get(),
                        begin: false,
                    },
                )
            })
        } else if self.note_output {
            note_output(event)?
        } else {
            None
        };
        if let Some(wire) = wire {
            if self.frames.is_some_and(|frames| time >= frames)
                || time < self.last_offset
                || !(self.sink)(wire)
            {
                return Err(TryPushError::new());
            }
            self.last_offset = time;
        }
        Ok(())
    }
}

/// Convert the host's first note output port. The engine represents exact channel/key pairs
/// and an optional CLAP note ID. Reject unrepresentable wildcard notes instead of retargeting them.
fn note_output(event: &UnknownEvent) -> Result<Option<WireEvent>, TryPushError> {
    let kind = if let Some(note) = event.as_event::<NoteOnEvent>() {
        let (channel, key, note_id) = note_address(note.pckn())?;
        let velocity = note.velocity();
        if !(0.0..=1.0).contains(&velocity) {
            return Err(TryPushError::new());
        }
        // CLAP permits a zero-velocity note-on; MIDI 1.0 reserves velocity zero for note-off.
        let velocity = (velocity * 127.0).round().max(1.0) as u8;
        WireEventKind::NoteOn {
            channel,
            key,
            velocity,
            note_id,
        }
    } else if let Some(note) = event.as_event::<NoteOffEvent>() {
        let (channel, key, note_id) = note_address(note.pckn())?;
        WireEventKind::NoteOff {
            channel,
            key,
            note_id,
        }
    } else if let Some(pressure) = event.as_event::<NoteExpressionEvent>()
        && pressure.expression_type() == Some(NoteExpressionType::Pressure)
    {
        let address = pressure.pckn();
        let value = pressure.value();
        if !(0.0..=1.0).contains(&value) {
            return Err(TryPushError::new());
        }
        let value = (value * 127.0).round() as u8;
        if let Pckn {
            port_index: Match::Specific(0),
            channel: Match::Specific(channel),
            key: Match::All,
            note_id: Match::All,
        } = address
            && channel < 16
        {
            WireEventKind::Midi([0xd0 | channel as u8, value, 0])
        } else {
            let (channel, key, _) = note_address(address)?;
            WireEventKind::Midi([0xa0 | channel, key, value])
        }
    } else if let Some(midi) = event.as_event::<MidiEvent>() {
        let [status, data1, data2] = midi.data();
        if midi.port_index() != 0 || !(0x80..0xf0).contains(&status) || data1 >= 128 || data2 >= 128
        {
            return Err(TryPushError::new());
        }
        let channel = status & 0x0f;
        match status & 0xf0 {
            0x90 if data2 != 0 => WireEventKind::NoteOn {
                channel,
                key: data1,
                velocity: data2,
                note_id: NO_NOTE_ID,
            },
            0x80 | 0x90 => WireEventKind::NoteOff {
                channel,
                key: data1,
                note_id: NO_NOTE_ID,
            },
            _ => WireEventKind::Midi([status, data1, data2]),
        }
    } else {
        // Note-end is voice bookkeeping for instruments, not downstream MIDI.
        return Ok(None);
    };
    Ok(Some(WireEvent::new(event.header().time(), kind)))
}

fn note_address(address: Pckn) -> Result<(u8, u8, u32), TryPushError> {
    let (Match::Specific(0), Match::Specific(channel), Match::Specific(key)) =
        (address.port_index, address.channel, address.key)
    else {
        return Err(TryPushError::new());
    };
    if channel >= 16 || key >= 128 {
        return Err(TryPushError::new());
    }
    Ok((
        channel as u8,
        key as u8,
        address.note_id.into_specific().unwrap_or(NO_NOTE_ID),
    ))
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
        // The layer relies on komponist-ipc's encoding round-tripping every kind it handles.
        for (i, k) in kinds.into_iter().enumerate() {
            let e = WireEvent::new(i as u32, k);
            assert_eq!(e.offset, i as u32);
            assert_eq!(e.decode(), Some(k));
        }
        assert_eq!(WireEvent::default().decode(), None);
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
    fn midi_output_preserves_ids_offsets_and_equal_time_order() {
        let mut slots = [WireEvent::default(); 4];
        let mut output = crate::EventOut::new(&mut slots);
        let mut sink = WireSink::process(16, true, |event| output.push(event));
        let addressed = Pckn::from_raw(0, 3, 60, 42);
        let on = NoteOnEvent::new(2, addressed, 0.5);
        let off = NoteOffEvent::new(8, addressed, 0.0);
        sink.try_push(on.as_ref()).unwrap();
        assert!(
            sink.try_push(NoteOffEvent::new(1, addressed, 0.0).as_ref())
                .is_err()
        );
        assert!(
            sink.try_push(NoteOffEvent::new(16, addressed, 0.0).as_ref())
                .is_err()
        );
        assert!(
            sink.try_push(NoteOnEvent::new(2, Pckn::match_all(), 1.0).as_ref())
                .is_err()
        );
        sink.try_push(ParamGestureBeginEvent::new(2, ClapId::new(9)).as_ref())
            .unwrap();
        sink.try_push(MidiEvent::new(8, 0, [0x95, 65, 0]).as_ref())
            .unwrap();
        sink.try_push(off.as_ref()).unwrap();
        assert!(
            sink.try_push(MidiEvent::new(9, 0, [0xb0, 1, 64]).as_ref())
                .is_err()
        );
        assert_eq!(output.dropped(), 1);
        let events: Vec<_> = output
            .as_slice()
            .iter()
            .map(|wire| (wire.offset, wire.decode().unwrap()))
            .collect();
        assert_eq!(
            events,
            [
                (
                    2,
                    WireEventKind::NoteOn {
                        channel: 3,
                        key: 60,
                        velocity: 64,
                        note_id: 42
                    }
                ),
                (2, WireEventKind::ParamGesture { id: 9, begin: true }),
                (
                    8,
                    WireEventKind::NoteOff {
                        channel: 5,
                        key: 65,
                        note_id: NO_NOTE_ID
                    }
                ),
                (
                    8,
                    WireEventKind::NoteOff {
                        channel: 3,
                        key: 60,
                        note_id: 42
                    }
                ),
            ]
        );
    }

    #[test]
    fn midi_note_pressure_survives_the_clap_expression_encoding() {
        let mut received = None;
        let mut sink = WireSink::process(16, true, |event| {
            received = Some(event);
            true
        });
        let event = NoteExpressionEvent::new(
            11,
            Pckn::from_raw(0, 2, 61, -1),
            NoteExpressionType::Pressure,
            71.0 / 127.0,
        );
        sink.try_push(event.as_ref()).unwrap();
        assert_eq!(
            received,
            Some(WireEvent::new(11, WireEventKind::Midi([0xa2, 61, 71])))
        );
    }

    #[test]
    fn wildcard_key_and_note_id_pressure_routes_to_the_addressed_channel() {
        let mut received = None;
        let mut sink = WireSink::process(16, true, |event| {
            received = Some(event);
            true
        });
        let event = NoteExpressionEvent::new(
            11,
            Pckn::from_raw(0, 2, -1, -1),
            NoteExpressionType::Pressure,
            71.0 / 127.0,
        );
        sink.try_push(event.as_ref()).unwrap();
        assert_eq!(
            received,
            Some(WireEvent::new(11, WireEventKind::Midi([0xd2, 71, 0])))
        );
    }

    #[test]
    fn parameter_flush_ignores_midi_and_sample_offsets() {
        let mut events = Vec::new();
        let mut sink = WireSink::flush(|event| {
            events.push(event);
            true
        });
        sink.try_push(MidiEvent::new(3, 0, [0xb0, 1, 64]).as_ref())
            .unwrap();
        let id = ClapId::new(4);
        sink.try_push(ParamValueEvent::new(50, id, Pckn::match_all(), 0.4).as_ref())
            .unwrap();
        assert_eq!(
            events,
            [WireEvent::new(
                0,
                WireEventKind::ParamValue { id: 4, value: 0.4 }
            )]
        );
    }

    #[test]
    fn sink_converts_params_and_gestures() {
        let mut out = Vec::new();
        let mut sink = WireSink::process(8, false, |event| {
            out.push(event);
            true
        });
        let id = ClapId::new(9);
        sink.try_push(ParamGestureBeginEvent::new(1, id).as_ref())
            .unwrap();
        sink.try_push(ParamValueEvent::new(2, id, Pckn::match_all(), 0.75).as_ref())
            .unwrap();
        sink.try_push(ParamGestureEndEvent::new(3, id).as_ref())
            .unwrap();
        sink.try_push(MidiEvent::new(0, 0, [0x90, 60, 1]).as_ref())
            .unwrap();
        let decoded: Vec<_> = out
            .iter()
            .map(|e| (e.offset, e.decode().unwrap()))
            .collect();
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
