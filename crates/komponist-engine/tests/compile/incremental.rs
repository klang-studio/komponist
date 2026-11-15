//! Incremental rebuilds (pointer equality of unchanged track programs), slot assignment and
//! [`compile::slot_changes`], limits, invariants and the compile time of the demo project.

use std::sync::Arc;
use std::time::Instant;

use komponist_engine::compile::{self, NoEnv, SlotChanges};
use komponist_engine::program::{PROBABILITY_FOLLOW, Program, TrackProgram};
use komponist_engine::{MAX_SLOTS, MAX_TRACKS, SlotId};
use komponist_model::edits::{self, ChannelProp, NewDevice, TrackProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId};

use crate::support::{TestEnv, clip, device, midi, project, track};

/// Indices of the tracks of `next` whose program is not the same `Arc` as in `previous`.
fn rebuilt(previous: &Program, next: &Program) -> Vec<usize> {
    next.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            previous
                .track(t.id)
                .is_none_or(|prev| !Arc::ptr_eq(prev, t))
        })
        .map(|(i, _)| i)
        .collect()
}

fn track_id(p: &Project, name: &str) -> TrackId {
    p.tracks.iter().find(|t| t.name == name).unwrap().id
}

fn program_of<'a>(program: &'a Program, p: &Project, name: &str) -> &'a Arc<TrackProgram> {
    program.track(track_id(p, name)).unwrap()
}

#[test]
fn rebuild_of_an_unchanged_project_reuses_every_track() {
    let p = demo_project();
    let first = compile::build(&p, None, &NoEnv);
    let second = compile::build(&p, Some(&first), &NoEnv);
    assert!(rebuilt(&first, &second).is_empty());
    assert_eq!(second.slots, first.slots);
    assert_eq!(
        compile::slot_changes(Some(&first), &second),
        SlotChanges::default()
    );
}

#[test]
fn rebuild_touches_only_the_changed_track() {
    let p0 = demo_project();
    let prog0 = compile::build(&p0, None, &NoEnv);
    let bass = track_id(&p0, "Bass");
    let clip = p0.track(bass).unwrap().clips[1].id;
    let p1 = edits::set_clips_muted(&p0, &[clip], true).unwrap();
    let prog1 = compile::build(&p1, Some(&prog0), &NoEnv);

    let bass_index = p1.track_index(bass).unwrap();
    assert_eq!(rebuilt(&prog0, &prog1), [bass_index]);
    let (old, new) = (
        program_of(&prog0, &p0, "Bass"),
        program_of(&prog1, &p1, "Bass"),
    );
    assert!(Arc::ptr_eq(&new.source, &p1.tracks[bass_index]));
    assert!(Arc::ptr_eq(&old.meter, &new.meter), "the meter survives");
    assert_eq!((old.notes.len(), new.notes.len()), (80, 40));
    // Unchanged tracks are the very same programs, notes and meters included.
    for (i, t) in prog1
        .tracks
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != bass_index)
    {
        assert!(Arc::ptr_eq(t, &prog0.tracks[i]));
    }
    assert_eq!(
        compile::slot_changes(Some(&prog0), &prog1),
        SlotChanges::default()
    );
}

#[test]
fn mixer_edit_rebuilds_the_track_but_keeps_its_notes() {
    let p0 = demo_project();
    let prog0 = compile::build(&p0, None, &NoEnv);
    let lead = track_id(&p0, "Lead");
    let p1 = edits::set_track_prop(
        &p0,
        &[lead],
        TrackProp::Channel(ChannelProp::VolumeDb(-20.0)),
    )
    .unwrap();
    let prog1 = compile::build(&p1, Some(&prog0), &NoEnv);

    assert_eq!(rebuilt(&prog0, &prog1), [p1.track_index(lead).unwrap()]);
    let (old, new) = (
        program_of(&prog0, &p0, "Lead"),
        program_of(&prog1, &p1, "Lead"),
    );
    assert!(Arc::ptr_eq(&old.notes, &new.notes), "clips unchanged");
    assert!(Arc::ptr_eq(&old.meter, &new.meter));
    assert!(
        (new.gain - 0.1).abs() < 1e-6,
        "−20 dB = 0.1, got {}",
        new.gain
    );
}

