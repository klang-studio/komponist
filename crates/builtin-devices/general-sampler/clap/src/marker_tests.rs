//! Exercise the waveform marker extension and state through the exported CLAP entry point.

use std::ffi::{c_char, c_void};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::version::CLAP_VERSION;
use komponist_plugin_api::device_assets::edit_state;
use komponist_plugin_api::{device_assets, waveform_markers as wire};
use serde_json::{Value, json};
use wire::{Edit, EditStatus, Request, SliceOptions, Snapshot};

use crate::clap_entry;
use crate::layout_contract::Instance;
use crate::sampler_contract::{load_state, save_state};

const FRAMES: usize = 4097;
const RATE: u32 = 48_000;
const END: f64 = FRAMES as f64 / RATE as f64;

unsafe extern "C" fn no_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

unsafe extern "C" fn ignore_request(_: *const clap_host) {}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"Waveform marker test host".as_ptr(),
    vendor: c"klang-studio".as_ptr(),
    url: c"https://github.com/klang-studio/komponist".as_ptr(),
    version: c"1".as_ptr(),
    get_extension: Some(no_extension),
    request_restart: Some(ignore_request),
    request_process: Some(ignore_request),
    request_callback: Some(ignore_request),
};

struct Entry;

impl Entry {
    fn open() -> Self {
        // SAFETY: The module path remains live for the entry call.
        assert!(unsafe { (clap_entry.init.unwrap())(c"general-sampler.clap".as_ptr()) });
        Self
    }

    fn instance(&self) -> Instance {
        // SAFETY: This entry is initialized, the static host outlives the instance, and all
        // metadata and callback tables come from this module's exported CLAP factory.
        unsafe {
            let raw = (clap_entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr());
            let factory = raw.cast::<clap_plugin_factory>().as_ref().unwrap();
            assert_eq!((factory.get_plugin_count.unwrap())(factory), 1);
            let descriptor = (factory.get_plugin_descriptor.unwrap())(factory, 0)
                .as_ref()
                .unwrap();
            let plugin = (factory.create_plugin.unwrap())(factory, &HOST, descriptor.id);
            assert!(!plugin.is_null());
            let instance = Instance(plugin);
            assert!(((*plugin).init.unwrap())(plugin));
            instance
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: The test drops all plugin instances before closing the initialized entry.
        unsafe { (clap_entry.deinit.unwrap())() };
    }
}

struct Markers<'a> {
    instance: &'a Instance,
    table: &'a wire::Extension,
}

impl<'a> Markers<'a> {
    fn get(instance: &'a Instance) -> Self {
        let plugin = instance.0;
        // SAFETY: The factory initialized this instance before querying its versioned extension.
        let table = unsafe {
            ((*plugin).get_extension.unwrap())(plugin, wire::EXTENSION_ID.as_ptr())
                .cast::<wire::Extension>()
                .as_ref()
                .expect("waveform marker extension")
        };
        Self { instance, table }
    }

    fn snapshot(&self) -> Snapshot {
        let mut bytes = [0; wire::MAX_SNAPSHOT_BYTES];
        let plugin = self.instance.0;
        // SAFETY: The instance and bounded writable buffer live through the synchronous call.
        let size = unsafe {
            (self.table.snapshot.unwrap())(plugin.cast(), 0, bytes.as_mut_ptr(), bytes.len() as u32)
        };
        Snapshot::decode(&bytes[..size as usize]).expect("valid marker snapshot")
    }

    fn edit(&self, revision: u64, operation: Edit) -> u32 {
        let mut bytes = [0; wire::MAX_EDIT_BYTES];
        let size = Request {
            revision,
            operation,
        }
        .encode(&mut bytes)
        .unwrap();
        // SAFETY: The instance and encoded request live through the synchronous call.
        unsafe {
            (self.table.edit.unwrap())(self.instance.0.cast(), 0, bytes.as_ptr(), size as u32)
        }
    }

    fn apply(&self, operation: Edit) -> Snapshot {
        let before = self.snapshot();
        assert_eq!(
            self.edit(before.revision, operation),
            EditStatus::Applied as u32
        );
        let after = self.snapshot();
        assert_ne!(before.revision, after.revision);
        after
    }

