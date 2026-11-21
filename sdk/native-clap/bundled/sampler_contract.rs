//! Run the same sample-state and note playback contract through both exported CLAP instruments.

use std::ffi::c_void;
use std::ptr;
use std::time::{Duration, Instant};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_ON, clap_event_header, clap_event_note,
    clap_input_events,
};
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::json;

use crate::layout_contract::Instance;

const FRAMES: usize = 128;
const NOTE_OFFSET: usize = 16;

unsafe extern "C" fn input_size(_: *const clap_input_events) -> u32 {
    1
}

unsafe extern "C" fn input_get(
    input: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    if index == 0 {
        // SAFETY: The test's input list stores a live note with a header at offset zero.
        unsafe { (*input).ctx.cast() }
    } else {
        ptr::null()
    }
}

pub(crate) fn sample_asset_roundtrip<P: ClapPlugin>(key: u8) {
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
    let mut note = clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: NOTE_OFFSET as u32,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_NOTE_ON,
            flags: 0,
        },
        note_id: 13,
        port_index: 0,
        channel: 0,
        key: i16::from(key),
        velocity: 0.8,
    };
    let in_events = clap_input_events {
        ctx: ptr::from_mut(&mut note).cast::<c_void>(),
        size: Some(input_size),
        get: Some(input_get),
    };

    // SAFETY: The wrapper, event, and output buffers outlive their calls, and the test owns the
    // whole instance and acts as its control and audio threads sequentially.
    unsafe {
        let wrapper = &*(*plugin).plugin_data.cast::<Wrapper<P>>();
        let mut state = wrapper.get_state_object();
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
        let mut render = || {
            ((*plugin).reset.unwrap())(plugin);
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

        assert!(render().iter().all(|sample| *sample == 0.0));
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
            let rendered = render();
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
        ((*plugin).stop_processing.unwrap())(plugin);
        ((*plugin).deactivate.unwrap())(plugin);
    }
}