#[test]
fn latency_change_updates_routing_without_recompiling_notes() {
    let p = demo_project();
    let prog0 = compile::build(&p, None, &NoEnv);
    // The Lead reverb reports 512 frames: every other track waits for it.
    let lead = p.track(track_id(&p, "Lead")).unwrap();
    let reverb = lead.channel.devices[3].id;
    let env = TestEnv {
        latencies: vec![(reverb, 512)],
        ..TestEnv::default()
    };
    let prog1 = compile::build(&p, Some(&prog0), &env);

    let lead_index = p.track_index(lead.id).unwrap();
    let others: Vec<usize> = (0..p.tracks.len()).filter(|&i| i != lead_index).collect();
    assert_eq!(rebuilt(&prog0, &prog1), others, "Lead keeps pdc 0");
    for (old, new) in prog0.tracks.iter().zip(&prog1.tracks) {
        assert!(Arc::ptr_eq(&old.notes, &new.notes));
        assert!(Arc::ptr_eq(&old.meter, &new.meter));
        let expected = if new.id == lead.id { 0 } else { 512 };
        assert_eq!(new.pdc_delay, expected);
    }
}

#[test]
fn input_device_appearing_updates_routing() {
    let mut p = project(vec![track(1, "Keys", vec![])]);
    Arc::make_mut(&mut p.tracks[0]).input =
        komponist_model::MidiInputSel::Device("Keystep".to_owned());
    let prog0 = compile::build(&p, None, &NoEnv);
    assert!(prog0.tracks[0].input.port_missing);
    let env = TestEnv {
        ports: vec![("Keystep".to_owned(), komponist_engine::MidiPort(7))],
        ..TestEnv::default()
    };
    let prog1 = compile::build(&p, Some(&prog0), &env);
    let input = prog1.tracks[0].input;
    assert_eq!(
        (input.port, input.port_missing),
        (Some(komponist_engine::MidiPort(7)), false)
    );
}

#[test]
fn first_build_adds_every_device_in_id_order() {
    let p = demo_project();
    let program = compile::build(&p, None, &NoEnv);
    let changes = compile::slot_changes(None, &program);
    let mut devices: Vec<DeviceId> = p
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter().map(|d| d.id))
        .collect();
    devices.sort_unstable();
    let expected: Vec<(DeviceId, SlotId)> = devices.into_iter().zip((0..).map(SlotId)).collect();
    assert_eq!(changes.added, expected);
    assert!(changes.removed.is_empty());
    assert_eq!(program.slots.entries(), &expected[..]);
}

#[test]
fn slots_are_stable_and_freed_slots_wait_one_build() {
    let effect = |name: &str| NewDevice {
        role: DeviceRole::Effect,
        plugin: device(0, DeviceRole::Effect, name).plugin.clone(),
        state: None,
    };
    let p0 = demo_project();
    let prog0 = compile::build(&p0, None, &NoEnv);
    let used = prog0.slots.entries().len() as u32;

    // Remove the Lead reverb and add a delay to Keys in one sync: the delay gets a fresh slot,
    // not the reverb's, because the reverb's processor is removed only after the program swap.
    let lead = p0.track(track_id(&p0, "Lead")).unwrap();
    let reverb = lead.channel.devices[3].id;
    let reverb_slot = prog0.slots.slot(reverb).unwrap();
    let p1 = edits::remove_device(&p0, reverb).unwrap();
    let keys = ChannelRef::Track(track_id(&p0, "Keys"));
    let (p1, delay) = edits::insert_device(&p1, keys, 1, effect("Delay")).unwrap();
    let prog1 = compile::build(&p1, Some(&prog0), &NoEnv);
    assert_eq!(
        compile::slot_changes(Some(&prog0), &prog1),
        SlotChanges {
            added: vec![(delay, SlotId(used))],
            removed: vec![(reverb, reverb_slot)],
        }
    );
    for &(device, slot) in prog0.slots.entries() {
        if device != reverb {
            assert_eq!(
                prog1.slots.slot(device),
                Some(slot),
                "{device:?} keeps its slot"
            );
        }
    }

    // Reordering a chain keeps the slots too: Lead's chorus moves before its EQ.
    let chorus_before = lead.channel.devices[2].id;
    let p2 = edits::move_device(&p1, chorus_before, 1).unwrap();
    let prog2 = compile::build(&p2, Some(&prog1), &NoEnv);
    assert_eq!(
        compile::slot_changes(Some(&prog1), &prog2),
        SlotChanges::default()
    );
    let lead_chain = &program_of(&prog2, &p2, "Lead").chain;
    let order: Vec<(DeviceId, SlotId)> = lead_chain.iter().map(|s| (s.device, s.slot)).collect();
    let expected: Vec<(DeviceId, SlotId)> = [0, 2, 1]
        .map(|i| lead.channel.devices[i].id)
        .into_iter()
        .map(|d| (d, prog0.slots.slot(d).unwrap()))
        .collect();
    assert_eq!(order, expected);
    assert!(lead_chain[1].bypass, "the chorus stays bypassed");

    // The next new device reuses the freed slot.
    let (p3, chorus) = edits::insert_device(&p2, keys, 1, effect("Chorus")).unwrap();
    let prog3 = compile::build(&p3, Some(&prog2), &NoEnv);
    assert_eq!(
        compile::slot_changes(Some(&prog2), &prog3),
        SlotChanges {
            added: vec![(chorus, reverb_slot)],
            removed: vec![],
        }
    );
    // Only Keys changed.
    assert_eq!(
        rebuilt(&prog2, &prog3),
        [p3.track_index(track_id(&p3, "Keys")).unwrap()]
    );
}

