//! Common CLAP event-only ABI and DSP parity check for the bundled MIDI processors.

use std::{ffi::c_void, io::Read, ptr};

use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON,
    CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT, CLAP_TRANSPORT_HAS_BEATS_TIMELINE,
    CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_PLAYING,
    clap_event_header, clap_event_midi, clap_event_note, clap_event_param_value,
    clap_event_transport, clap_input_events, clap_output_events,
};
use clap_sys::ext::note_ports::{CLAP_EXT_NOTE_PORTS, clap_plugin_note_ports};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_plugin_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use komponist_device_core::Spec;
use komponist_model::{Meter, PPQ, Tick, TickRange};
use komponist_processor::processor::EventBuffer;
use komponist_processor::{Event, EventKind, NOTE_ID_NONE, ProcessContext, TransportInfo};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;

use super::layout_contract::Instance;

const FRAMES: u32 = 2048;
const RATE: f32 = 48_000.0;
const TEMPO: f64 = 120.0;

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: std::mem::size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
    // SAFETY: Every invocation comes from the process call that borrows this header slice.
    let headers = unsafe { &*(*list).ctx.cast::<&[*const clap_event_header]>() };
    headers.len() as u32
}

unsafe extern "C" fn get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    // SAFETY: The borrowed header slice is live until the process call returns.
    let headers = unsafe { &*(*list).ctx.cast::<&[*const clap_event_header]>() };
    headers.get(index as usize).copied().unwrap_or(ptr::null())
}

unsafe extern "C" fn receive(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    // SAFETY: The plugin passes a live CLAP event; the process call owns and lends the output vec.
    let (header, output) = unsafe { (&*event, &mut *(*list).ctx.cast::<Vec<Event>>()) };
    if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return false;
    }
    let kind = match header.type_ {
        CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF => {
            // SAFETY: Both CLAP note event types use the same representation.
            let note = unsafe { &*event.cast::<clap_event_note>() };
            let channel = note.channel as u8;
            let key = note.key as u8;
            let note_id = u32::try_from(note.note_id).unwrap_or(NOTE_ID_NONE);
            if header.type_ == CLAP_EVENT_NOTE_ON {
                EventKind::NoteOn {
                    channel,
                    key,
                    note_id,
                    velocity: (note.velocity * 127.0).round() as u8,
                }
            } else {
                EventKind::NoteOff {
                    channel,
                    key,
                    note_id,
                }
            }
        }
        CLAP_EVENT_MIDI => {
            // SAFETY: CLAP_EVENT_MIDI identifies the standard MIDI event representation.
            EventKind::Midi(unsafe { (*event.cast::<clap_event_midi>()).data })
        }
        _ => return false,
    };
    output.push(Event {
        offset: header.time,
        kind,
    });
    true
}

