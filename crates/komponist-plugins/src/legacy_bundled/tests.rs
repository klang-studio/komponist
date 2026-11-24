use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
use komponist_model::edits::{self, NewClip, NewClipContent, NewDevice, NewTrack};
use komponist_model::macros::Mapping;
use komponist_model::{ChannelRef, DeviceId, Project, Sidechain, Tick};

use serde_json::json;

use super::*;

fn historical_volt() -> Device {
    serde_json::from_str(include_str!("volt-historic.json")).expect("legacy project device")
}

fn inventory(kind: BuiltinKind) -> (PluginDescriptor, Vec<ClapParamInfo>, Value) {
    let params = legacy_infos(kind);
    let descriptor = PluginDescriptor {
        localized: None,
        format: ClapFormat::Clap,
        id: clap_id(kind).into(),
        name: kind.catalog().name.into(),
        vendor: "klang-studio".into(),
        version: "0.1.0".into(),
        kind: match kind.catalog().role {
            DeviceRole::Instrument => PluginKind::Instrument,
            DeviceRole::Effect => PluginKind::Effect,
            DeviceRole::MidiEffect => PluginKind::MidiEffect,
        },
        features: Vec::new(),
        bundle: PathBuf::new(),
        sidechain: kind.catalog().sidechain,
    };
    let aliases: BTreeMap<_, _> = params
        .iter()
        .enumerate()
        .map(|(index, info)| (info.id.as_str(), 4000 + index as u32))
        .collect();
    let raw = params
        .iter()
        .map(|info| {
            let steps = info.steps();
            ClapParamInfo {
                id: aliases[info.id.as_str()],
                name: info.name.clone(),
                module: String::new(),
                min: 0.0,
                max: f64::from(steps.max(1)),
                default: 0.0,
                step_count: steps,
                flags: (if steps > 0 { param_flags::STEPPED } else { 0 })
                    | (if info.automatable {
                        param_flags::AUTOMATABLE
                    } else {
                        0
                    }),
            }
        })
        .collect::<Vec<_>>();
    let layout = json!({
        "schema_version": 1, "default_locale": "en", "locales": ["en"],
        "author_ids": aliases,
        "pages": [{
            "key": "main", "labels": {"en": "Main"}, "columns": 1,
            "cells": [{"widget": "knob", "col": 0, "row": 0, "cols": 1, "rows": 1,
                       "param_id": raw[0].id}],
        }],
    });
    (descriptor, raw, layout)
}

fn decode_state(bytes: &[u8]) -> Value {
    let len = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    assert_eq!(len as usize, bytes.len() - 8);
    serde_json::from_slice(&bytes[8..]).unwrap()
}

