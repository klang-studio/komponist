use std::ptr;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use nice_plug::wrapper::clap::Wrapper;

use super::layout_contract::{Instance, assert_localized_bindings, read_layout};
use super::*;

#[test]
fn legacy_factory_selection_survives_adapter_state_callbacks() {
    use std::collections::BTreeMap;

    use komponist_device_core::pinned_clap_state::FACTORY_PRESET_FIELD;

    let params = HallPlugin::default().params();
    let preset = || {
        params
            .serialize_fields()
            .remove(FACTORY_PRESET_FIELD)
            .map(|name| serde_json::from_str::<String>(&name).unwrap())
    };
    assert_eq!(preset(), None);
    params.deserialize_fields(&BTreeMap::from([(
        FACTORY_PRESET_FIELD.into(),
        serde_json::to_string(Hall::SPEC.presets()[0].name).unwrap(),
    )]));
    assert_eq!(preset().as_deref(), Some("Small Room"));

    let restored = HallPlugin::default().params();
    restored.deserialize_fields(&params.serialize_fields());
    assert_eq!(restored.serialize_fields(), params.serialize_fields());

    for invalid in ["\"A missing preset\"", "false"] {
        params.deserialize_fields(&BTreeMap::from([(
            FACTORY_PRESET_FIELD.into(),
            invalid.to_owned(),
        )]));
        assert_eq!(preset(), None);
        params.deserialize_fields(&restored.serialize_fields());
        assert_eq!(preset().as_deref(), Some("Small Room"));
    }

    params.deserialize_fields(&BTreeMap::new());
    assert_eq!(preset(), None);
}

#[test]
fn advertised_layout_contains_every_binding_and_stable_room_ids() {
    assert_localized_bindings::<HallPlugin>(Hall::SPEC.params().len());
    let layout = read_layout::<HallPlugin>();
    assert_eq!(layout["metadata"]["names"]["de"], "Hall");
    assert_eq!(layout["metadata"]["names"]["en"], "Reverb");
    let first = Instance::new::<HallPlugin>();
    // SAFETY: The instance is live and initialized on this test thread.
    unsafe {
        let cells = layout["pages"][0]["cells"].as_array().unwrap();
        assert_eq!(cells.len(), Hall::SPEC.params().len());
        let room_id = Wrapper::<HallPlugin>::custom_extension_param_id(first.0, "room").unwrap();
        let room = cells
            .iter()
            .find(|cell| cell["param_id"] == room_id)
            .unwrap();
        assert_eq!(room["choices"][0]["labels"]["de"], "Zimmer");
        assert_eq!(room["choices"][2]["labels"]["en"], "Church");
    }
}

#[test]
fn impulse_through_the_clap_callbacks_matches_the_existing_hall_dsp() {
    const FRAMES: usize = 512;
    let instance = Instance::new::<HallPlugin>();
    let plugin = instance.0;
    let mut reference = komponist_reverb::dsp();
    for (index, parameter) in Hall::SPEC.params().iter().enumerate() {
        reference.set_param(index, parameter.default);
    }
    reference.activate(48_000.0);
    reference.reset();

    let mut steady_time = 0;
    let mut heard_tail = false;
    // SAFETY: This test exclusively owns the initialized plugin and all event/audio buffers.
    // Each block provides two initialized, distinct channels for both input and output.
    unsafe {
        assert!(((*plugin).activate.unwrap())(
            plugin,
            48_000.0,
            1,
            FRAMES as u32
        ));
        assert!(((*plugin).start_processing.unwrap())(plugin));
        for index in 0..16 {
            let mut input = [[0.0f32; FRAMES]; 2];
            if index == 0 {
                input[0][0] = 1.0;
            }
            let mut out = [[0.0f32; FRAMES]; 2];
            let mut expected = [[0.0f32; FRAMES]; 2];
            let [expected_l, expected_r] = &mut expected;
            reference.process([&input[0], &input[1]], [expected_l, expected_r]);

            let mut in_ptrs = input.each_mut().map(|samples| samples.as_mut_ptr());
            let mut out_ptrs = out.each_mut().map(|samples| samples.as_mut_ptr());
            let port = |channels: &mut [*mut f32; 2]| clap_audio_buffer {
                data32: channels.as_mut_ptr(),
                data64: ptr::null_mut(),
                channel_count: 2,
                latency: 0,
                constant_mask: 0,
            };
            let in_port = port(&mut in_ptrs);
            let mut out_port = port(&mut out_ptrs);
            let block = clap_process {
                steady_time,
                frames_count: FRAMES as u32,
                transport: ptr::null(),
                audio_inputs: &in_port,
                audio_outputs: &mut out_port,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: ptr::null(),
                out_events: ptr::null(),
            };
            assert_ne!(
                ((*plugin).process.unwrap())(plugin, &block),
                CLAP_PROCESS_ERROR
            );
            for (frame, (actual, expected)) in out
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .enumerate()
            {
                assert!(
                    (actual - expected).abs() < 1e-6,
                    "block {index} sample {frame}: CLAP {actual}, DSP {expected}"
                );
            }
            heard_tail |= index > 0 && out.iter().flatten().any(|sample| *sample != 0.0);
            steady_time += FRAMES as i64;
        }
        ((*plugin).stop_processing.unwrap())(plugin);
        ((*plugin).deactivate.unwrap())(plugin);
    }
    assert!(heard_tail);
}