#[test]
fn removed_track_frees_its_slots() {
    let p0 = demo_project();
    let prog0 = compile::build(&p0, None, &NoEnv);
    let arp = track_id(&p0, "Arp");
    let p1 = edits::remove_tracks(&p0, &[arp]).unwrap();
    let prog1 = compile::build(&p1, Some(&prog0), &NoEnv);
    let vital = p0.track(arp).unwrap().channel.devices[0].id;
    assert_eq!(
        compile::slot_changes(Some(&prog0), &prog1).removed,
        [(vital, prog0.slots.slot(vital).unwrap())]
    );
    assert!(rebuilt(&prog0, &prog1).is_empty(), "other tracks stay");
    assert!(prog1.track(arp).is_none());
}

#[test]
fn tracks_beyond_the_limit_are_left_out() {
    let tracks = (0..MAX_TRACKS as u64 + 3)
        .map(|i| {
            let mut t = track(i + 1, &format!("Spur {i}"), vec![]);
            t.channel.devices = vec![device(1000 + i, DeviceRole::Instrument, "hexa")];
            t
        })
        .collect();
    let program = compile::build(&project(tracks), None, &NoEnv);
    assert_eq!(program.tracks.len(), MAX_TRACKS);
    assert_eq!(program.slots.entries().len(), MAX_TRACKS);
}

#[test]
fn devices_beyond_the_slot_table_are_left_out() {
    let mut t = track(1, "Viele", vec![]);
    t.channel.devices = (0..MAX_SLOTS as u64 + 5)
        .map(|i| device(100 + i, DeviceRole::Effect, "Gain"))
        .collect();
    let program = compile::build(&project(vec![t]), None, &NoEnv);
    assert_eq!(program.slots.entries().len(), MAX_SLOTS);
    assert_eq!(program.tracks[0].chain.len(), MAX_SLOTS);
    assert!(
        program.tracks[0]
            .chain
            .iter()
            .all(|s| (s.slot.0 as usize) < MAX_SLOTS)
    );
}

#[test]
fn duplicate_track_ids_are_skipped() {
    let p = project(vec![
        track(
            1,
            "Eins",
            vec![clip(10, 0, 960, midi(&[(0, 240, 60, 100)]))],
        ),
        track(1, "Doppelt", vec![]),
        track(2, "Zwei", vec![]),
    ]);
    let program = compile::build(&p, None, &NoEnv);
    let names: Vec<&str> = program
        .tracks
        .iter()
        .map(|t| t.source.name.as_str())
        .collect();
    assert_eq!(names, ["Eins", "Zwei"]);
}