    fn wait_until_editable(&self) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = self.snapshot();
            if snapshot.editable {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "the sample never became editable"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn write_wave(path: &Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..FRAMES {
        writer
            .write_sample(if frame % 16 < 8 { 1000i16 } else { -1000i16 })
            .unwrap();
    }
    writer.finalize().unwrap();
}

fn bind_file(instance: &Instance, path: &Path) {
    let plugin = instance.0;
    let binding = device_assets::Bindings {
        slots: vec![device_assets::Binding {
            id: 0,
            path: Some(path.to_str().unwrap().into()),
            reload: false,
        }],
    };
    let mut bytes = vec![0; device_assets::MAX_BINDINGS_BYTES];
    let size = binding.encode(&mut bytes).unwrap();
    // SAFETY: The initialized instance and encoded slot update live through the call.
    unsafe {
        let table =
            ((*plugin).get_extension.unwrap())(plugin, device_assets::EXTENSION_ID.as_ptr())
                .cast::<device_assets::Extension>()
                .as_ref()
                .unwrap();
        assert_eq!(
            (table.set_bindings.unwrap())(plugin.cast(), bytes.as_ptr(), size as u32),
            1
        );
    }
}

fn prepare_asset_edit(
    instance: &Instance,
    slot: u32,
    intent: edit_state::Intent,
    source: Option<&[u8]>,
) -> (u32, Vec<u8>, Vec<u32>) {
    unsafe extern "C" fn copy_state(output: *mut c_void, state: *const u8, len: u32) -> bool {
        // SAFETY: The test supplies its output and the plugin borrows its state until return.
        unsafe {
            let output = &mut *output.cast::<(Vec<u8>, Vec<u32>)>();
            output
                .0
                .extend_from_slice(std::slice::from_raw_parts(state, len as usize));
        }
        true
    }

    unsafe extern "C" fn copy_resets(output: *mut c_void, ids: *const u32, len: u32) -> bool {
        // SAFETY: The test supplies its output and the plugin borrows the IDs until return.
        unsafe {
            let output = &mut *output.cast::<(Vec<u8>, Vec<u32>)>();
            output
                .1
                .extend_from_slice(std::slice::from_raw_parts(ids, len as usize));
        }
        true
    }

    let plugin = instance.0;
    let mut edits = [0; edit_state::MAX_EDITS_BYTES];
    let size = edit_state::Edits {
        slots: vec![edit_state::Edit { slot, intent }],
    }
    .encode(&mut edits)
    .unwrap();
    let (source, source_len) = source.map_or((ptr::null(), 0), |source| {
        (source.as_ptr(), source.len() as u32)
    });
    let mut replacement: (Vec<u8>, Vec<u32>) = (Vec::new(), Vec::new());
    // SAFETY: The initialized plugin and all borrowed buffers remain valid for this call.
    let result = unsafe {
        let table = ((*plugin).get_extension.unwrap())(plugin, edit_state::EXTENSION_ID.as_ptr())
            .cast::<edit_state::Extension>()
            .as_ref()
            .expect("asset edit state extension");
        (table.prepare.unwrap())(
            plugin.cast(),
            edits.as_ptr(),
            size as u32,
            source,
            source_len,
            ptr::from_mut(&mut replacement).cast(),
            copy_state,
            copy_resets,
        )
    };
    (result, replacement.0, replacement.1)
}

fn check_asset_edit_preparation(instance: &Instance, source: &Value, assets: &Value) {
    use edit_state::Intent;
    use komponist_device_core::pinned_clap_state::{decode, encode_document};

    let live = save_state(instance);
    let mut state = source.clone();
    state["params"]["mode"] = json!({"string": "one-shot"});
    let input = encode_document(&state);
    for intent in [Intent::Assign, Intent::Clear] {
        let (result, replacement, resets) = prepare_asset_edit(instance, 0, intent, Some(&input));
        assert_eq!(result, edit_state::REPLACED);
        let mut expected = state.clone();
        let mut expected_assets = assets.clone();
        expected_assets
            .as_object_mut()
            .unwrap()
            .remove("sample_edit");
        expected["fields"]["sample_assets"] = expected_assets.to_string().into();
        if intent == Intent::Assign {
            // Assigning a file resets the trim to its defaults and reports the two parameters.
            expected["params"]["start"] = json!({"f32": 0.0});
            expected["params"]["end"] = json!({"f32": 1.0});
            assert_eq!(resets.len(), 2);
        } else {
            assert!(resets.is_empty());
        }
        assert_eq!(decode(&replacement), Some(expected));
    }
    let (result, replacement, resets) = prepare_asset_edit(instance, 0, Intent::Assign, None);
    assert_eq!(result, edit_state::REPLACED);
    assert_eq!(resets.len(), 2);
    assert!(decode(&replacement).is_some());
    for (slot, intent, input, result) in [
        (
            0,
            Intent::Relink,
            Some(input.as_slice()),
            edit_state::UNCHANGED,
        ),
        (
            1,
            Intent::Assign,
            Some(input.as_slice()),
            edit_state::REJECTED,
        ),
        (0, Intent::Clear, Some(&[1, 2][..]), edit_state::REJECTED),
    ] {
        assert_eq!(
            prepare_asset_edit(instance, slot, intent, input),
            (result, vec![], vec![])
        );
    }
    assert_eq!(save_state(instance), live);
}

#[test]
fn waveform_markers_commit_to_legacy_clap_state() {
    let scratch = tempfile::tempdir().unwrap();
    let sample = scratch.path().join("original.wav");
    write_wave(&sample);
    let entry = Entry::open();
    let instance = entry.instance();
    let markers = Markers::get(&instance);
    let first = markers.snapshot();
    let insert = Edit::Insert {
        seconds: 0.027_343_750_01,
    };
    assert!(!first.editable);
    assert!(first.slice_options.is_none());
    assert_eq!(
        markers.edit(first.revision, insert),
        EditStatus::Unavailable as u32
    );
    // SAFETY: The live instance accepts a bounded writable buffer. The null input and unknown
    // slot deliberately exercise the extension's invalid-argument results.
    unsafe {
        let plugin = instance.0.cast();
        let mut bytes = [0; 1];
        let snapshot = markers.table.snapshot.unwrap();
        assert_eq!(snapshot(plugin, 0, bytes.as_mut_ptr(), 1), 0);
        assert_eq!(snapshot(plugin, 1, bytes.as_mut_ptr(), 1), 0);
        let edit = markers.table.edit.unwrap();
        assert_eq!(edit(plugin, 0, ptr::null(), 1), EditStatus::Invalid as u32);
        assert_eq!(
            edit(plugin, 0, bytes.as_ptr(), 1),
            EditStatus::Invalid as u32
        );
    }

    let mut legacy = save_state(&instance);
    legacy["params"]["mode"] = json!({"string": "slices"});
    legacy["params"]["start"] = json!({"f32": 0.125});
    legacy["params"]["end"] = json!({"f32": 0.875});
    legacy["params"]["fade_in"] = json!({"f32": 0.125});
    assert!(load_state(&instance, &legacy));
    let unloaded = markers.snapshot();
    assert!(!unloaded.editable);
    assert_eq!(unloaded.slice_options, Some(vec![SliceOptions::default()]));
    assert_eq!(
        markers.edit(unloaded.revision, Edit::DetectSlices),
        EditStatus::Unavailable as u32
    );
    let legacy_assets = json!({
        "samples": {"1": {"abs": sample, "rel": "collected/original.wav", "name": "Original"}},
        "other_setting": "retained",
        "sample_edit": {
            "first_slice": {"reverse": false, "fade_in_ms": null, "fade_out_ms": null, "gate": false},
            "slices": [0.015625, {"time": 0.0234375, "reverse": true, "fade_in_ms": 2.0, "fade_out_ms": 3.0, "gate": true}, 0.125]
        }
    });
    legacy["fields"]["sample_assets"] = legacy_assets.to_string().into();
    assert!(load_state(&instance, &legacy));
    check_asset_edit_preparation(&instance, &legacy, &legacy_assets);
    let ready = markers.wait_until_editable();
    assert_eq!(ready.positions, [0.015625, 0.0234375, 0.125]);
    let options = ready.slice_options.as_ref().unwrap();
    assert_eq!(options[0].reverse, Some(false));
    assert_eq!(options[1], SliceOptions::default());
    assert_eq!(
        options[2],
        SliceOptions {
            gate: true,
            reverse: Some(true),
            fade_in_ms: Some(2.0),
            fade_out_ms: Some(3.0),
        }
    );
    assert_eq!(ready.default_options.unwrap().reverse, Some(false));
    assert_eq!(ready.default_options.unwrap().fade_in_ms, Some(1250.0));
    assert_eq!(
        markers.edit(ready.revision, Edit::Insert { seconds: 0.005 }),
        EditStatus::Invalid as u32
    );
    let inserted = markers.apply(insert);
    assert_eq!(
        inserted.positions,
        [0.015625, 0.0234375, 0.027_343_750_01, 0.125]
    );
    assert_eq!(inserted.slice_options.as_ref().unwrap()[3], options[2]);
    assert_eq!(
        markers.edit(ready.revision, Edit::Remove { index: 1 }),
        EditStatus::Stale as u32
    );
    let moved = markers.apply(Edit::Move {
        index: 2,
        seconds: 0.03125,
    });
    assert_eq!(
        markers.edit(
            moved.revision,
            Edit::Move {
                index: 2,
                seconds: 0.03125
            }
        ),
        EditStatus::Unchanged as u32
    );
    assert_eq!(markers.snapshot(), moved);
    assert_eq!(
        markers.edit(
            moved.revision,
            Edit::Move {
                index: 2,
                seconds: 0.015625
            }
        ),
        EditStatus::Invalid as u32
    );

    let saved = save_state(&instance);
    assert_eq!(saved["params"], legacy["params"]);
    let assets: Value =
        serde_json::from_str(saved["fields"]["sample_assets"].as_str().unwrap()).unwrap();
    assert_eq!(assets["samples"], legacy_assets["samples"]);
    assert_eq!(assets["other_setting"], legacy_assets["other_setting"]);
    assert_eq!(
        assets["sample_edit"]["first_slice"],
        legacy_assets["sample_edit"]["first_slice"]
    );
    assert_eq!(
        assets["sample_edit"]["slices"],
        json!([
            0.015625,
            legacy_assets["sample_edit"]["slices"][1],
            {"time": 0.03125, "reverse": true, "fade_in_ms": 2.0, "fade_out_ms": 3.0, "gate": true},
            0.125
        ])
    );

    assert_eq!(
        markers.apply(Edit::Remove { index: 2 }).positions,
        ready.positions
    );
    let at_end = markers.apply(Edit::Move {
        index: 2,
        seconds: END,
    });
    assert_eq!(at_end.positions[2], END);
    assert!(load_state(&instance, &legacy));
    assert_eq!(
        markers.edit(at_end.revision, Edit::Remove { index: 2 }),
        EditStatus::Stale as u32
    );
    assert_eq!(markers.snapshot().positions, ready.positions);

    let other = entry.instance();
    let other_markers = Markers::get(&other);
    assert!(load_state(&other, &saved));
    assert_eq!(
        other_markers.wait_until_editable().positions,
        moved.positions
    );

    let before_options = markers.wait_until_editable();
    let first_options = SliceOptions {
        gate: true,
        reverse: Some(true),
        fade_in_ms: Some(0.0),
        fade_out_ms: None,
    };
    let change_first = Edit::SetSliceOptions {
        slice: 0,
        options: first_options,
    };
    let changed = markers.apply(change_first);
    assert_eq!(changed.positions, before_options.positions);
    assert_eq!(changed.slice_options.as_ref().unwrap()[0], first_options);
    assert_eq!(
        markers.edit(before_options.revision, change_first),
        EditStatus::Stale as u32
    );
    assert_eq!(
        markers.edit(changed.revision, change_first),
        EditStatus::Unchanged as u32
    );
    assert_eq!(
        markers.edit(
            changed.revision,
            Edit::SetSliceOptions {
                slice: 4,
                options: SliceOptions::default(),
            }
        ),
        EditStatus::Invalid as u32
    );
    let reset = markers.apply(Edit::SetSliceOptions {
        slice: 2,
        options: SliceOptions::default(),
    });
    assert_eq!(reset.positions, before_options.positions);
    assert_eq!(
        reset.slice_options.as_ref().unwrap()[2],
        SliceOptions::default()
    );
    let options_state = save_state(&instance);
    let options_assets: Value =
        serde_json::from_str(options_state["fields"]["sample_assets"].as_str().unwrap()).unwrap();
    assert_eq!(options_state["params"], legacy["params"]);
    assert_eq!(options_assets["samples"], legacy_assets["samples"]);
    assert_eq!(options_assets["other_setting"], "retained");
    assert_eq!(
        options_assets["sample_edit"]["first_slice"],
        json!({
            "gate": true, "reverse": true, "fade_in_ms": 0.0, "fade_out_ms": null
        })
    );
    assert_eq!(options_assets["sample_edit"]["slices"][1], 0.0234375);
    assert!(load_state(&instance, &legacy));
    let restored = markers.wait_until_editable();
    assert_eq!(restored.slice_options, before_options.slice_options);
    assert_eq!(restored.positions, before_options.positions);

    let detected = markers.apply(Edit::DetectSlices);
    assert_eq!(detected.slice_options.unwrap()[0], options[0]);
    assert!(
        detected
            .positions
            .iter()
            .all(|&time| (END * 0.125..END * 0.875).contains(&time))
    );
    assert_eq!(
        markers.edit(detected.revision, Edit::DetectSlices),
        EditStatus::Unchanged as u32
    );

    let before_relink = other_markers.snapshot();
    let replacement = scratch.path().join("replacement.wav");
    std::fs::copy(&sample, &replacement).unwrap();
    bind_file(&other, &replacement);
    assert_eq!(
        other_markers.edit(before_relink.revision, Edit::Remove { index: 0 }),
        EditStatus::Stale as u32
    );
    assert_eq!(
        other_markers.wait_until_editable().positions,
        moved.positions
    );
}
