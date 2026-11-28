//! Harnesses for plugin-supplied input (#230): the fuzz targets in `fuzz/` and the proptests of
//! this crate run the same checks. Not an API: each function panics when it finds a broken
//! invariant, and any other outcome (a refusal included) is a pass.

use crate::native_layout::NativeLayout;
use crate::params::ParamInfo;
use crate::{device_assets, device_canvas, sampler_ui, signal_taps, waveform_markers};

/// Parses `bytes` as a native layout. A layout that parses answers every query, parses the same
/// way again, and can be checked against an inventory of its own parameters.
pub fn native_layout(bytes: &[u8]) {
    let Ok(layout) = NativeLayout::parse(bytes) else {
        return;
    };
    assert_eq!(NativeLayout::parse(bytes).as_ref(), Ok(&layout));
    let _ = (
        layout.has_canvas(),
        layout.has_audio_asset_editor(),
        layout.has_parameter_display(),
        layout.has_eq_band_controls(),
        layout.has_cell_selections(),
        layout.localized_plugin(),
        layout.author_ids().map(|ids| ids.len()),
        layout.parameter_alternates().count(),
    );
    let mut ids: Vec<u32> = layout
        .pages
        .iter()
        .flat_map(|page| &page.cells)
        .flat_map(|cell| cell.parameter_ids())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    for &id in &ids {
        let _ = (layout.parameter_display(id), layout.parameter_alternate(id));
    }
    // Two inventories: plain continuous parameters, and stepped ones.
    for steps in [0, 4] {
        let params: Vec<ParamInfo> = ids
            .iter()
            .map(|&id| ParamInfo {
                id,
                name: format!("p{id}"),
                module: String::new(),
                min: 0.0,
                max: if steps == 0 { 1.0 } else { f64::from(steps) },
                default: 0.0,
                step_count: steps,
                flags: 0,
            })
            .collect();
        let _ = layout.validate_params(&params);
    }
}

/// Decodes `bytes` as each of the plugin-supplied postcard and JSON messages of this crate (sampler
/// snapshot, waveform markers, device assets, asset edits, device canvas) and as a pad activity
/// frame. Whatever decodes is valid: it encodes again, to bytes that decode to the same encoding.
pub fn plugin_messages(bytes: &[u8]) {
    postcard_round_trip(
        bytes,
        sampler_ui::MAX_BYTES,
        sampler_ui::Snapshot::decode,
        |v, out| v.encode(out),
    );
    postcard_round_trip(
        bytes,
        waveform_markers::MAX_SNAPSHOT_BYTES,
        waveform_markers::Snapshot::decode,
        |v, out| v.encode(out),
    );
    postcard_round_trip(
        bytes,
        waveform_markers::MAX_EDIT_BYTES,
        waveform_markers::Request::decode,
        |v, out| v.encode(out),
    );
    postcard_round_trip(
        bytes,
        device_assets::MAX_SNAPSHOT_BYTES,
        device_assets::Snapshot::decode,
        |v, out| v.encode(out),
    );
    postcard_round_trip(
        bytes,
        device_assets::MAX_BINDINGS_BYTES,
        device_assets::Bindings::decode,
        |v, out| v.encode(out),
    );
    postcard_round_trip(
        bytes,
        device_assets::edit_state::MAX_EDITS_BYTES,
        device_assets::edit_state::Edits::decode,
        |v, out| v.encode(out),
    );
    // Encoding may refuse a valid list: its floats can serialise longer than the input wrote them
    // (`0` as `0.0`), past the size limit.
    if let Some(again) = device_canvas::DisplayList::decode(bytes).and_then(|list| list.encode()) {
        let decoded = device_canvas::DisplayList::decode(&again).expect("and decodes again");
        assert_eq!(decoded.encode().as_ref(), Some(&again));
    }
    // A pad activity frame: the bytes as `f32` samples, the first byte as the pad count.
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let count = usize::from(bytes.first().copied().unwrap_or(0) % 10);
    if let Some(activity) = signal_taps::PadActivity::decode(&samples, count) {
        assert!(activity.last_pad < count);
        assert!(
            activity
                .playheads
                .iter()
                .flatten()
                .all(|p| (0.0..=1.0).contains(p))
        );
    }
}