#[test]
fn demo_notes_are_sorted_and_valid() {
    let program = compile::build(&demo_project(), None, &NoEnv);
    let mut total = 0;
    for t in &program.tracks {
        total += t.notes.len();
        assert!(
            t.notes.is_sorted_by_key(|n| (n.start, n.key)),
            "{}",
            t.source.name
        );
        for n in t.notes.iter() {
            assert!(n.length.0 >= 1 && n.key <= 127 && (1..=127).contains(&n.velocity));
            assert!(n.probability <= 100 || n.probability == PROBABILITY_FOLLOW);
            assert!(n.start.0 >= 0 && n.start + n.length <= program.song_end);
        }
    }
    // Drums 2 × 4 × 14 + 2 × 4 × 21, Bass 80, Lead 68, Pad 24, Keys 40, Arp 128.
    assert_eq!(total, 280 + 80 + 68 + 24 + 40 + 128);
}

/// Prints the compile times of the demo project (run with `--nocapture` to see them). Debug
/// builds are slow, so the only assertion is a generous upper bound.
#[test]
fn demo_compile_time() {
    const RUNS: u32 = 50;
    let p0 = demo_project();
    let started = Instant::now();
    let mut program = compile::build(&p0, None, &NoEnv);
    for _ in 1..RUNS {
        program = compile::build(&p0, None, &NoEnv);
    }
    let full = started.elapsed() / RUNS;

    let bass = track_id(&p0, "Bass");
    let clip = p0.track(bass).unwrap().clips[0].id;
    let p1 = edits::set_clips_muted(&p0, &[clip], true).unwrap();
    let started = Instant::now();
    for _ in 0..RUNS {
        std::hint::black_box(compile::build(&p1, Some(&program), &NoEnv));
    }
    let incremental = started.elapsed() / RUNS;

    eprintln!("demo project compile: full {full:?}, one track changed {incremental:?}");
    assert!(full.as_millis() < 100, "full build took {full:?}");
}

/// WP-88: MIDI effects only at the head of a track chain (at most `MAX_STAGES`), the instrument
/// right behind them; everything else in the wrong place is left out.
#[test]
fn midi_effects_compile_in_front_of_the_instrument_and_misplaced_ones_are_left_out() {
    use komponist_engine::MAX_STAGES;
    let mut t = track(1, "Arp", vec![]);
    t.channel.devices = vec![
        device(10, DeviceRole::MidiEffect, "Scale"),
        device(11, DeviceRole::MidiEffect, "Arp"),
        device(12, DeviceRole::Instrument, "Synth"),
        device(13, DeviceRole::Effect, "EQ"),
        // Wrong places: behind the instrument, a second instrument.
        device(14, DeviceRole::MidiEffect, "Late"),
        device(15, DeviceRole::Instrument, "Second"),
    ];
    let mut master = track(2, "Other", vec![]);
    master.channel.devices = vec![device(20, DeviceRole::MidiEffect, "Stray")];
    let program = compile::build(&project(vec![t, master]), None, &NoEnv);
    let ids =
        |i: usize| -> Vec<u64> { program.tracks[i].chain.iter().map(|s| s.device.0).collect() };
    assert_eq!(ids(0), [10, 11, 12, 13]);
    // A MIDI effect without an instrument in a track chain is valid (the chain is then silent).
    assert_eq!(ids(1), [20]);
    let first = &program.tracks[0];
    assert_eq!(first.stage_count(), 2);
    assert_eq!(first.instrument().map(|s| s.device.0), Some(12));
    assert_eq!(program.tracks[1].instrument(), None);

    // The master never takes one, and the stage count is capped.
    let mut p = project(vec![track(3, "Viele", vec![])]);
    let many: Vec<_> = (0..MAX_STAGES as u64 + 3)
        .map(|i| device(100 + i, DeviceRole::MidiEffect, "Fx"))
        .chain([device(200, DeviceRole::Instrument, "Synth")])
        .collect();
    Arc::make_mut(&mut p.tracks[0]).channel.devices = many;
    Arc::make_mut(&mut p.master).devices = vec![device(300, DeviceRole::MidiEffect, "Stray")];
    let program = compile::build(&p, None, &NoEnv);
    assert_eq!(program.tracks[0].stage_count(), MAX_STAGES);
    assert_eq!(
        program.tracks[0].instrument().map(|s| s.device.0),
        Some(200)
    );
    assert!(program.master.chain.is_empty());
}
