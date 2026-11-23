use std::collections::HashSet;
use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::edits::{
    NewBus, NewTrack, add_bus, add_send, add_track, duplicate_tracks, remove_device, remove_send,
    remove_tracks, replace_instrument,
};
use crate::macros::MACRO_COUNT;
use crate::params::ParamId;
use crate::{Document, PluginFormat, PluginRef};

fn instrument(name: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: name.to_owned(),
        name: name.to_owned(),
        vendor: String::new(),
        version: String::new(),
        module: Box::default(),
    }
}

fn track_with_device(p: &Project) -> (Project, TrackId, DeviceId) {
    let (p, id) = add_track(
        p,
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(instrument("Synth")),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = p.track(id).unwrap().channel.devices[0].id;
    (p, id, device)
}

fn parameter(device: DeviceId, id: u32) -> AutomationTarget {
    AutomationTarget::Param {
        device,
        param: ParamId::plugin(id),
    }
}

fn mapping(target: AutomationTarget) -> Mapping {
    Mapping {
        target,
        label: "Target".into(),
        min: 0.25,
        max: 0.75,
        invert: false,
        bend: 0.0,
    }
}

fn slot_at(p: &Project, track: TrackId, index: u8) -> &TrackMacro {
    &p.track(track).unwrap().macros.0[usize::from(index)]
}

#[test]
fn edits_preserve_the_slot_identity_and_clamp_drag_values() {
    let (p, track, _) = track_with_device(&Project::new("Macros"));
    let target = AutomationTarget::Volume;
    let p = rename_macro(&p, track, 7, "Dynamics".into()).unwrap();
    let p = set_macro_value(&p, track, 7, 5.0).unwrap();
    let p = add_mapping(&p, track, 7, mapping(target.clone())).unwrap();
    let p = set_mapping_invert(&p, track, 7, &target, true).unwrap();
    let p = set_mapping_bend(&p, track, 7, &target, -2.0).unwrap();
    let p = set_mapping_range(&p, track, 7, &target, 5.0, 0.75).unwrap();
    let m = &slot_at(&p, track, 7).mappings[0];
    assert_eq!((m.min, m.max, m.invert, m.bend), (0.75, 0.75, true, -1.0));
    let p = set_mapping_range(&p, track, 7, &target, 0.75, -5.0).unwrap();
    assert_eq!(slot_at(&p, track, 7).mappings[0].max, 0.75);
    let p = clear_macro(&p, track, 7).unwrap();
    let slot = slot_at(&p, track, 7);
    assert_eq!((slot.name.as_str(), slot.value), ("Dynamics", 1.0));
    assert!(slot.mappings.is_empty());
    assert!(set_macro_value(&p, track, 8, 0.5).is_err());
    assert!(set_macro_value(&p, track, 7, f32::NAN).is_err());
    assert!(set_mapping_bend(&p, track, 7, &target, 0.5).is_err());
}

#[test]
fn moving_to_a_full_slot_leaves_the_first_driver_in_place() {
    let (mut p, track, device) = track_with_device(&Project::new("Macros"));
    let target = AutomationTarget::Pan;
    p = add_mapping(&p, track, 0, mapping(target.clone())).unwrap();
    for id in 0..MAX_MAPPINGS as u32 {
        p = add_mapping(&p, track, 1, mapping(parameter(device, id))).unwrap();
    }
    assert_eq!(
        add_mapping(&p, track, 1, mapping(target.clone())),
        Err(EditError::MacroMappingLimit { index: 1 })
    );
    assert_eq!(p.track(track).unwrap().macros.driver(&target), Some(0));
    let same = add_mapping(&p, track, 1, mapping(parameter(device, 0))).unwrap();
    assert!(Arc::ptr_eq(
        p.track(track).unwrap(),
        same.track(track).unwrap()
    ));
    let p = remove_mapping(&p, track, 1, &parameter(device, 0)).unwrap();
    let mut doc = Document::new(p);
    doc.apply("Move mapping", |p| {
        add_mapping(p, track, 1, mapping(target.clone()))
    })
    .unwrap();
    assert_eq!(
        doc.project().track(track).unwrap().macros.driver(&target),
        Some(1)
    );
    assert!(slot_at(doc.project(), track, 0).mappings.is_empty());
    assert_eq!(
        slot_at(doc.project(), track, 1)
            .mappings
            .last()
            .unwrap()
            .target,
        target
    );
    assert!(doc.undo());
    assert_eq!(
        doc.project().track(track).unwrap().macros.driver(&target),
        Some(0)
    );
}

#[test]
fn only_the_owning_tracks_targets_are_mappable() {
    let (p, track, _) = track_with_device(&Project::new("Macros"));
    let (p, other, device) = track_with_device(&p);
    let (p, bus) = add_bus(&p, NewBus::default()).unwrap();
    let (p, send) = add_send(&p, other, bus).unwrap();
    for target in [
        parameter(device, 0),
        AutomationTarget::Send { send },
        AutomationTarget::Macro { index: 0 },
    ] {
        assert!(add_mapping(&p, track, 0, mapping(target)).is_err());
    }
}

#[test]
fn device_removal_and_instrument_replacement_restore_the_mapping_on_undo() {
    let (p, track, device) = track_with_device(&Project::new("Macros"));
    let target = parameter(device, 42);
    let p = add_mapping(&p, track, 0, mapping(target.clone())).unwrap();
    let mut doc = Document::new(p);
    doc.apply("Replace instrument", |p| {
        replace_instrument(p, track, instrument("Other")).map(|(p, _)| p)
    })
    .unwrap();
    assert!(doc.project().find_device(device).is_none());
    assert!(slot_at(doc.project(), track, 0).mappings.is_empty());
    assert!(doc.undo());
    assert!(doc.project().find_device(device).is_some());
    assert_eq!(slot_at(doc.project(), track, 0).mappings[0].target, target);

    doc.apply("Remove device", |p| remove_device(p, device))
        .unwrap();
    assert!(slot_at(doc.project(), track, 0).mappings.is_empty());
    assert!(doc.undo());
    assert_eq!(slot_at(doc.project(), track, 0).mappings[0].target, target);
}

#[test]
fn copy_remaps_targets_and_bus_removal_cleans_up_their_sends() {
    let (p, track, device) = track_with_device(&Project::new("Macros"));
    let (p, bus) = add_bus(&p, NewBus::default()).unwrap();
    let (p, send) = add_send(&p, track, bus).unwrap();
    let p = add_mapping(&p, track, 0, mapping(parameter(device, 7))).unwrap();
    let p = add_mapping(&p, track, 0, mapping(AutomationTarget::Send { send })).unwrap();
    let (p, copies) = duplicate_tracks(&p, &[track]).unwrap();
    let copy = p.track(copies[0]).unwrap();
    let copy_device = copy.channel.devices[0].id;
    let copy_send = copy.sends[0].id;
    assert_ne!(copy_device, device);
    assert_ne!(copy_send, send);
    let mappings = &slot_at(&p, copy.id, 0).mappings;
    assert_eq!(mappings[0].target, parameter(copy_device, 7));
    assert_eq!(
        mappings[1].target,
        AutomationTarget::Send { send: copy_send }
    );

    let cut = remove_send(&p, copy_send).unwrap();
    assert_eq!(slot_at(&cut, copy.id, 0).mappings.len(), 1);
    assert_eq!(slot_at(&cut, track, 0).mappings.len(), 2);

    let mut doc = Document::new(p.clone());
    doc.apply("Remove bus", |p| remove_tracks(p, &[bus]))
        .unwrap();
    assert_eq!(slot_at(doc.project(), track, 0).mappings.len(), 1);
    assert_eq!(slot_at(doc.project(), copy.id, 0).mappings.len(), 1);
    assert!(doc.undo());
    assert_eq!(doc.project().as_ref(), &p);
}

proptest! {
    #[test]
    fn edit_sequences_preserve_mapping_invariants(
        ops in prop::collection::vec((0..8u8, 0..10u8, 0..30u32, -2.0f32..2.0, -2.0f32..2.0), 0..180),
    ) {
        let (mut p, track, device) = track_with_device(&Project::new("Macros"));
        for (op, slot, id, a, b) in ops {
            let target = parameter(device, id);
            let next = match op {
                0..=1 => add_mapping(&p, track, slot, mapping(target)),
                2 => remove_mapping(&p, track, slot, &target),
                3 => set_mapping_invert(&p, track, slot, &target, a > 0.0),
                4 => set_mapping_range(&p, track, slot, &target, a, b),
                5 => set_mapping_bend(&p, track, slot, &target, a),
                6 => set_macro_value(&p, track, slot, a),
                _ => clear_macro(&p, track, slot),
            };
            if let Ok(next) = next {
                p = next;
            }
            let track = p.track(track).unwrap();
            let mut targets = HashSet::new();
            prop_assert_eq!(track.macros.0.len(), MACRO_COUNT);
            for slot in &track.macros.0 {
                prop_assert!((0.0..=1.0).contains(&slot.value));
                prop_assert!(slot.mappings.len() <= MAX_MAPPINGS);
                for m in &slot.mappings {
                    prop_assert!(track_has_target(track, &m.target));
                    prop_assert!(targets.insert(&m.target));
                    prop_assert!((0.0..=m.max).contains(&m.min));
                    prop_assert!(m.max <= 1.0);
                    prop_assert!((-1.0..=1.0).contains(&m.bend));
                }
            }
        }
    }
}
