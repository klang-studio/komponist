//! Unit tests of the routing edits and queries (wave 9, ARCHITECTURE §6.8).

use std::sync::Arc;

use super::tests::check_invariants;
use super::*;
use crate::document::Document;
use crate::project::{SEND_LEVEL_DEFAULT_DB, Sidechain, Tap, TrackSend};
use crate::routing::{self, Gate};

// ------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------

#[track_caller]
fn ok(result: EditResult) -> Project {
    let p = result.expect("edit succeeds");
    check_invariants(&p);
    p
}

#[track_caller]
fn ok_with<T>(result: Result<(Project, T), EditError>) -> (Project, T) {
    let (p, value) = result.expect("edit succeeds");
    check_invariants(&p);
    (p, value)
}

fn plugin(name: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: format!("test.{name}"),
        name: name.to_owned(),
        vendor: "Test".to_owned(),
        version: "1.0".to_owned(),
        module: Box::default(),
    }
}

fn effect(name: &str) -> NewDevice {
    NewDevice {
        role: DeviceRole::Effect,
        plugin: plugin(name),
        state: None,
    }
}

/// A project with MIDI tracks named `names` (each with an instrument), in this order.
fn tracks(names: &[&str]) -> (Project, Vec<TrackId>) {
    let mut p = Project::new("Routing");
    let mut ids = Vec::new();
    for name in names {
        let spec = NewTrack {
            name: Some((*name).to_owned()),
            hue: None,
            instrument: Some(plugin(name)),
            index: None,
            arm_exclusive: false,
        };
        let (next, id) = ok_with(add_track(&p, spec));
        p = next;
        ids.push(id);
    }
    (p, ids)
}

fn names(p: &Project) -> Vec<&str> {
    p.tracks.iter().map(|t| t.name.as_str()).collect()
}

fn track(p: &Project, id: TrackId) -> &Track {
    p.track(id).expect("track exists")
}

fn bus_out(id: TrackId) -> TrackOutput {
    TrackOutput::Bus(id)
}

fn effect_bus(p: &Project, senders: &[TrackId]) -> (Project, TrackId) {
    ok_with(add_bus(p, NewBus::effect(senders.to_vec())))
}

fn group(p: &Project, members: &[TrackId]) -> (Project, TrackId) {
    ok_with(add_bus(p, NewBus::group(members.to_vec())))
}

fn device_on(p: &Project, track: TrackId) -> DeviceId {
    p.track(track).unwrap().channel.devices[0].id
}

// ------------------------------------------------------------------------------------------------
// Creating buses
// ------------------------------------------------------------------------------------------------

