//! Run the same sample-state and note playback contract through both exported CLAP instruments.

use std::ffi::c_void;
use std::io::{Cursor, Read};
use std::ptr;
use std::time::{Duration, Instant};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, clap_event_header,
    clap_event_note, clap_input_events,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use komponist_native_clap_sdk::sampler_ui::{EXTENSION_ID, MAX_BYTES, Snapshot, Status};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::json;

use crate::layout_contract::Instance;

const FRAMES: usize = 128;
const NOTE_OFFSET: usize = 16;

unsafe extern "C" fn input_size(input: *const clap_input_events) -> u32 {
    // SAFETY: Its caller retains the borrowed note slice through process.
    (unsafe { &*(*input).ctx.cast::<&[clap_event_note]>() }).len() as u32
}

unsafe extern "C" fn input_get(
    input: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: Its caller retains the borrowed note slice through process.
    unsafe { &*(*input).ctx.cast::<&[clap_event_note]>() }
        .get(index as usize)
        .map_or(ptr::null(), |note| &note.header)
}

unsafe extern "C" fn read_state(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    // SAFETY: The wrapper supplies a writable buffer and the synchronous caller retains Cursor.
    unsafe {
        let cursor = &mut *(*stream).ctx.cast::<Cursor<&[u8]>>();
        let buffer = std::slice::from_raw_parts_mut(buffer.cast(), size as usize);
        cursor.read(buffer).map_or(-1, |count| count as i64)
    }
}

unsafe extern "C" fn write_state(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    // SAFETY: The wrapper supplies a readable buffer and the synchronous caller retains Vec.
    unsafe {
        let state = &mut *(*stream).ctx.cast::<Vec<u8>>();
        state.extend_from_slice(std::slice::from_raw_parts(buffer.cast(), size as usize));
        size as i64
    }
}

#[repr(C)]
struct SamplerUi {
    snapshot: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, f64) -> u32,
    preview: unsafe extern "C" fn(*const clap_plugin, f64),
}

fn read_snapshot(instance: &Instance) -> Snapshot {
    let plugin = instance.0;
    let mut bytes = [0; MAX_BYTES];
    // SAFETY: The test retains the CLAP instance and the writable snapshot buffer.
    let len = unsafe {
        let table = ((*plugin).get_extension.unwrap())(plugin, EXTENSION_ID.as_ptr());
        let table = table
            .cast::<SamplerUi>()
            .as_ref()
            .expect("sampler UI extension");
        assert_eq!((table.snapshot)(plugin, bytes.as_mut_ptr(), 1, 0.0), 0);
        (table.snapshot)(plugin, bytes.as_mut_ptr(), MAX_BYTES as u32, 0.0)
    };
    Snapshot::decode(&bytes[..len as usize]).expect("valid sampler UI data")
}

fn check_state_roundtrip<P: ClapPlugin>(instance: &Instance, assets: &str) {
    let plugin = instance.0;
    // SAFETY: The instance is initialized and all buffers remain live through each state call.
    unsafe {
        let table = ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_STATE.as_ptr());
        assert!(!table.is_null());
        let table = &*table.cast::<clap_plugin_state>();
        let load = |plugin, state: &serde_json::Value| {
            let body = serde_json::to_vec(state).unwrap();
            let mut record = (body.len() as u64).to_le_bytes().to_vec();
            record.extend(body);
            let mut cursor = Cursor::new(record.as_slice());
            let stream = clap_istream {
                ctx: ptr::from_mut(&mut cursor).cast(),
                read: Some(read_state),
            };
            (table.load.unwrap())(plugin, &stream)
        };
        let save = |plugin| {
            let mut record = Vec::<u8>::new();
            let stream = clap_ostream {
                ctx: ptr::from_mut(&mut record).cast(),
                write: Some(write_state),
            };
            assert!((table.save.unwrap())(plugin, &stream));
            assert_eq!(
                u64::from_le_bytes(record[..8].try_into().unwrap()) as usize,
                record.len() - 8
            );
            serde_json::from_slice::<serde_json::Value>(&record[8..]).unwrap()
        };

        let mut state = save(plugin);
        state["fields"]["sample_assets"] = assets.into();
        assert!(load(plugin, &state));
        let expected = save(plugin);
        assert_eq!(expected["fields"]["sample_assets"], assets);
        let mut invalid = expected.clone();
        let float_value = invalid["params"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .find_map(|parameter| parameter.get_mut("f32"))
            .expect("a continuous sampler parameter");
        *float_value = json!(if float_value.as_f64().unwrap() > 0.5 {
            0.25
        } else {
            0.75
        });
        for malformed in ["[]", r#"{"samples":[]}"#, r#"{"sample_edit":[]}"#] {
            invalid["fields"]["sample_assets"] = malformed.into();
            assert!(!load(plugin, &invalid), "accepted {malformed}");
            assert_eq!(
                save(plugin),
                expected,
                "an invalid field must leave live state unchanged"
            );
        }

        let other = Instance::new::<P>();
        assert!(load(other.0, &expected));
        assert_eq!(save(other.0), expected);
    }
}

