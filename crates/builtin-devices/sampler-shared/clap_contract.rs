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
use komponist_native_clap_sdk::device_assets;
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

pub(crate) fn load_state(instance: &Instance, state: &serde_json::Value) -> bool {
    let plugin = instance.0;
    let body = serde_json::to_vec(state).unwrap();
    let mut record = (body.len() as u64).to_le_bytes().to_vec();
    record.extend(body);
    let mut cursor = Cursor::new(record.as_slice());
    let stream = clap_istream {
        ctx: ptr::from_mut(&mut cursor).cast(),
        read: Some(read_state),
    };
    // SAFETY: The instance is initialized and keeps its extension table live. The calling
    // thread keeps the stream and record live until synchronous load returns.
    unsafe {
        let table = ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_STATE.as_ptr());
        let table = table
            .cast::<clap_plugin_state>()
            .as_ref()
            .expect("state extension");
        (table.load.unwrap())(plugin, &stream)
    }
}

pub(crate) fn save_state(instance: &Instance) -> serde_json::Value {
    let plugin = instance.0;
    let mut record = Vec::<u8>::new();
    let stream = clap_ostream {
        ctx: ptr::from_mut(&mut record).cast(),
        write: Some(write_state),
    };
    // SAFETY: The instance is initialized and keeps its extension table live. The calling
    // thread keeps the stream and record live until synchronous save returns.
    unsafe {
        let table = ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_STATE.as_ptr());
        let table = table
            .cast::<clap_plugin_state>()
            .as_ref()
            .expect("state extension");
        assert!((table.save.unwrap())(plugin, &stream));
    }
    assert_eq!(
        u64::from_le_bytes(record[..8].try_into().unwrap()) as usize,
        record.len() - 8
    );
    serde_json::from_slice(&record[8..]).unwrap()
}

fn check_state_roundtrip<P: ClapPlugin>(instance: &Instance, assets: &str) {
    let mut state = save_state(instance);
    state["fields"]["sample_assets"] = assets.into();
    assert!(load_state(instance, &state));
    let expected = save_state(instance);
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
        assert!(!load_state(instance, &invalid), "accepted {malformed}");
        assert_eq!(
            save_state(instance),
            expected,
            "an invalid field must leave live state unchanged"
        );
    }

    let other = Instance::new::<P>();
    assert!(load_state(&other, &expected));
    assert_eq!(save_state(&other), expected);
}