#[test]
fn legacy_ids_resolve_once_and_cannot_elevate_a_clap_identity() {
    let all = BuiltinKind::all()
        .map(|kind| {
            let reference = kind.catalog().plugin_ref();
            let id = bundled_clap_id(&reference).unwrap();
            (reference.id, id)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(all.len(), 12);
    assert_eq!(all.values().collect::<BTreeSet<_>>().len(), 12);
    assert_eq!(all["knurr"], "org.klang-studio.komponist.volt");
    assert_eq!(all["surf"], "org.klang-studio.komponist.surf");
    assert_eq!(all["pisynth"], "org.klang-studio.komponist.hexa");
    assert_eq!(all["tonleiter"], "org.klang-studio.komponist.scale");
    let mut impostor = historical_volt().plugin;
    impostor.format = PluginFormat::Clap;
    assert_eq!(bundled_clap_id(&impostor), None);
}

#[test]
fn historic_volt_maps_the_same_automation_and_preset_without_mutating_the_source() {
    let old = historical_volt();
    let before = serde_json::to_vec(&old).unwrap();
    let kind = BuiltinKind::from_plugin(&old.plugin).unwrap();
    let (descriptor, params, document) = inventory(kind);
    let layout = NativeLayout::parse(&document.to_string().into_bytes()).unwrap();
    let prepared = prepare(&old, kind, &descriptor, &layout, &params, "volt.clap", None).unwrap();

    assert_eq!(prepared.replacement.id, old.id);
    assert_eq!(prepared.replacement.bypass, old.bypass);
    assert_eq!(prepared.replacement.role, old.role);
    assert_eq!(prepared.replacement.sidechain, old.sidechain);
    assert_eq!(prepared.replacement.plugin.format, PluginFormat::Clap);
    assert_eq!(prepared.legacy_preset.as_deref(), Some("Fette Säge"));

    let state = decode_state(prepared.replacement.state.as_deref().unwrap());
    let preset: String =
        serde_json::from_str(state["fields"]["factory_preset"].as_str().unwrap()).unwrap();
    assert_eq!(preset, "Fat Saw");
    assert_eq!(state["params"]["osc_a_wave"]["i32"], 2);
    assert_eq!(state["params"]["octave"]["i32"], -1);
    let cutoff_id = ParamId::new("cutoff");
    let old_infos = legacy_infos(kind);
    let cutoff = old_infos
        .iter()
        .find(|input| input.id == cutoff_id)
        .unwrap();
    let binding = prepared.parameter(&cutoff_id).unwrap();
    assert_eq!(
        binding.id,
        ParamId::plugin(layout.author_ids().unwrap()["cutoff"])
    );
    assert_eq!(
        state["params"]["cutoff"]["f32"].as_f64().unwrap(),
        f64::from(cutoff.to_normalized(1370.0) as f32)
    );
    for position in [0.13, 0.37, 0.59, 0.82] {
        let legacy = cutoff.to_plain(position);
        let ordinary_linear = cutoff.range().0 + position * (cutoff.range().1 - cutoff.range().0);
        assert!((legacy - ordinary_linear).abs() > 1.0);
        assert_eq!(legacy, cutoff.to_plain(binding.clap_plain(position)));
    }
    let octave = old_infos
        .iter()
        .find(|input| input.id.as_str() == "octave")
        .unwrap();
    let binding = prepared.parameter(&octave.id).unwrap();
    for position in [0.00, 0.24, 0.49, 0.51, 0.76, 1.00] {
        assert_eq!(
            octave.snap(octave.to_plain(position)),
            octave.range().0 + binding.clap_plain(position)
        );
    }

    // If live metadata claims a different value domain, preparation fails with the old
    // serialized device untouched. There is no fallback that grants native routing.
    let mut incompatible = params;
    incompatible
        .iter_mut()
        .find(|param| param.id == layout.author_ids().unwrap()["cutoff"])
        .unwrap()
        .max = 2.0;
    assert!(
        prepare(
            &old,
            kind,
            &descriptor,
            &layout,
            &incompatible,
            "volt.clap",
            None
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&old).unwrap(), before);
}

#[test]
fn sampler_keeps_project_relative_assets_and_rebases_an_existing_source() {
    let project = tempfile::tempdir().unwrap();
    let asset = project.path().join("drum.wav");
    std::fs::write(&asset, b"for existence check only").unwrap();
    let kind = BuiltinKind::Internal(Internal::GeneralSampler);
    let old = br#"{"params":{"gain":-12},"samples":{"1":{"abs":"/before/drum.wav","rel":"drum.wav","name":"drum"}},"sample_edit":{"slices":[0.1,0.2]}}"#;
    let (payload, _) = convert_state(
        kind,
        Some(old),
        &legacy_infos(kind),
        "0.1.0",
        Some(project.path()),
    )
    .unwrap();
    let state = decode_state(&payload.unwrap());
    let asset_state: Value = serde_json::from_str(
        state["fields"][pinned_clap_state::SAMPLE_ASSETS_FIELD]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(asset_state["samples"]["1"]["abs"], asset.to_str().unwrap());
    assert_eq!(asset_state["samples"]["1"]["rel"], "drum.wav");
    assert_eq!(asset_state["sample_edit"]["slices"], json!([0.1, 0.2]));
    assert!(
        convert_state(
            kind,
            Some(br#"{"params":{},"unknown":{"keep":true}}"#),
            &legacy_infos(kind),
            "0.1.0",
            Some(project.path())
        )
        .is_err()
    );
}

#[test]
fn hexa_conversion_retains_the_entire_factory_patch() {
    let original = komponist_hexa::preset_patch(1).unwrap();
    let bytes = komponist_hexa::encode_state(&original);
    let kind = BuiltinKind::Hexa;
    let (state, _) = convert_state(kind, Some(&bytes), &legacy_infos(kind), "0.1.0", None).unwrap();
    let state = decode_state(&state.unwrap());
    let inner: Value =
        serde_json::from_str(state["fields"]["hexa-patch-v1"].as_str().unwrap()).unwrap();
    let patch: komponist_hexa::Patch = serde_json::from_value(inner["patch"].clone()).unwrap();
    assert_eq!(patch, original);
    assert_eq!(state["params"].as_object().unwrap().len(), WIRE_COUNT);
}

#[test]
fn author_aliases_must_cover_the_unique_live_inventory() {
    let kind = BuiltinKind::from_plugin(&historical_volt().plugin).unwrap();
    let (_, params, document) = inventory(kind);
    let parse = |value: &Value| NativeLayout::parse(&value.to_string().into_bytes());
    assert!(parse(&document).unwrap().validate_params(&params).is_ok());

    let mut old = document.clone();
    old.as_object_mut().unwrap().remove("author_ids");
    let old = parse(&old).unwrap();
    assert!(old.validate_params(&params).is_ok());
    assert!(old.author_ids().is_none());

    let mut invalid = document.clone();
    invalid["author_ids"]["cutoff"] = invalid["author_ids"]["osc_a_wave"].clone();
    assert!(parse(&invalid).is_err());

    let mut invalid = document;
    invalid["author_ids"]["cutoff"] = json!(99999);
    assert!(parse(&invalid).unwrap().validate_params(&params).is_err());
}

fn fixture_plan(device: &Device) -> LegacyDevicePlan {
    let kind = BuiltinKind::from_plugin(&device.plugin).unwrap();
    let (descriptor, params, document) = inventory(kind);
    let layout = NativeLayout::parse(&document.to_string().into_bytes()).unwrap();
    prepare(
        device,
        kind,
        &descriptor,
        &layout,
        &params,
        "bundle.clap",
        None,
    )
    .unwrap()
}

fn target(device: DeviceId, param: &str) -> AutomationTarget {
    AutomationTarget::Param {
        device,
        param: ParamId::new(param),
    }
}

fn legacy_project() -> (Project, DeviceId, DeviceId) {
    let volt = historical_volt();
    let (project, track) = edits::add_track(
        &Project::new("Historical project"),
        NewTrack {
            name: Some("Synth".into()),
            hue: None,
            instrument: Some(volt.plugin.clone()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (mut project, master_id) = edits::insert_device(
        &project,
        ChannelRef::Master,
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: BuiltinKind::Internal(Internal::Presse)
                .catalog()
                .plugin_ref(),
            state: None,
            sidechain: None,
        },
    )
    .unwrap();
    let master = Arc::make_mut(&mut project.master);
    Arc::make_mut(&mut master.devices[0]).sidechain = Some(Sidechain::new(track));
    let instrument = Arc::make_mut(&mut project.tracks[0]);
    let instrument = Arc::make_mut(&mut instrument.channel.devices[0]);
    let volt_id = instrument.id;
    *instrument = Device {
        id: volt_id,
        ..volt
    };

    let (project, clip) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick(30),
            length: Tick(960),
            name: "MIDI".into(),
            content: NewClipContent::Midi {
                notes: Vec::new(),
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let cutoff = target(volt_id, "cutoff");
    let points = Points::new(vec![
        Point {
            tick: Tick(0),
            value: 0.19,
            shape: Shape::Curve { tension: 0.4 },
        },
        Point::linear(Tick(180), 0.73),
    ]);
    let project =
        edits::automation::add_clip_automation(&project, clip, cutoff.clone(), "Cutoff".into())
            .unwrap();
    let mut project =
        edits::automation::set_clip_points(&project, clip, &cutoff, points.clone()).unwrap();
    let track = Arc::make_mut(&mut project.tracks[0]);
    Arc::make_mut(&mut track.automation.lanes[0]).points = points;
    track.macros.0[0].value = 0.41;
    track.macros.0[0].mappings.push(Mapping {
        target: target(volt_id, "octave"),
        label: "Octave".into(),
        min: 0.24,
        max: 0.76,
        invert: true,
        bend: -0.2,
    });
    let (project, _) = edits::automation::add_lane(
        &project,
        AutomationOwner::Master,
        target(master_id, "threshold"),
        "Threshold".into(),
    )
    .unwrap();
    (project, volt_id, master_id)
}

#[test]
fn migrates_all_project_parameter_references_and_keeps_the_source() {
    let (source, volt, master) = legacy_project();
    let volt_plan = fixture_plan(&source.tracks[0].channel.devices[0]);
    let master_plan = fixture_plan(&source.master.devices[0]);
    let before = serde_json::to_vec(&source).unwrap();
    let mut expected = source.clone();
    let mapped = |id, plan: &LegacyDevicePlan, param| AutomationTarget::Param {
        device: id,
        param: plan.parameter(&ParamId::new(param)).unwrap().id.clone(),
    };
    let cutoff = mapped(volt, &volt_plan, "cutoff");
    let track = Arc::make_mut(&mut expected.tracks[0]);
    track.channel.devices[0] = Arc::new(volt_plan.replacement().clone());
    Arc::make_mut(&mut track.automation.lanes[0]).target = cutoff.clone();
    Arc::make_mut(&mut track.clips[0]).automation[0].target = cutoff;
    track.macros.0[0].mappings[0].target = mapped(volt, &volt_plan, "octave");
    Arc::make_mut(&mut expected.master_automation).lanes[0] =
        Arc::new(komponist_model::automation::Lane {
            target: mapped(master, &master_plan, "threshold"),
            ..(*source.master_automation.lanes[0]).clone()
        });
    Arc::make_mut(&mut expected.master).devices[0] = Arc::new(master_plan.replacement().clone());

    let migration = migrate_legacy_bundled_project(&source, [volt_plan, master_plan]).unwrap();
    assert_eq!(migration.project, expected);
    assert_eq!(
        migration.legacy_presets,
        BTreeMap::from([(volt, "Fette Säge".into())])
    );
    assert_eq!(serde_json::to_vec(&source).unwrap(), before);
}

#[test]
fn an_unknown_target_rejects_the_entire_copy() {
    let (mut source, volt, _) = legacy_project();
    let track = Arc::make_mut(&mut source.tracks[0]);
    Arc::make_mut(&mut track.clips[0]).automation[0].target = target(volt, "unknown");
    let plans = [
        fixture_plan(&source.master.devices[0]),
        fixture_plan(&track.channel.devices[0]),
    ];
    let before = serde_json::to_vec(&source).unwrap();
    let error = migrate_legacy_bundled_project(&source, plans).unwrap_err();
    assert!(error.contains("unknown legacy parameter unknown"));
    assert_eq!(serde_json::to_vec(&source).unwrap(), before);
}

#[test]
fn rejects_stale_missing_duplicate_and_cross_channel_plans() {
    let (source, volt, _) = legacy_project();
    let plan = fixture_plan(&source.tracks[0].channel.devices[0]);

    let mut stale = source.clone();
    let track = Arc::make_mut(&mut stale.tracks[0]);
    Arc::make_mut(&mut track.channel.devices[0]).state = None;
    assert!(
        migrate_legacy_bundled_project(&stale, [plan.clone()])
            .unwrap_err()
            .contains("changed")
    );

    assert!(
        migrate_legacy_bundled_project(&Project::new("Empty"), [plan.clone()])
            .unwrap_err()
            .contains("missing")
    );
    assert!(
        migrate_legacy_bundled_project(&source, [plan.clone(), plan.clone()])
            .unwrap_err()
            .contains("duplicate migration plan")
    );

    let mut cross_channel = source.clone();
    let lane = &mut Arc::make_mut(&mut cross_channel.master_automation).lanes[0];
    Arc::make_mut(lane).target = target(volt, "cutoff");
    assert!(
        migrate_legacy_bundled_project(&cross_channel, [plan])
            .unwrap_err()
            .contains("another channel")
    );
}

#[test]
fn rejects_changes_to_the_automation_domain() {
    let (source, _, _) = legacy_project();
    let mut plan = fixture_plan(&source.tracks[0].channel.devices[0]);
    let octave = ParamId::new("octave");
    let binding = ClapParamInfo {
        id: plan.params[&octave].id.plugin_id().unwrap(),
        name: "Octave".into(),
        module: String::new(),
        min: 0.0,
        max: 4.0,
        default: 2.0,
        step_count: 4,
        flags: param_flags::STEPPED | param_flags::AUTOMATABLE,
    };
    let incompatible = ParamInfo::new(
        octave.clone(),
        "Octave",
        ParamKind::Float {
            min: 1.0,
            max: 9.0,
            scale: Scale::Log,
            steps: 4,
        },
        3.0,
    );
    plan.params.get_mut(&octave).unwrap().automation_issue =
        automation_issue(&incompatible, &binding);
    let error = migrate_legacy_bundled_project(&source, [plan]).unwrap_err();
    assert!(error.contains("nonlinear stepped range"));
}