pub(crate) fn sample_asset_roundtrip<P: ClapPlugin>(key: u8, gate_param: &str) {
    let directory = tempfile::tempdir().unwrap();
    let sample = directory.path().join("linked.wav");
    let mut writer = hound::WavWriter::create(
        &sample,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..2048 {
        let value = if (frame / 12) % 2 == 0 {
            11_000i16
        } else {
            -11_000
        };
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();

    let instance = Instance::new::<P>();
    let plugin = instance.0;
    let missing = directory.path().join("missing.wav");
    let assets = |path: &std::path::Path| {
        json!({
            "samples": {"1": {"abs": path, "rel": "collected/linked.wav", "name": "linked"}},
            "sample_edit": {"slices": [0.01, {"time": 0.02, "reverse": true}]}
        })
        .to_string()
    };
    // SAFETY: This is an initialized instance and the versioned ID is NUL-terminated.
    assert!(unsafe {
        ((*plugin).get_extension.unwrap())(
            plugin,
            c"org.klang-studio.komponist.sampler-ui/1".as_ptr(),
        )
        .is_null()
    });
    assert_eq!(read_snapshot(&instance).pads[0].status, Status::Empty);
    check_state_roundtrip::<P>(&instance, &assets(&sample));
    let note = clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: NOTE_OFFSET as u32,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_NOTE_ON,
            flags: 0,
        },
        note_id: 13,
        port_index: 0,
        channel: 2,
        key: i16::from(key),
        velocity: 0.8,
    };
    // SAFETY: The wrapper, event, and output buffers outlive their calls, and the test owns the
    // whole instance and acts as its control and audio threads sequentially.
    unsafe {
        let wrapper = &*(*plugin).plugin_data.cast::<Wrapper<P>>();
        let mut gate_state = serde_json::to_value(wrapper.get_state_object()).unwrap();
        assert!(gate_state["params"].get(gate_param).is_some());
        gate_state["params"][gate_param] = json!({"string": "gate"});
        let mut state: nice_plug::prelude::PluginState =
            serde_json::from_value(gate_state).unwrap();
        state
            .fields
            .insert("sample_assets".into(), assets(&missing));
        assert!(wrapper.set_state_inner(&mut state));
        assert_eq!(
            wrapper.get_state_object().fields["sample_assets"],
            assets(&missing)
        );

        assert!(((*plugin).activate.unwrap())(
            plugin,
            48_000.0,
            1,
            FRAMES as u32
        ));
        assert!(((*plugin).start_processing.unwrap())(plugin));
        let mut steady_time = 0;
        let mut render = |notes: &[clap_event_note]| {
            ((*plugin).reset.unwrap())(plugin);
            let in_events = clap_input_events {
                ctx: ptr::from_ref(&notes).cast_mut().cast::<c_void>(),
                size: Some(input_size),
                get: Some(input_get),
            };
            let mut out = [[0.0f32; FRAMES]; 2];
            let mut output_channels = out.each_mut().map(|samples| samples.as_mut_ptr());
            let mut output = clap_audio_buffer {
                data32: output_channels.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: 2,
                latency: 0,
                constant_mask: 0,
            };
            let block = clap_process {
                steady_time,
                frames_count: FRAMES as u32,
                transport: ptr::null(),
                audio_inputs: ptr::null(),
                audio_outputs: &mut output,
                audio_inputs_count: 0,
                audio_outputs_count: 1,
                in_events: &in_events,
                out_events: ptr::null(),
            };
            assert_ne!(
                ((*plugin).process.unwrap())(plugin, &block),
                CLAP_PROCESS_ERROR
            );
            steady_time += FRAMES as i64;
            out[0]
        };

        assert!(render(&[note]).iter().all(|sample| *sample == 0.0));
        // Replacing the missing link and preserving the portable metadata does not require
        // another activation. The loader completes when it has decoded the new target.
        state.fields.insert("sample_assets".into(), assets(&sample));
        assert!(wrapper.set_state_inner(&mut state));
        assert_eq!(
            wrapper.get_state_object().fields["sample_assets"],
            assets(&sample)
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let rendered = render(&[note]);
            assert!(rendered[..NOTE_OFFSET].iter().all(|sample| *sample == 0.0));
            if rendered[NOTE_OFFSET..]
                .iter()
                .any(|sample| sample.abs() > 0.01)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not deliver the linked sample"
            );
            std::thread::sleep(Duration::from_millis(3));
        }
        let snapshot = read_snapshot(&instance);
        assert_eq!(snapshot.pads[0].status, Status::Ready);
        assert_eq!(snapshot.pads[0].name, "linked");
        assert!(snapshot.pads[0].peaks.iter().any(|peak| *peak > 0.01));
        let mut release = clap_event_note {
            header: clap_event_header {
                time: 32,
                type_: CLAP_EVENT_NOTE_OFF,
                ..note.header
            },
            key: -1,
            note_id: -1,
            velocity: 0.0,
            ..note
        };
        let matched = render(&[note, release]);
        release.channel = 3;
        let other_channel = render(&[note, release]);
        let level =
            |samples: [f32; FRAMES]| samples[64..].iter().map(|sample| sample.abs()).sum::<f32>();
        let (matched, other_channel) = (level(matched), level(other_channel));
        assert!(
            matched < other_channel * 0.99,
            "release must attenuate only its own channel: {matched} vs {other_channel}"
        );
        ((*plugin).stop_processing.unwrap())(plugin);
        ((*plugin).deactivate.unwrap())(plugin);
    }
}