fn check_file_slots<P: ClapPlugin>(sample: &std::path::Path) {
    use device_assets::{AssetStatus, Binding, Bindings, Extension, OfflineStatus};

    let instance = Instance::new::<P>();
    let plugin = instance.0;
    // SAFETY: The instance owns the immutable extension table and the test retains each buffer
    // until the corresponding CLAP callback returns. Audio processing has not started.
    unsafe {
        let raw = ((*plugin).get_extension.unwrap())(plugin, device_assets::EXTENSION_ID.as_ptr());
        let table = raw
            .cast::<Extension>()
            .as_ref()
            .expect("file-slot extension");
        let status = || {
            let mut bytes = [0; device_assets::MAX_SNAPSHOT_BYTES];
            let len =
                table.snapshot.unwrap()(plugin.cast(), bytes.as_mut_ptr(), bytes.len() as u32);
            let snapshot = device_assets::Snapshot::decode(&bytes[..len as usize]).unwrap();
            assert_eq!(snapshot.slots[0].id, 0);
            snapshot.slots[0].status.clone()
        };
        let bind = |slots| {
            let binding = Bindings { slots };
            let mut bytes = vec![0; device_assets::MAX_BINDINGS_BYTES];
            let len = binding.encode(&mut bytes).unwrap();
            table.set_bindings.unwrap()(plugin.cast(), bytes.as_ptr(), len as u32)
        };
        let prepare = table.prepare_offline.unwrap();
        assert_eq!(status(), AssetStatus::Empty);
        let link = Binding {
            id: 0,
            path: Some(sample.to_str().unwrap().into()),
            reload: false,
        };
        assert_eq!(
            bind(vec![
                link.clone(),
                Binding {
                    id: u32::MAX,
                    path: None,
                    reload: false,
                }
            ]),
            0
        );
        assert_eq!(status(), AssetStatus::Empty);
        assert_eq!(bind(vec![link.clone()]), 1);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match OfflineStatus::from_raw(prepare(plugin.cast(), 1, 48_000.0, 120.0)) {
                OfflineStatus::Ready => break,
                OfflineStatus::Preparing => {
                    assert!(Instant::now() < deadline, "sample preparation timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                OfflineStatus::Failed => panic!("the linked sample could not be prepared"),
            }
        }
        assert_eq!(status(), AssetStatus::Ready);
        let mut embedded = save_state(&instance);
        embedded["fields"]["sample_assets"] = json!({
            "samples": {"01": {
                "abs": sample.with_extension("missing.wav"),
                "rel": "missing.wav",
                "name": "embedded"
            }},
            "sample_revisions": [17]
        })
        .to_string()
        .into();
        let assert_host_managed = || {
            assert!(load_state(&instance, &embedded));
            let saved = save_state(&instance);
            let fields: serde_json::Value =
                serde_json::from_str(saved["fields"]["sample_assets"].as_str().unwrap()).unwrap();
            assert!(fields.get("samples").is_none());
            assert_eq!(fields["sample_revisions"], json!([17]));
        };
        assert_host_managed();
        assert_eq!(status(), AssetStatus::Ready);
        assert_eq!(
            prepare(plugin.cast(), 0, 0.0, 0.0),
            OfflineStatus::Ready as u32
        );
        let moved = sample.with_extension("backup.wav");
        std::fs::rename(sample, &moved).unwrap();
        assert_eq!(bind(vec![link.clone()]), 1);
        assert_eq!(status(), AssetStatus::Ready);
        assert_eq!(
            bind(vec![Binding {
                reload: true,
                ..link
            }]),
            1
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while status() == AssetStatus::Loading {
            assert!(Instant::now() < deadline, "sample reload timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(status(), AssetStatus::Missing);
        std::fs::rename(moved, sample).unwrap();
        assert_eq!(
            bind(vec![Binding {
                id: 0,
                path: None,
                reload: false
            }]),
            1
        );
        assert_host_managed();
        assert_eq!(status(), AssetStatus::Empty);
    }
}

pub(crate) fn sample_asset_roundtrip<P: ClapPlugin>(key: u8, gate_param: &str, has_playhead: bool) {
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
    check_file_slots::<P>(&sample);

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
    let initial = read_snapshot(&instance);
    let pad_count = initial.pads.len();
    assert_eq!(initial.pads[0].status, Status::Empty);
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
        use komponist_native_clap_sdk::signal_taps::{
            Descriptor, EXTENSION_ID, Extension, MAX_PAD_ACTIVITY_FRAME, PAD_ACTIVITY,
            PLAYHEAD_POSITION, PadActivity,
        };

        let wrapper = &*(*plugin).plugin_data.cast::<Wrapper<P>>();
        let taps = ((*plugin).get_extension.unwrap())(plugin, EXTENSION_ID.as_ptr())
            .cast::<Extension>()
            .as_ref()
            .unwrap();
        assert_eq!(taps.count.unwrap()(plugin.cast()), 1);
        let mut descriptor = Descriptor::EMPTY;
        assert_eq!(taps.info.unwrap()(plugin.cast(), 0, &mut descriptor), 1);
        let (kind, name) = if has_playhead {
            (PLAYHEAD_POSITION, "playhead")
        } else {
            (PAD_ACTIVITY, "pads")
        };
        assert_eq!(descriptor.kind, kind);
        assert_eq!(descriptor.key(), Some(name));
        let read_activity = || {
            let mut frame = [0.0; MAX_PAD_ACTIVITY_FRAME];
            assert_eq!(
                taps.read.unwrap()(plugin.cast(), 0, frame.as_mut_ptr(), frame.len() as u32),
                frame.len() as u32,
            );
            PadActivity::decode(&frame, pad_count).unwrap()
        };
        let read_playhead = || {
            if has_playhead {
                let mut value = 0.0;
                assert_eq!(taps.read.unwrap()(plugin.cast(), 0, &mut value, 1), 1);
                (value >= 0.0).then_some(value)
            } else {
                read_activity().playheads[0]
            }
        };
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

        use komponist_native_clap_sdk::device_assets;
        let table =
            ((*plugin).get_extension.unwrap())(plugin, device_assets::EXTENSION_ID.as_ptr());
        let prepare = table
            .cast::<device_assets::Extension>()
            .as_ref()
            .unwrap()
            .prepare_offline
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status =
                device_assets::OfflineStatus::from_raw(prepare(plugin.cast(), 1, 48_000.0, 120.0));
            match status {
                device_assets::OfflineStatus::Failed => break,
                device_assets::OfflineStatus::Preparing => {
                    assert!(
                        Instant::now() < deadline,
                        "missing restored sample did not settle"
                    );
                    std::thread::sleep(Duration::from_millis(3));
                }
                device_assets::OfflineStatus::Ready => {
                    panic!("missing restored sample cannot render")
                }
            }
        }
        assert_eq!(
            prepare(plugin.cast(), 0, 0.0, 0.0),
            device_assets::OfflineStatus::Ready as u32
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
            taps.begin.unwrap()(plugin.cast(), 1);
            assert_ne!(
                ((*plugin).process.unwrap())(plugin, &block),
                CLAP_PROCESS_ERROR
            );
            steady_time += FRAMES as i64;
            out[0]
        };

        assert!(render(&[note]).iter().all(|sample| *sample == 0.0));
        assert_eq!(read_playhead(), None);
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
        let position = read_playhead().expect("playing the loaded sample reports its position");
        assert!((0.0..=1.0).contains(&position));
        assert_eq!(Some(position), snapshot.pads[0].playhead);
        if !has_playhead {
            let initial = read_activity();
            assert_eq!(initial.last_pad, 0);
            let second_pad = clap_event_note {
                key: i16::from(key + 1),
                ..note
            };
            assert!(render(&[second_pad]).iter().all(|sample| *sample == 0.0));
            let following = read_activity();
            assert_ne!(following.hit, initial.hit);
            assert_eq!(following.last_pad, 1);
            assert!(following.playheads.iter().all(Option::is_none));
        }
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