pub(crate) fn assert_stage_parity<P: ClapPlugin>(
    spec: &'static Spec,
    stage: impl FnOnce() -> Box<dyn komponist_processor::EventProcessor + Send>,
) {
    let instance = Instance::new::<P>();
    let plugin = instance.0;
    let mut reference = stage();
    for (index, parameter) in spec.params().iter().enumerate() {
        reference.set_param(index as u32, parameter.default);
    }

    // SAFETY: This test is the instance's CLAP main thread and drives its processor exclusively
    // with valid per-call events and capture buffers. There are no audio ports on this plugin.
    unsafe {
        let note_ports = ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_NOTE_PORTS.as_ptr())
            .cast::<clap_plugin_note_ports>();
        assert!(!note_ports.is_null());
        assert_eq!(((*note_ports).count.unwrap())(plugin, true), 1);
        assert_eq!(((*note_ports).count.unwrap())(plugin, false), 1);
        assert!(((*plugin).activate.unwrap())(
            plugin,
            f64::from(RATE),
            1,
            FRAMES
        ));
        assert!(((*plugin).start_processing.unwrap())(plugin));
        let mut expected = EventBuffer::with_insertion_order(32);
        let mut heard_note_on = false;
        let mut heard_note_off = false;
        for block in 0..4 {
            let mut clap_events = Vec::new();
            let mut input_events = Vec::new();
            let mut note = clap_event_note {
                header: header::<clap_event_note>(0, CLAP_EVENT_NOTE_ON),
                note_id: 91,
                port_index: 0,
                channel: 2,
                key: 61,
                velocity: 100.0 / 127.0,
            };
            let retrigger = clap_event_note {
                note_id: 92,
                ..note
            };
            let release = clap_event_note {
                header: header::<clap_event_note>(4, CLAP_EVENT_NOTE_OFF),
                note_id: 93,
                port_index: 0,
                channel: 2,
                key: -1,
                velocity: 0.0,
            };
            let cc = clap_event_midi {
                header: header::<clap_event_midi>(3, CLAP_EVENT_MIDI),
                port_index: 0,
                data: [0xb2, 1, 64],
            };
            if block == 0 {
                clap_events.extend([&note.header as *const _, &cc.header as *const _]);
                input_events.extend([
                    Event {
                        offset: 0,
                        kind: EventKind::NoteOn {
                            key: 61,
                            channel: 2,
                            velocity: 100,
                            note_id: 91,
                        },
                    },
                    Event {
                        offset: 3,
                        kind: EventKind::Midi(cc.data),
                    },
                ]);
            } else if block == 1 {
                // Retrigger the key and then deliver the old note's keyed release.
                note.header.time = 64;
                note.header.type_ = CLAP_EVENT_NOTE_OFF;
                note.velocity = 0.0;
                clap_events.extend([&retrigger.header as *const _, &note.header as *const _]);
                input_events.push(Event {
                    offset: 0,
                    kind: EventKind::NoteOn {
                        key: 61,
                        channel: 2,
                        velocity: 100,
                        note_id: 92,
                    },
                });
            } else if block == 2 {
                // Host order at one frame: press a key, then release that same note by ID.
                note.header.time = 4;
                note.key = 63;
                note.note_id = 93;
                // The reference consumes the resolved address, without the CLAP wildcard.
                clap_events.extend([&note.header as *const _, &release.header as *const _]);
                input_events.extend([
                    Event {
                        offset: 4,
                        kind: EventKind::NoteOn {
                            key: 63,
                            channel: 2,
                            velocity: 100,
                            note_id: 93,
                        },
                    },
                    Event {
                        offset: 4,
                        kind: EventKind::NoteOff {
                            key: 63,
                            channel: 2,
                            note_id: 93,
                        },
                    },
                ]);
            } else {
                note.header.time = 64;
                note.header.type_ = CLAP_EVENT_NOTE_OFF;
                note.note_id = 92;
                note.key = -1;
                note.velocity = 0.0;
                clap_events.push(&note.header as *const _);
                input_events.push(Event {
                    offset: 64,
                    kind: EventKind::NoteOff {
                        key: 61,
                        channel: 2,
                        note_id: 92,
                    },
                });
            }
            let beat = f64::from(block * FRAMES) * TEMPO / (60.0 * f64::from(RATE));
            let transport = clap_event_transport {
                header: header::<clap_event_transport>(0, CLAP_EVENT_TRANSPORT),
                flags: CLAP_TRANSPORT_HAS_TEMPO
                    | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                    | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                    | CLAP_TRANSPORT_IS_PLAYING,
                song_pos_beats: (beat * CLAP_BEATTIME_FACTOR as f64).round() as i64,
                song_pos_seconds: 0,
                tempo: TEMPO,
                tempo_inc: 0.0,
                loop_start_beats: 0,
                loop_end_beats: 0,
                loop_start_seconds: 0,
                loop_end_seconds: 0,
                bar_start: 0,
                bar_number: 0,
                tsig_num: 4,
                tsig_denom: 4,
            };
            let headers = clap_events.as_slice();
            let mut actual: Vec<Event> = Vec::new();
            let input = clap_input_events {
                ctx: ptr::from_ref(&headers).cast_mut().cast::<c_void>(),
                size: Some(size),
                get: Some(get),
            };
            let output = clap_output_events {
                ctx: ptr::from_mut(&mut actual).cast::<c_void>(),
                try_push: Some(receive),
            };
            let process = clap_process {
                steady_time: i64::from(block * FRAMES),
                frames_count: FRAMES,
                transport: &transport,
                audio_inputs: ptr::null(),
                audio_outputs: ptr::null_mut(),
                audio_inputs_count: 0,
                audio_outputs_count: 0,
                in_events: &input,
                out_events: &output,
            };
            assert_ne!(
                ((*plugin).process.unwrap())(plugin, &process),
                CLAP_PROCESS_ERROR
            );
            let context = ProcessContext {
                frames: FRAMES,
                sample_rate: RATE,
                transport: TransportInfo {
                    playing: true,
                    recording: false,
                    looping: false,
                    tempo_bpm: TEMPO,
                    meter: Meter::FOUR_FOUR,
                    position_ticks: beat * PPQ as f64,
                    loop_range: TickRange::new(Tick(0), Tick(0)),
                },
                offline: false,
                deadline: None,
            };
            reference.process_events(&context, &input_events, &mut expected.sink(FRAMES));
            assert_eq!(actual, expected.events(), "MIDI events in block {block}");
            heard_note_on |= actual
                .iter()
                .any(|e| matches!(e.kind, EventKind::NoteOn { .. }));
            heard_note_off |= actual
                .iter()
                .any(|e| matches!(e.kind, EventKind::NoteOff { .. }));
        }
        ((*plugin).stop_processing.unwrap())(plugin);
        ((*plugin).deactivate.unwrap())(plugin);
        assert!(
            heard_note_on && heard_note_off,
            "parity must cover a complete note"
        );
    }
}