#[test]
fn effect_bus_gets_sends_with_the_defaults() {
    let (p, t) = tracks(&["Lead", "Pad", "Bass"]);
    let (p, hall) = effect_bus(&p, &[t[0], t[1]]);
    let bus = track(&p, hall);
    assert_eq!(bus.name, "Bus 1");
    assert_eq!(bus.kind, TrackKind::Bus);
    assert!(!bus.folder && !bus.folded && !bus.arm);
    assert_eq!(bus.output, TrackOutput::Master);
    assert!(bus.clips.is_empty() && bus.channel.devices.is_empty() && bus.sends.is_empty());
    assert_eq!(names(&p), ["Lead", "Pad", "Bass", "Bus 1"], "at the end");
    assert!(Hue::ALL.contains(&bus.hue));
    assert!(
        p.tracks[..3].iter().all(|t| t.hue != bus.hue),
        "a palette hue no track uses"
    );
    for &sender in &t[..2] {
        let sends = &track(&p, sender).sends;
        assert_eq!(sends.len(), 1);
        let s = &sends[0];
        assert_eq!(
            (s.target, s.level_db, s.tap, s.enabled),
            (hall, SEND_LEVEL_DEFAULT_DB, Tap::Post, true)
        );
        assert_eq!(track(&p, sender).output, TrackOutput::Master);
    }
    assert!(track(&p, t[2]).sends.is_empty());
    let (p, second) = effect_bus(&p, &[]);
    assert_eq!(track(&p, second).name, "Bus 2");
    // A bus cannot send (owner decision 11 A).
    assert_eq!(
        add_bus(&p, NewBus::effect(vec![hall])),
        Err(EditError::Invalid("a bus has no sends"))
    );
    assert_eq!(
        add_bus(&p, NewBus::effect(vec![TrackId(999)])),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
}

#[test]
fn group_takes_the_place_of_the_topmost_member() {
    let (p, t) = tracks(&["Kick", "Bass", "Snare", "Lead"]);
    let (p, rhythm) = group(&p, &[t[2], t[0]]);
    assert_eq!(names(&p), ["Gruppe 1", "Kick", "Snare", "Bass", "Lead"]);
    let g = track(&p, rhythm);
    assert!(g.folder && g.is_group());
    assert_eq!(g.output, TrackOutput::Master);
    assert_eq!(routing::group_members(&p, rhythm), [t[0], t[2]]);
    assert_eq!(routing::group_of(&p, t[0]), Some(rhythm));
    assert_eq!(routing::group_of(&p, t[1]), None);
    // A group made inside a group nests and outputs to the outer group.
    let (p, inner) = group(&p, &[t[2]]);
    assert_eq!(
        names(&p),
        ["Gruppe 1", "Kick", "Gruppe 2", "Snare", "Bass", "Lead"]
    );
    assert_eq!(track(&p, inner).output, bus_out(rhythm));
    assert_eq!(routing::group_ancestors(&p, t[2]), [inner, rhythm]);
    // Grouping a group with a track outside keeps the inner members where they are.
    let (p, outer) = group(&p, &[rhythm, t[2], t[3]]);
    assert_eq!(track(&p, rhythm).output, bus_out(outer));
    assert_eq!(track(&p, t[3]).output, bus_out(outer));
    assert_eq!(
        track(&p, t[2]).output,
        bus_out(inner),
        "inside a listed group"
    );
    assert_eq!(
        names(&p),
        [
            "Gruppe 3", "Gruppe 1", "Kick", "Gruppe 2", "Snare", "Lead", "Bass"
        ]
    );
    // An empty group (menu „Neue Gruppe“) goes to the index; before a member it joins that group.
    let spec = NewBus {
        group: true,
        index: Some(2),
        ..NewBus::default()
    };
    let (p, empty) = ok_with(add_bus(&p, spec));
    assert_eq!(track(&p, empty).output, bus_out(rhythm));
    assert_eq!(track(&p, empty).name, "Gruppe 4");
}

#[test]
fn bus_rejects_clips_instruments_and_arm() {
    let (p, t) = tracks(&["Lead"]);
    let (p, b) = effect_bus(&p, &[]);
    let clip = NewClip {
        start: Tick::ZERO,
        length: Tick(3840),
        name: "Clip".to_owned(),
        content: NewClipContent::Step { rows: Vec::new() },
    };
    assert_eq!(
        add_clip(&p, b, clip.clone()),
        Err(EditError::Invalid("a bus holds no clips"))
    );
    let (p, c) = ok_with(add_clip(&p, t[0], clip));
    assert_eq!(
        move_clips(&p, &[c], Tick::ZERO, 1),
        Err(EditError::Invalid("a bus holds no clips"))
    );
    assert!(copy_clips(&p, &[c], Tick::ZERO, 1).is_err());
    let instrument = NewDevice {
        role: DeviceRole::Instrument,
        ..effect("Synth")
    };
    assert!(insert_device(&p, ChannelRef::Track(b), 0, instrument).is_err());
    let midi_fx = NewDevice {
        role: DeviceRole::MidiEffect,
        ..effect("Arp")
    };
    assert!(insert_device(&p, ChannelRef::Track(b), 0, midi_fx).is_err());
    assert!(replace_instrument(&p, b, plugin("Synth")).is_err());
    let (p, _) = ok_with(insert_device(&p, ChannelRef::Track(b), 0, effect("Hall")));
    // Arm and monitoring are ignored on a bus, also inside a drag across tracks.
    let p = ok(set_track_prop(&p, &[t[0], b], TrackProp::Arm(true)));
    assert!(track(&p, t[0]).arm && !track(&p, b).arm && !track(&p, b).monitor);
    let p = ok(set_exclusive(&p, b, ExclusiveFlag::Arm));
    assert!(!track(&p, t[0]).arm && !track(&p, b).arm);
    // Only a group folds.
    let p = ok(set_track_prop(&p, &[t[0], b], TrackProp::Folded(true)));
    assert!(!track(&p, t[0]).folded && !track(&p, b).folded);
    let (p, g) = group(&p, &[t[0]]);
    let p = ok(set_track_prop(&p, &[g], TrackProp::Folded(true)));
    assert!(track(&p, g).folded);
}

// ------------------------------------------------------------------------------------------------
// Outputs
// ------------------------------------------------------------------------------------------------

#[test]
fn outputs_go_to_buses_without_cycles() {
    let (p, t) = tracks(&["Lead", "Pad"]);
    let (p, b1) = effect_bus(&p, &[]);
    let (p, b2) = effect_bus(&p, &[]);
    let p = ok(set_output(&p, &[t[0], t[1]], bus_out(b1)));
    let p = ok(set_output(&p, &[b1], bus_out(b2)));
    assert_eq!(
        set_output(&p, &[b2], bus_out(b1)),
        Err(EditError::RoutingCycle)
    );
    assert_eq!(
        set_output(&p, &[b1], bus_out(b1)),
        Err(EditError::RoutingCycle)
    );
    assert_eq!(
        set_output(&p, &[t[0]], bus_out(t[1])),
        Err(EditError::NotABus(t[1]))
    );
    assert_eq!(
        set_output(&p, &[t[0]], bus_out(TrackId(999))),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
    assert_eq!(
        routing::output_targets(&p, b2),
        [TrackOutput::Master],
        "b1 feeds b2"
    );
    assert_eq!(
        routing::output_targets(&p, t[0]),
        [TrackOutput::Master, bus_out(b1), bus_out(b2)]
    );
    assert_eq!(
        routing::output_targets(&p, b1),
        [TrackOutput::Master, bus_out(b2)]
    );
    assert!(routing::output_targets(&p, TrackId(999)).is_empty());
    let back = ok(set_output(&p, &[b1], TrackOutput::Master));
    assert_eq!(track(&back, b1).output, TrackOutput::Master);
}

#[test]
fn output_to_a_group_moves_the_track_into_its_block() {
    let (p, t) = tracks(&["Kick", "Snare", "Bass", "Lead"]);
    let (p, g) = group(&p, &[t[0], t[1]]);
    assert_eq!(names(&p), ["Gruppe 1", "Kick", "Snare", "Bass", "Lead"]);
    let p = ok(set_output(&p, &[t[3]], bus_out(g)));
    assert_eq!(names(&p), ["Gruppe 1", "Kick", "Snare", "Lead", "Bass"]);
    let p = ok(set_output(&p, &[t[0]], TrackOutput::Master));
    assert_eq!(
        names(&p),
        ["Gruppe 1", "Snare", "Lead", "Kick", "Bass"],
        "behind the block"
    );
    // Moving keeps the blocks: the group takes its members along, a member stays inside.
    let p = ok(move_tracks(&p, &[g], 5));
    assert_eq!(names(&p), ["Kick", "Bass", "Gruppe 1", "Snare", "Lead"]);
    let p = ok(move_tracks(&p, &[t[3]], 0));
    assert_eq!(names(&p), ["Kick", "Bass", "Gruppe 1", "Lead", "Snare"]);
    // A new track inserted before a member joins the group; at the block's end it does not.
    let spec = |index| NewTrack {
        name: Some("Neu".to_owned()),
        hue: None,
        instrument: None,
        index: Some(index),
        arm_exclusive: false,
    };
    let (q, inside) = ok_with(add_track(&p, spec(4)));
    assert_eq!(track(&q, inside).output, bus_out(g));
    let (q, outside) = ok_with(add_track(&p, spec(5)));
    assert_eq!(track(&q, outside).output, TrackOutput::Master);
}

// ------------------------------------------------------------------------------------------------
// Sends
// ------------------------------------------------------------------------------------------------

#[test]
fn sends_are_added_changed_and_removed() {
    let (p, t) = tracks(&["Lead", "Pad"]);
    let (p, hall) = effect_bus(&p, &[]);
    let (p, delay) = effect_bus(&p, &[]);
    let (p, s) = ok_with(add_send(&p, t[0], hall));
    assert_eq!(
        track(&p, t[0]).sends,
        [TrackSend {
            id: s,
            target: hall,
            level_db: -6.0,
            tap: Tap::Post,
            enabled: true,
        }]
    );
    assert_eq!(
        add_send(&p, t[0], hall),
        Err(EditError::Invalid("the track already sends to this bus"))
    );
    assert_eq!(
        add_send(&p, hall, delay),
        Err(EditError::Invalid("a bus has no sends"))
    );
    assert_eq!(add_send(&p, t[0], t[1]), Err(EditError::NotABus(t[1])));
    assert_eq!(routing::send_targets(&p, t[0]), [delay]);
    assert_eq!(routing::send_targets(&p, t[1]), [hall, delay]);

    let p = ok(set_send(&p, s, SendProp::LevelDb(f32::NEG_INFINITY)));
    assert_eq!(track(&p, t[0]).sends[0].level_db, VOLUME_DB_MIN);
    let p = ok(set_send(&p, s, SendProp::LevelDb(40.0)));
    assert_eq!(track(&p, t[0]).sends[0].level_db, VOLUME_DB_MAX);
    assert!(set_send(&p, s, SendProp::LevelDb(f32::NAN)).is_err());
    let p = ok(set_send(&p, s, SendProp::Tap(Tap::Pre)));
    let p = ok(set_send(&p, s, SendProp::Enabled(false)));
    let p = ok(set_send(&p, s, SendProp::Target(delay)));
    let send = &track(&p, t[0]).sends[0];
    assert_eq!(
        (send.id, send.target, send.tap, send.enabled),
        (s, delay, Tap::Pre, false)
    );
    let (p, s2) = ok_with(add_send(&p, t[0], hall));
    assert!(set_send(&p, s2, SendProp::Target(delay)).is_err());
    let p = ok(remove_send(&p, s));
    assert_eq!(track(&p, t[0]).sends.len(), 1);
    assert_eq!(remove_send(&p, s), Err(EditError::SendNotFound(s)));
    assert_eq!(
        set_send(&p, s, SendProp::Enabled(true)),
        Err(EditError::SendNotFound(s))
    );
}

// ------------------------------------------------------------------------------------------------
// Sidechains
// ------------------------------------------------------------------------------------------------

#[test]
fn sidechains_take_any_source_that_closes_no_cycle() {
    let (p, t) = tracks(&["Kick", "Bass", "Pad"]);
    let (p, b) = effect_bus(&p, &[t[1]]);
    let (p, comp) = ok_with(insert_device(
        &p,
        ChannelRef::Track(t[1]),
        1,
        effect("presse"),
    ));
    let kick = Sidechain::new(t[0]);
    assert_eq!(kick.tap, Tap::Pre, "owner decision 10 A");
    let p = ok(set_sidechain(&p, comp, Some(kick)));
    assert_eq!(p.find_device(comp).unwrap().2.sidechain, Some(kick));
    // Own track; the bus the track sends to (bus → bass → bus).
    assert_eq!(
        set_sidechain(&p, comp, Some(Sidechain::new(t[1]))),
        Err(EditError::RoutingCycle)
    );
    assert_eq!(
        set_sidechain(&p, comp, Some(Sidechain::new(b))),
        Err(EditError::RoutingCycle)
    );
    assert_eq!(routing::sidechain_sources(&p, comp), [t[0], t[2]]);
    // The sidechain orders the graph: Kick can no longer get a send from... Bass feeding Kick.
    let (p, kick_comp) = ok_with(insert_device(
        &p,
        ChannelRef::Track(t[0]),
        1,
        effect("presse"),
    ));
    assert_eq!(
        set_sidechain(&p, kick_comp, Some(Sidechain::new(t[1]))),
        Err(EditError::RoutingCycle)
    );
    // A device on the master takes every track and bus.
    let (p, limiter) = ok_with(insert_device(&p, ChannelRef::Master, 0, effect("Limiter")));
    assert_eq!(
        routing::sidechain_sources(&p, limiter),
        [t[0], t[1], t[2], b]
    );
    let p = ok(set_sidechain(
        &p,
        limiter,
        Some(Sidechain {
            source: b,
            tap: Tap::Post,
        }),
    ));
    let p = ok(set_sidechain(&p, comp, None));
    assert_eq!(p.find_device(comp).unwrap().2.sidechain, None);
    assert_eq!(
        set_sidechain(&p, comp, Some(Sidechain::new(TrackId(999)))),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
    assert_eq!(
        set_sidechain(&p, DeviceId(999), None),
        Err(EditError::DeviceNotFound(DeviceId(999)))
    );
    // A send that would close a cycle through a sidechain is rejected too.
    let (p, b2) = effect_bus(&p, &[]);
    let (p, b2_comp) = ok_with(insert_device(
        &p,
        ChannelRef::Track(b2),
        0,
        effect("presse"),
    ));
    let p = ok(set_sidechain(&p, kick_comp, Some(Sidechain::new(b2))));
    assert!(set_sidechain(&p, b2_comp, Some(Sidechain::new(t[0]))).is_err());
    assert_eq!(add_send(&p, t[0], b2), Err(EditError::RoutingCycle));
}

// ------------------------------------------------------------------------------------------------
// Deleting and duplicating
// ------------------------------------------------------------------------------------------------

#[test]
fn deleting_a_bus_repairs_every_reference_in_one_undo_step() {
    let (p, t) = tracks(&["Kick", "Bass", "Pad", "Lead"]);
    let (p, b1) = effect_bus(&p, &[t[0], t[2]]);
    let (p, b2) = effect_bus(&p, &[]);
    let p = ok(set_output(&p, &[t[1]], bus_out(b1)));
    let p = ok(set_output(&p, &[b1], bus_out(b2)));
    let (p, comp) = ok_with(insert_device(
        &p,
        ChannelRef::Track(t[3]),
        1,
        effect("presse"),
    ));
    let p = ok(set_sidechain(&p, comp, Some(Sidechain::new(b1))));
    let (p, limiter) = ok_with(insert_device(&p, ChannelRef::Master, 0, effect("Limiter")));
    let p = ok(set_sidechain(&p, limiter, Some(Sidechain::new(b1))));

    let mut doc = Document::new(p.clone());
    doc.apply("Bus löschen", |p| remove_tracks(p, &[b1]))
        .unwrap();
    let q = Arc::clone(doc.project());
    check_invariants(&q);
    assert!(q.track(b1).is_none());
    assert_eq!(track(&q, t[1]).output, bus_out(b2), "where the bus went");
    assert!(track(&q, t[0]).sends.is_empty() && track(&q, t[2]).sends.is_empty());
    assert_eq!(q.find_device(comp).unwrap().2.sidechain, None);
    assert_eq!(q.find_device(limiter).unwrap().2.sidechain, None);
    assert!(routing::edges(&q).iter().all(|e| match e.from {
        ChannelRef::Track(id) => id != b1,
        ChannelRef::Master => false,
    } && e.to != ChannelRef::Track(b1)));
    assert!(doc.undo());
    assert_eq!(**doc.project(), p);
    // Deleting a chain of buses: outputs follow to the first bus that stays.
    let q = ok(remove_tracks(&p, &[b1, b2]));
    assert_eq!(track(&q, t[1]).output, TrackOutput::Master);
}

#[test]
fn deleting_a_group_dissolves_it() {
    let (p, t) = tracks(&["Kick", "Snare", "Bass"]);
    let (p, outer) = group(&p, &[t[0], t[1], t[2]]);
    let (p, inner) = group(&p, &[t[0], t[1]]);
    assert_eq!(names(&p), ["Gruppe 1", "Gruppe 2", "Kick", "Snare", "Bass"]);
    let q = ok(remove_tracks(&p, &[inner]));
    assert_eq!(names(&q), ["Gruppe 1", "Kick", "Snare", "Bass"]);
    assert_eq!(routing::group_members(&q, outer), [t[0], t[1], t[2]]);
    let q = ok(remove_tracks(&p, &[outer]));
    assert_eq!(names(&q), ["Gruppe 2", "Kick", "Snare", "Bass"]);
    assert_eq!(track(&q, inner).output, TrackOutput::Master);
    assert_eq!(track(&q, t[2]).output, TrackOutput::Master);
    // „Mit Spuren löschen“: the UI lists the members too.
    let q = ok(remove_tracks(&p, &[outer, inner, t[0], t[1], t[2]]));
    assert!(q.tracks.is_empty());
}

#[test]
fn removing_a_track_keeps_untouched_tracks_shared() {
    let (p, t) = tracks(&["Kick", "Bass", "Pad"]);
    let (p, b) = effect_bus(&p, &[t[1]]);
    let q = ok(remove_tracks(&p, &[t[0]]));
    for id in [t[1], t[2], b] {
        let before = &p.tracks[p.track_index(id).unwrap()];
        let after = &q.tracks[q.track_index(id).unwrap()];
        assert!(Arc::ptr_eq(before, after), "{id:?}");
    }
    // Deleting a sidechain source clears the sidechain only.
    let comp = device_on(&p, t[1]);
    let p = ok(set_sidechain(&p, comp, Some(Sidechain::new(t[0]))));
    let q = ok(remove_tracks(&p, &[t[0]]));
    assert_eq!(q.find_device(comp).unwrap().2.sidechain, None);
    assert_eq!(track(&q, t[1]).sends.len(), 1);
}

#[test]
fn duplicate_keeps_the_routing_and_maps_it_inside_the_copy() {
    let (p, t) = tracks(&["Kick", "Bass", "Lead"]);
    let (p, hall) = effect_bus(&p, &[t[1]]);
    let (p, g) = group(&p, &[t[0], t[1]]);
    let bass_dev = device_on(&p, t[1]);
    let p = ok(set_sidechain(&p, bass_dev, Some(Sidechain::new(t[0]))));
    let p = ok(set_track_prop(&p, &[t[1]], TrackProp::Arm(true)));
    // One track: same output, same send target (new send id), same sidechain source.
    let (q, copies) = ok_with(duplicate_tracks(&p, &[t[1]]));
    let copy = track(&q, copies[0]);
    assert_eq!(copy.name, "Bass 2");
    assert!(!copy.arm);
    assert_eq!(copy.output, bus_out(g));
    assert_eq!(copy.sends.len(), 1);
    assert_eq!(copy.sends[0].target, hall);
    assert_ne!(copy.sends[0].id, track(&p, t[1]).sends[0].id);
    assert_ne!(copy.channel.devices[0].id, bass_dev);
    assert_eq!(
        copy.channel.devices[0].sidechain,
        Some(Sidechain::new(t[0]))
    );
    assert_eq!(
        names(&q),
        ["Gruppe 1", "Kick", "Bass", "Bass 2", "Lead", "Bus 1"]
    );
    // A group with its members: the copies form a copy of the group, the sidechain follows.
    let (q, copies) = ok_with(duplicate_tracks(&p, &[g, t[0], t[1]]));
    let [g2, kick2, bass2] = copies[..] else {
        panic!("three copies")
    };
    assert_eq!(
        names(&q),
        [
            "Gruppe 1",
            "Kick",
            "Bass",
            "Gruppe 1 2",
            "Kick 2",
            "Bass 2",
            "Lead",
            "Bus 1"
        ]
    );
    assert_eq!(routing::group_members(&q, g2), [kick2, bass2]);
    assert_eq!(routing::group_members(&q, g), [t[0], t[1]]);
    assert_eq!(
        track(&q, bass2).channel.devices[0].sidechain,
        Some(Sidechain::new(kick2))
    );
    assert_eq!(track(&q, bass2).sends[0].target, hall);
    assert_eq!(
        duplicate_tracks(&p, &[TrackId(999)]),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
}

// ------------------------------------------------------------------------------------------------
// Queries
// ------------------------------------------------------------------------------------------------

#[test]
fn topological_order_follows_every_edge() {
    let (p, t) = tracks(&["Kick", "Bass", "Pad"]);
    assert_eq!(
        routing::topo_order(&p).unwrap(),
        [
            ChannelRef::Track(t[0]),
            ChannelRef::Track(t[1]),
            ChannelRef::Track(t[2]),
            ChannelRef::Master
        ],
        "display order without buses"
    );
    let (p, b) = effect_bus(&p, &[t[2]]);
    let p = ok(move_tracks(&p, &[b], 0));
    // Kick's compressor listens to Pad: Pad first.
    let (p, comp) = ok_with(insert_device(
        &p,
        ChannelRef::Track(t[0]),
        1,
        effect("presse"),
    ));
    let p = ok(set_sidechain(&p, comp, Some(Sidechain::new(t[2]))));
    let order = routing::topo_order(&p).unwrap();
    let pos = |n: ChannelRef| order.iter().position(|o| *o == n).unwrap();
    for edge in routing::edges(&p) {
        assert!(pos(edge.from) < pos(edge.to), "{edge:?}");
    }
    assert_eq!(
        order,
        [
            ChannelRef::Track(t[1]),
            ChannelRef::Track(t[2]),
            ChannelRef::Track(b),
            ChannelRef::Track(t[0]),
            ChannelRef::Master
        ]
    );
    assert!(routing::reaches(
        &p,
        ChannelRef::Track(t[2]),
        ChannelRef::Track(t[0])
    ));
    assert!(routing::would_create_cycle(
        &p,
        ChannelRef::Track(t[0]),
        ChannelRef::Track(t[2])
    ));
    assert!(!routing::would_create_cycle(
        &p,
        ChannelRef::Track(t[1]),
        ChannelRef::Track(b)
    ));
}

#[test]
fn solo_follows_the_signal_path() {
    // Kick, Snare → group Drums → master; Lead sends to Hall; Bass → master.
    let (p, t) = tracks(&["Kick", "Snare", "Lead", "Bass"]);
    let [kick, snare, lead, bass] = t[..] else {
        unreachable!()
    };
    let (p, drums) = group(&p, &[kick, snare]);
    let (p, hall) = effect_bus(&p, &[lead, kick]);
    let send = |p: &Project, from| track(p, from).sends[0].id;
    let open = |p: &Project| -> Vec<(String, bool)> {
        let g = routing::gates(p);
        let mut out: Vec<(String, bool)> = p
            .tracks
            .iter()
            .map(|t| (t.name.clone(), g.output_open(t.id)))
            .collect();
        out.push(("Lead→Hall".to_owned(), g.send_open(send(p, lead))));
        out.push(("Kick→Hall".to_owned(), g.send_open(send(p, kick))));
        out
    };
    let named = |v: Vec<(&str, bool)>| -> Vec<(String, bool)> {
        v.into_iter().map(|(n, o)| (n.to_owned(), o)).collect()
    };
    let all = |on: bool| {
        vec![
            ("Gruppe 1", on),
            ("Kick", on),
            ("Snare", on),
            ("Lead", on),
            ("Bass", on),
            ("Bus 1", on),
            ("Lead→Hall", on),
            ("Kick→Hall", on),
        ]
    };
    assert_eq!(open(&p), named(all(true)));
    let solo = |p: &Project, id| {
        ok(set_track_prop(
            p,
            &[id],
            TrackProp::Channel(ChannelProp::Solo(true)),
        ))
    };
    // Solo Lead: Lead and its send stay, Hall stays (Lead feeds it), the rest is silent.
    let q = solo(&p, lead);
    assert_eq!(
        open(&q),
        named(vec![
            ("Gruppe 1", false),
            ("Kick", false),
            ("Snare", false),
            ("Lead", true),
            ("Bass", false),
            ("Bus 1", true),
            ("Lead→Hall", true),
            ("Kick→Hall", false),
        ])
    );
    let g = routing::gates(&q);
    assert_eq!(
        g.output(bass),
        Gate {
            muted: false,
            solo_muted: true
        }
    );
    // Solo the group: its members and their sends stay, Lead does not.
    let q = solo(&p, drums);
    assert_eq!(
        open(&q),
        named(vec![
            ("Gruppe 1", true),
            ("Kick", true),
            ("Snare", true),
            ("Lead", false),
            ("Bass", false),
            ("Bus 1", true),
            ("Lead→Hall", false),
            ("Kick→Hall", true),
        ])
    );
    // Solo the effect bus: only the wet signal (sends into it stay, outputs of the senders not).
    let q = solo(&p, hall);
    assert_eq!(
        open(&q),
        named(vec![
            ("Gruppe 1", false),
            ("Kick", false),
            ("Snare", false),
            ("Lead", false),
            ("Bass", false),
            ("Bus 1", true),
            ("Lead→Hall", true),
            ("Kick→Hall", true),
        ])
    );
    // Mute closes the output and every send; a muted bus keeps its inputs.
    let q = ok(set_track_prop(
        &p,
        &[kick, hall],
        TrackProp::Channel(ChannelProp::Mute(true)),
    ));
    let g = routing::gates(&q);
    assert!(!g.output_open(kick) && !g.send_open(send(&q, kick)));
    assert!(!g.output_open(hall) && g.send_open(send(&q, lead)));
    assert!(g.output(kick).muted && !g.output(kick).solo_muted);
    // A disabled send is closed.
    let q = ok(set_send(&p, send(&p, lead), SendProp::Enabled(false)));
    assert!(!routing::gates(&q).send_open(send(&q, lead)));
}

#[test]
fn solo_without_buses_is_the_old_rule() {
    let (p, t) = tracks(&["A", "B", "C"]);
    let p = ok(set_track_prop(
        &p,
        &[t[0], t[2]],
        TrackProp::Channel(ChannelProp::Solo(true)),
    ));
    let p = ok(set_track_prop(
        &p,
        &[t[2]],
        TrackProp::Channel(ChannelProp::Mute(true)),
    ));
    let g = routing::gates(&p);
    for track in &p.tracks {
        let c = &track.channel;
        assert_eq!(
            g.output_open(track.id),
            !c.mute && (!p.any_solo() || c.solo),
            "{}",
            track.name
        );
    }
}
