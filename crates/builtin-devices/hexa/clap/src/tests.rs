use std::collections::BTreeSet;
use std::ffi::{c_char, c_void};
use std::io::{Cursor, Read};
use std::{ptr, sync::Arc};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, clap_event_header,
    clap_event_note, clap_input_events,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use komponist_native_clap_sdk::display::{DisplayList, EXTENSION_ID as DISPLAY_ID, FmFrame};
use komponist_native_clap_sdk::{NativeClapLayout, compile, compile_v2};
use nice_plug::wrapper::clap::Wrapper;

use super::*;

unsafe extern "C" fn no_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"Hexa test host".as_ptr(),
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
            let wrapper = Arc::into_raw(Wrapper::<HexaPlugin>::new(&HOST).unwrap());
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
        let panel = HexaPlugin::panel();
        let bytes = compile::<HexaPlugin>(first.0, &panel).unwrap();
        assert_eq!(bytes, compile::<HexaPlugin>(second.0, &panel).unwrap());
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
        let expected = infos(Lang::En)
            .iter()
            .map(|info| {
                Wrapper::<HexaPlugin>::custom_extension_param_id(first.0, info.id.as_str()).unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(expected.len(), infos(Lang::En).len());
        assert_eq!(ids, expected);
    }
}

#[test]
fn native_editor_preserves_wire_ids_and_links_the_operator_selector() {
    let instance = Instance::new();
    // SAFETY: This instance and its layout queries are confined to this thread.
    let (v1, v2) = unsafe {
        (
            compile::<HexaPlugin>(instance.0, &HexaPlugin::panel()).unwrap(),
            compile_v2::<HexaPlugin>(instance.0, &HexaPlugin::panel_v2().unwrap()).unwrap(),
        )
    };
    let legacy: serde_json::Value = serde_json::from_slice(&v1).unwrap();
    let native: serde_json::Value = serde_json::from_slice(&v2).unwrap();
    assert_eq!(native["author_ids"], legacy["author_ids"]);
    let pages = native["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0]["labels"]["de"], "Operatoren");
    assert_eq!(pages[1]["labels"]["en"], "Envelopes");
    let selectors: Vec<_> = pages[..2]
        .iter()
        .map(|page| {
            page["cells"]
                .as_array()
                .unwrap()
                .iter()
                .find_map(|cell| cell.get("selector"))
                .unwrap()
        })
        .collect();
    assert_eq!(selectors[0], selectors[1]);
    assert_eq!(selectors[0]["shared"], true);
    let canvas = pages[0]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|cell| cell.get("canvas"))
        .unwrap();
    assert_eq!(canvas["key"], fm_frame(&instance).key);
    assert_eq!(canvas["fm_topology"]["selector"], selectors[0]["key"]);
    assert_eq!(
        canvas["fm_topology"]["carriers"].as_array().unwrap().len(),
        6
    );
    assert_eq!(
        pages[1]["cells"][0]["graph"]["variants"][5]["slots"]["attack"],
        native["author_ids"]["op6_attack"]
    );
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

fn render_notes(plugin: *const clap_plugin) -> [[f32; FRAMES]; 2] {
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
    output
}

const FRAMES: usize = 512;
const NOTE_ON: usize = 37;
const NOTE_OFF: usize = 317;

unsafe extern "C" fn read_state(
    stream: *const clap_istream,
    buffer: *mut c_void,
    count: u64,
) -> i64 {
    // SAFETY: The CLAP call borrows the cursor and supplies writable storage for count bytes.
    unsafe {
        let cursor = &mut *(*stream).ctx.cast::<Cursor<&[u8]>>();
        let bytes = std::slice::from_raw_parts_mut(buffer.cast::<u8>(), count as usize);
        cursor.read(bytes).map_or(-1, |read| read as i64)
    }
}

unsafe extern "C" fn write_state(
    stream: *const clap_ostream,
    buffer: *const c_void,
    count: u64,
) -> i64 {
    // SAFETY: The CLAP call borrows the result vector and supplies readable storage for count bytes.
    unsafe {
        let output = &mut *(*stream).ctx.cast::<Vec<u8>>();
        let bytes = std::slice::from_raw_parts(buffer.cast::<u8>(), count as usize);
        output.extend_from_slice(bytes);
        count as i64
    }
}

fn state_extension(instance: &Instance) -> &clap_plugin_state {
    // SAFETY: The table is non-null and is bound by the instance lifetime.
    unsafe {
        let extension = ((*instance.0).get_extension.unwrap())(instance.0, CLAP_EXT_STATE.as_ptr());
        assert!(!extension.is_null());
        &*extension.cast()
    }
}

fn patch_from_saved_state(stream: &[u8]) -> komponist_hexa::Patch {
    let size = u64::from_le_bytes(stream[..8].try_into().unwrap());
    assert_eq!(stream.len() - 8, size as usize);
    let state: serde_json::Value = serde_json::from_slice(&stream[8..]).unwrap();
    let patch: serde_json::Value =
        serde_json::from_str(state["fields"]["hexa-patch-v1"].as_str().unwrap()).unwrap();
    serde_json::from_value(patch["patch"].clone()).unwrap()
}

/// The pinned CLAP state stream of `patch`.
fn clap_state(patch: &komponist_hexa::Patch) -> Vec<u8> {
    komponist_hexa::state::encode_clap_patch(*patch, env!("CARGO_PKG_VERSION"))
}

fn load_patch(instance: &Instance, patch: &komponist_hexa::Patch) {
    let input = clap_state(patch);
    let mut cursor = Cursor::new(input.as_slice());
    let stream = clap_istream {
        ctx: (&mut cursor as *mut Cursor<&[u8]>).cast(),
        read: Some(read_state),
    };
    // SAFETY: The initialized plugin and stream are exclusive to this thread.
    assert!(unsafe { (state_extension(instance).load.unwrap())(instance.0, &stream) });
}

fn fm_frame(instance: &Instance) -> FmFrame {
    #[repr(C)]
    struct DisplayTable {
        draw: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32,
    }
    let mut buffer = [0; 1024];
    // SAFETY: The initialized instance owns the extension and the buffer is writable.
    let len = unsafe {
        let table = ((*instance.0).get_extension.unwrap())(instance.0, DISPLAY_ID.as_ptr());
        assert!(!table.is_null());
        ((*table.cast::<DisplayTable>()).draw)(instance.0, buffer.as_mut_ptr(), buffer.len() as u32)
    };
    let display = DisplayList::decode(&buffer[..len as usize]).unwrap();
    assert_eq!(display.fm_topologies.len(), 1);
    display.fm_topologies.into_iter().next().unwrap()
}

#[test]
fn display_reports_the_fixed_or_free_routing_of_the_loaded_patch() {
    let instance = Instance::new();
    let mut patch = default_patch();
    komponist_hexa::algorithms::apply(&mut patch, 4);
    load_patch(&instance, &patch);
    let fixed = fm_frame(&instance);
    assert_eq!(fixed.algorithm, Some(5));
    assert_eq!(fixed.modulates, [0, 1, 0, 4, 0, 16]);
    assert_eq!(fixed.outputs, 0b010101);
    assert_eq!(fixed.feedback, Some(5));

    // A free routing can contain a cycle and an audible modulator.
    patch.operators[0].modulates[1] = true;
    patch.operators[1].feedback = 0.3;
    load_patch(&instance, &patch);
    let free = fm_frame(&instance);
    assert_eq!(free.algorithm, None);
    assert_eq!(free.modulates, [2, 1, 0, 4, 0, 16]);
    assert_eq!(free.outputs, 0b010101);
    assert_eq!(free.feedback, Some(1));
    assert!(free.is_carrier(0));
    assert!(!free.is_carrier(1));

    render_notes(instance.0);
    assert_eq!(fm_frame(&instance), free);
}

#[test]
fn imported_factory_patch_round_trips_and_matches_built_in_audio() {
    let patch = komponist_hexa::preset_patch(1).unwrap();
    let input = clap_state(&patch);
    let mut cursor = Cursor::new(input.as_slice());
    let stream = clap_istream {
        ctx: (&mut cursor as *mut Cursor<&[u8]>).cast(),
        read: Some(read_state),
    };
    let instance = Instance::new();
    let extension = state_extension(&instance);
    // SAFETY: The initialized plugin and stream are exclusive to this thread.
    assert!(unsafe { (extension.load.unwrap())(instance.0, &stream) });
    let actual = render_notes(instance.0);

    let mut saved = Vec::<u8>::new();
    let stream = clap_ostream {
        ctx: (&mut saved as *mut Vec<u8>).cast(),
        write: Some(write_state),
    };
    // SAFETY: The initialized plugin and stream are exclusive to this thread.
    assert!(unsafe { (extension.save.unwrap())(instance.0, &stream) });
    assert_eq!(patch_from_saved_state(&saved), patch);

    let mut reference = HexaProcessor::new(patch);
    reference.activate(48_000.0, FRAMES).unwrap();
    let events = [
        Event {
            offset: NOTE_ON as u32,
            kind: EventKind::NoteOn {
                key: 60,
                velocity: 102,
                channel: 0,
                note_id: 17,
            },
        },
        Event {
            offset: NOTE_OFF as u32,
            kind: EventKind::NoteOff {
                key: 60,
                channel: 0,
                note_id: 17,
            },
        },
    ];
    let mut expected = [[0.0f32; FRAMES]; 2];
    reference.render_events(
        &events,
        expected.each_mut().map(|channel| channel.as_mut_slice()),
    );
    assert!(expected.iter().flatten().any(|sample| sample.abs() > 0.001));
    for (at, (actual, expected)) in actual
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