unsafe extern "C" fn write_state(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    if size == 0 {
        return 0;
    }
    // SAFETY: The stream's context is its caller's Vec. CLAP lends size readable bytes.
    let (bytes, output) = unsafe {
        (
            std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize),
            &mut *(*stream).ctx.cast::<Vec<u8>>(),
        )
    };
    output.extend_from_slice(bytes);
    size as i64
}

unsafe extern "C" fn read_state(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    if size == 0 {
        return 0;
    }
    // SAFETY: The CLAP plugin lends a buffer of size writable bytes, and the input cursor lives
    // for the entire load_state call.
    let (input, bytes) = unsafe {
        (
            &mut *(*stream).ctx.cast::<std::io::Cursor<&[u8]>>(),
            std::slice::from_raw_parts_mut(buffer.cast::<u8>(), size as usize),
        )
    };
    input.read(bytes).map_or(-1, |read| read as i64)
}

pub(crate) fn assert_state_round_trip<P: ClapPlugin>(param: &str) {
    let first = Instance::new::<P>();
    let second = Instance::new::<P>();
    // SAFETY: The static host owns two initialized, inactive CLAP instances. All stream and
    // input-event storage lives until the corresponding callback returns.
    unsafe {
        let id = Wrapper::<P>::custom_extension_param_id(first.0, param).unwrap();
        assert_eq!(
            Wrapper::<P>::custom_extension_param_id(second.0, param),
            Some(id)
        );
        let extension = (*first.0).get_extension.unwrap();
        let params = extension(first.0, CLAP_EXT_PARAMS.as_ptr()).cast::<clap_plugin_params>();
        let state = extension(first.0, CLAP_EXT_STATE.as_ptr()).cast::<clap_plugin_state>();
        assert!(!params.is_null() && !state.is_null());

        let mut value = f64::NAN;
        let get_value = (*params).get_value.unwrap();
        assert!(get_value(second.0, id, &mut value));
        assert_eq!(value, 0.0);
        let param = clap_event_param_value {
            header: header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE),
            param_id: id,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value: 2.0,
        };
        let headers: &[*const clap_event_header] = &[&param.header];
        let input = clap_input_events {
            ctx: ptr::from_ref(&headers).cast_mut().cast::<c_void>(),
            size: Some(size),
            get: Some(get),
        };
        ((*params).flush.unwrap())(first.0, &input, ptr::null());
        assert!(get_value(first.0, id, &mut value));
        assert_eq!(value, 2.0);

        let mut bytes: Vec<u8> = Vec::new();
        let stream = clap_ostream {
            ctx: ptr::from_mut(&mut bytes).cast(),
            write: Some(write_state),
        };
        assert!(((*state).save.unwrap())(first.0, &stream));
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let stream = clap_istream {
            ctx: ptr::from_mut(&mut cursor).cast(),
            read: Some(read_state),
        };
        assert!(((*state).load.unwrap())(second.0, &stream));
        assert!(get_value(second.0, id, &mut value));
        assert_eq!(value, 2.0);
    }
}
