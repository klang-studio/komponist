use std::collections::BTreeSet;
use std::ffi::{c_char, c_void};
use std::{ptr, sync::Arc};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, clap_event_header,
    clap_event_note, clap_input_events,
};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::version::CLAP_VERSION;
use komponist_native_clap_sdk::{NativeClapLayout, compile};
use nice_plug::wrapper::clap::Wrapper;

use super::*;

unsafe extern "C" fn no_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"Surf test host".as_ptr(),
    vendor: c"klang-studio".as_ptr(),
    url: c"https://github.com/klang-studio/komponist".as_ptr(),
    version: c"1".as_ptr(),
    get_extension: Some(no_extension),
    request_restart: None,
    request_process: None,
    request_callback: None,
};

struct Instance(*const clap_plugin);

impl Instance {
    fn new() -> Self {
        // SAFETY: HOST outlives the plugin. Transfer the wrapper reference to the CLAP destroy call.
        unsafe {
            let wrapper = Arc::into_raw(Wrapper::<SurfPlugin>::new(&HOST).unwrap());
            let instance = Self((*wrapper).clap_plugin.as_ptr());
            assert!(((*instance.0).init.unwrap())(instance.0));
            instance
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: This instance owns the reference transferred in new().
        unsafe { ((*self.0).destroy.unwrap())(self.0) };
    }
}

#[test]
fn layout_uses_stable_ids_and_covers_every_synth_parameter() {
    let first = Instance::new();
    let second = Instance::new();
    // SAFETY: Both instances belong to this thread and remain initialized throughout the check.
    unsafe {
        let panel = SurfPlugin::panel();
        let bytes = compile::<SurfPlugin>(first.0, &panel).unwrap();
        assert_eq!(bytes, compile::<SurfPlugin>(second.0, &panel).unwrap());
        let layout: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let pages = layout["pages"].as_array().unwrap();
        assert_eq!(pages.len(), 4);
        let ids = pages
            .iter()
            .flat_map(|page| {
                page["cells"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|cell| cell["param_id"].as_u64().unwrap() as u32)
            })
            .collect::<BTreeSet<_>>();
        let expected = Surf::SPEC
            .params()
            .iter()
            .map(|info| {
                Wrapper::<SurfPlugin>::custom_extension_param_id(first.0, info.id.as_str()).unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(expected.len(), Surf::SPEC.params().len());
        assert_eq!(ids, expected);
        let author_ids = layout["author_ids"].as_object().unwrap();
        assert_eq!(author_ids.len(), Surf::SPEC.params().len());
        for info in Surf::SPEC.params() {
            assert_eq!(
                author_ids[info.id.as_str()],
                Wrapper::<SurfPlugin>::custom_extension_param_id(first.0, info.id.as_str())
                    .unwrap()
            );
        }
    }
}

unsafe extern "C" fn event_count(_: *const clap_input_events) -> u32 {
    2
}

unsafe extern "C" fn event_at(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: This callback is only used with a pointer to two live CLAP note events.
    let notes = unsafe { &*(*list).ctx.cast::<[clap_event_note; 2]>() };
    &notes[index as usize].header
}

#[test]
fn sample_offset_note_events_preserve_the_original_surf_audio() {
    const FRAMES: usize = 512;
    const NOTE_ON: usize = 37;
    const NOTE_OFF: usize = 317;

    let instance = Instance::new();
    let plugin = instance.0;
    let mut reference = komponist_surf::dsp();
    for (index, info) in Surf::SPEC.params().iter().enumerate() {
        reference.set_param(index, info.default);
    }
    reference.activate(48_000.0);
    reference.reset();

    let mut expected = [[0.0f32; FRAMES]; 2];
    let silence = [0.0; FRAMES];
    let [left, right] = &mut expected;
    reference.process(
        [&silence[..NOTE_ON]; 2],
        [&mut left[..NOTE_ON], &mut right[..NOTE_ON]],
    );
    reference.note_on(60, 102);
    reference.process(
        [&silence[NOTE_ON..NOTE_OFF]; 2],
        [&mut left[NOTE_ON..NOTE_OFF], &mut right[NOTE_ON..NOTE_OFF]],
    );
    reference.note_off(60);
    reference.process(
        [&silence[NOTE_OFF..]; 2],
        [&mut left[NOTE_OFF..], &mut right[NOTE_OFF..]],
    );

    let note = |sample: usize, type_| clap_event_note {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_note>() as u32,
            time: sample as u32,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_,
            flags: 0,
        },
        note_id: 17,
        port_index: 0,
        channel: 0,
        key: 60,
        velocity: 102.0 / 127.0,
    };
    let mut notes = [
        note(NOTE_ON, CLAP_EVENT_NOTE_ON),
        note(NOTE_OFF, CLAP_EVENT_NOTE_OFF),
    ];
    let events = clap_input_events {
        ctx: (&mut notes as *mut [clap_event_note; 2]).cast(),
        size: Some(event_count),
        get: Some(event_at),
    };
    let mut output = [[f32::NAN; FRAMES]; 2];
    let mut pointers = output.each_mut().map(|samples| samples.as_mut_ptr());
    let mut outputs = clap_audio_buffer {
        data32: pointers.as_mut_ptr(),
        data64: ptr::null_mut(),
        channel_count: 2,
        latency: 0,
        constant_mask: 0,
    };
    let block = clap_process {
        steady_time: 0,
        frames_count: FRAMES as u32,
        transport: ptr::null(),
        audio_inputs: ptr::null(),
        audio_outputs: &mut outputs,
        audio_inputs_count: 0,
        audio_outputs_count: 1,
        in_events: &events,
        out_events: ptr::null(),
    };
    // SAFETY: The initialized plugin and each block's stereo channel/event storage are exclusive.
    unsafe {
        assert!(((*plugin).activate.unwrap())(
            plugin,
            48_000.0,
            1,
            FRAMES as u32
        ));
        assert!(((*plugin).start_processing.unwrap())(plugin));
        assert_ne!(
            ((*plugin).process.unwrap())(plugin, &block),
            CLAP_PROCESS_ERROR
        );
        ((*plugin).stop_processing.unwrap())(plugin);
        ((*plugin).deactivate.unwrap())(plugin);
    }
    assert!(expected.iter().flatten().any(|sample| sample.abs() > 0.001));
    for (at, (actual, expected)) in output
        .iter()
        .flatten()
        .zip(expected.iter().flatten())
        .enumerate()
    {
        assert!(
            (actual - expected).abs() < 1e-6,
            "sample {at}: {actual} vs {expected}"
        );
    }
}

#[test]
fn every_enum_choice_matches_the_device_list() {
    let choices = |id: &str| match &Surf::SPEC
        .params()
        .iter()
        .find(|info| info.id.as_str() == id)
        .unwrap()
        .kind
    {
        komponist_model::params::ParamKind::Enum(names) => names.len(),
        other => panic!("{id} is {other:?}"),
    };
    assert_eq!(choices("osc_a_table"), Table::variants().len());
    assert_eq!(choices("osc_b_table"), Table::variants().len());
    assert_eq!(choices("mod1_dst"), ModTarget::variants().len());
    assert_eq!(choices("mod1_src"), ModSource::variants().len());
    assert_eq!(
        Table::variants().len(),
        komponist_model::params::wavetable::TABLES.len()
    );
}