/// Decodes with `decode`; a value must encode with `encode` into `max` bytes and decode again to
/// a value that encodes to the same bytes (bytes, not values: a NaN is not `==` itself).
fn postcard_round_trip<T>(
    bytes: &[u8],
    max: usize,
    decode: impl Fn(&[u8]) -> Option<T>,
    encode: impl Fn(&T, &mut [u8]) -> Option<usize>,
) {
    let Some(value) = decode(bytes) else {
        return;
    };
    let mut out = vec![0; max];
    let n = encode(&value, &mut out).expect("a decoded message encodes");
    let again = out[..n].to_vec();
    let decoded = decode(&again).expect("and decodes again");
    let n = encode(&decoded, &mut out).expect("and encodes again");
    assert_eq!(out[..n], again);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::Value;

    const LAYOUTS: [&[u8]; 2] = [
        include_bytes!("../../../test-plugins/native-layout/c-fixture/controls.json"),
        include_bytes!("../../../docs/clap-device-layout/example.json"),
    ];

    /// Valid messages of every kind `plugin_messages` decodes.
    fn message_seeds() -> Vec<Vec<u8>> {
        let sampler = sampler_ui::Snapshot {
            pads: vec![sampler_ui::Pad {
                status: sampler_ui::Status::Ready,
                name: "Kick".into(),
                length: 1.5,
                peaks: vec![0.5; 8],
                playhead: Some(0.25),
            }],
            last_hit: (3, 0),
            transients: vec![0.1, 0.2],
            sync: Some(sampler_ui::Sync::Ready),
            source_rate: Some(48_000),
            idle: false,
        };
        let options = waveform_markers::SliceOptions {
            gate: true,
            reverse: Some(false),
            fade_in_ms: Some(5.0),
            fade_out_ms: Some(10.0),
        };
        let markers = waveform_markers::Snapshot {
            revision: 7,
            editable: true,
            positions: vec![0.5, 1.0],
            slice_options: Some(vec![options; 3]),
            default_options: Some(options),
            preparation: None,
        };
        let request = waveform_markers::Request {
            revision: 7,
            operation: waveform_markers::Edit::Move {
                index: 1,
                seconds: 0.75,
            },
        };
        let assets = device_assets::Snapshot {
            slots: vec![device_assets::Slot {
                id: 1,
                role: "sample".into(),
                label: "Sample".into(),
                kind: device_assets::AssetKind::AudioSample,
                status: device_assets::AssetStatus::Failed("missing".into()),
            }],
        };
        let bindings = device_assets::Bindings {
            slots: vec![device_assets::Binding {
                id: 1,
                path: Some("/a.wav".into()),
                reload: true,
            }],
        };
        let edits = device_assets::edit_state::Edits {
            slots: vec![device_assets::edit_state::Edit {
                slot: 1,
                intent: device_assets::edit_state::Intent::Relink,
            }],
        };
        vec![
            postcard::to_stdvec(&sampler).unwrap(),
            postcard::to_stdvec(&markers).unwrap(),
            postcard::to_stdvec(&request).unwrap(),
            postcard::to_stdvec(&assets).unwrap(),
            postcard::to_stdvec(&bindings).unwrap(),
            postcard::to_stdvec(&edits).unwrap(),
            br#"{"frames":[{"key":"env","primitives":[{"kind":"line","from":[0,0],"to":[1,1],"tone":"accent"}]}]}"#.to_vec(),
        ]
    }

    /// Every seed is accepted, so the mutations start from messages that get past the decoders.
    #[test]
    fn the_seeds_decode() {
        let seeds = message_seeds();
        assert!(sampler_ui::Snapshot::decode(&seeds[0]).is_some());
        assert!(waveform_markers::Snapshot::decode(&seeds[1]).is_some());
        assert!(waveform_markers::Request::decode(&seeds[2]).is_some());
        assert!(device_assets::Snapshot::decode(&seeds[3]).is_some());
        assert!(device_assets::Bindings::decode(&seeds[4]).is_some());
        assert!(device_assets::edit_state::Edits::decode(&seeds[5]).is_some());
        assert!(device_canvas::DisplayList::decode(&seeds[6]).is_some());
        for layout in LAYOUTS {
            assert!(NativeLayout::parse(layout).is_ok());
        }
    }

    /// The JSON pointers of every value in `value`.
    fn pointers(value: &Value, at: String, out: &mut Vec<String>) {
        match value {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    pointers(item, format!("{at}/{i}"), out);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    pointers(item, format!("{at}/{key}"), out);
                }
            }
            _ => {}
        }
        out.push(at);
    }

    /// Values a hostile plugin puts where the layout expects something else.
    fn hostile_value(pick: u8) -> Value {
        match pick % 12 {
            0 => Value::Null,
            1 => Value::Bool(true),
            2 => Value::from(-1),
            3 => Value::from(u64::MAX),
            4 => Value::from(u32::MAX),
            5 => Value::from(1e308),
            6 => Value::from(0.5),
            7 => Value::from(""),
            8 => Value::from("x".repeat(300)),
            9 => Value::from("ä/\u{0}"),
            10 => Value::Array(Vec::new()),
            _ => Value::Object(Default::default()),
        }
    }

    proptest! {
        /// Layouts with values replaced, removed or duplicated (the `native_layout` fuzz target's
        /// harness, starting from the valid fixtures).
        #[test]
        fn damaged_layouts_parse_or_are_refused(
            seed in 0usize..LAYOUTS.len(),
            edits in proptest::collection::vec((any::<usize>(), any::<u8>(), 0u8..3), 1..6),
        ) {
            let mut doc: Value = serde_json::from_slice(LAYOUTS[seed]).unwrap();
            for (at, pick, how) in edits {
                let mut all = Vec::new();
                pointers(&doc, String::new(), &mut all);
                let pointer = &all[at % all.len()];
                let copy = doc.pointer(pointer).cloned();
                let Some(target) = doc.pointer_mut(pointer) else { continue };
                match how {
                    0 => *target = hostile_value(pick),
                    1 => match target {
                        Value::Array(items) if !items.is_empty() => {
                            items.remove(usize::from(pick) % items.len());
                        }
                        Value::Object(map) if !map.is_empty() => {
                            let key = map.keys().nth(usize::from(pick) % map.len()).cloned().unwrap();
                            map.remove(&key);
                        }
                        _ => *target = hostile_value(pick),
                    },
                    _ => {
                        if let (Value::Array(items), Some(copy)) = (target, copy) {
                            items.push(copy);
                        }
                    }
                }
            }
            native_layout(&serde_json::to_vec(&doc).unwrap());
        }

        /// Byte-level damage to the layouts.
        #[test]
        fn layout_bytes_with_flips_parse_or_are_refused(
            seed in 0usize..LAYOUTS.len(),
            flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        ) {
            let mut bytes = LAYOUTS[seed].to_vec();
            for (at, value) in flips {
                let len = bytes.len();
                bytes[at % len] = value;
            }
            native_layout(&bytes);
        }

        /// Damaged plugin messages (the `plugin_messages` fuzz target's harness).
        #[test]
        fn damaged_plugin_messages_decode_or_are_refused(
            seed in 0usize..7,
            flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..8),
            cut in proptest::option::of(any::<usize>()),
        ) {
            let mut bytes = message_seeds().swap_remove(seed);
            for (at, value) in flips {
                let len = bytes.len();
                bytes[at % len] = value;
            }
            if let Some(cut) = cut {
                bytes.truncate(cut % (bytes.len() + 1));
            }
            plugin_messages(&bytes);
        }

        /// Arbitrary bytes.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            plugin_messages(&bytes);
            native_layout(&bytes);
        }
    }
}
