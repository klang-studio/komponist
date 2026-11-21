//! Compile diagnostics (#92): every track and device the program leaves out is reported once, with
//! its position and the reason, in project order; everything else plays.

use std::collections::BTreeSet;
use std::sync::Arc;

use komponist_engine::compile::{self, Chain, CompileResult, Diagnostic, NoEnv, Reason, Subject};
use komponist_engine::{MAX_SLOTS, MAX_STAGES, MAX_TRACKS};
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceId, DeviceRole, Project, TrackId, TrackKind};

use crate::support::{device, project, track};

fn compiled(p: &Project) -> CompileResult {
    compile::build_with_diagnostics(p, None, &NoEnv)
}

fn track_diag(id: u64, index: usize, reason: Reason) -> Diagnostic {
    Diagnostic {
        subject: Subject::Track {
            track: TrackId(id),
            index,
        },
        reason,
    }
}

fn device_diag(
    chain: Chain,
    id: u64,
    index: usize,
    role: DeviceRole,
    reason: Reason,
) -> Diagnostic {
    Diagnostic {
        subject: Subject::Device {
            chain,
            device: DeviceId(id),
            index,
            role,
        },
        reason,
    }
}

fn on_track(id: u64, index: usize) -> Chain {
    Chain::Track {
        track: TrackId(id),
        index,
    }
}

/// Every device id of the project's chains (tracks first, then the master).
fn project_devices(p: &Project) -> Vec<DeviceId> {
    p.tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .chain(&p.master.devices)
        .map(|d| d.id)
        .collect()
}

#[test]
fn the_demo_project_compiles_without_diagnostics() {
    let result = compiled(&demo_project());
    assert_eq!(result.diagnostics, []);
}

#[test]
fn build_returns_the_same_program_as_build_with_diagnostics() {
    let p = demo_project();
    let plain = compile::build(&p, None, &NoEnv);
    let full = compiled(&p).program;
    assert_eq!(plain.tracks.len(), full.tracks.len());
    assert_eq!(plain.slots.entries(), full.slots.entries());
    assert_eq!(plain.master.chain, full.master.chain);
}

#[test]
fn tracks_beyond_the_limit_are_reported_but_not_their_devices() {
    let tracks = (0..MAX_TRACKS as u64 + 2)
        .map(|i| {
            let mut t = track(i + 1, &format!("Spur {i}"), vec![]);
            t.channel.devices = vec![device(1000 + i, DeviceRole::Instrument, "Hexa")];
            t
        })
        .collect();
    let result = compiled(&project(tracks));
    assert_eq!(result.program.tracks.len(), MAX_TRACKS);
    let n = MAX_TRACKS as u64;
    assert_eq!(
        result.diagnostics,
        [
            track_diag(n + 1, MAX_TRACKS, Reason::TrackLimit),
            track_diag(n + 2, MAX_TRACKS + 1, Reason::TrackLimit),
        ]
    );
    assert!(result.diagnostics.iter().all(|d| d.reason.is_limit()));
}

#[test]
fn a_duplicate_track_is_reported_at_its_own_index() {
    let p = project(vec![
        track(1, "Eins", vec![]),
        track(1, "Doppelt", vec![]),
        track(2, "Zwei", vec![]),
    ]);
    let result = compiled(&p);
    assert_eq!(result.program.tracks.len(), 2);
    assert_eq!(result.diagnostics, [track_diag(1, 1, Reason::DuplicateId)]);
    assert!(!Reason::DuplicateId.is_limit());
}

#[test]
fn duplicate_devices_are_reported_where_they_repeat() {
    let mut a = track(1, "A", vec![]);
    a.channel.devices = vec![
        device(10, DeviceRole::Instrument, "Synth"),
        device(11, DeviceRole::Effect, "EQ"),
        device(11, DeviceRole::Effect, "EQ"),
    ];
    let mut b = track(2, "B", vec![]);
    b.channel.devices = vec![device(10, DeviceRole::Effect, "Gain")];
    let mut p = project(vec![a, b]);
    Arc::make_mut(&mut p.master).devices = vec![device(11, DeviceRole::Effect, "Comp")];
    let result = compiled(&p);
    assert_eq!(
        result.diagnostics,
        [
            device_diag(
                on_track(1, 0),
                11,
                2,
                DeviceRole::Effect,
                Reason::DuplicateId
            ),
            device_diag(
                on_track(2, 1),
                10,
                0,
                DeviceRole::Effect,
                Reason::DuplicateId
            ),
            device_diag(
                Chain::Master,
                11,
                0,
                DeviceRole::Effect,
                Reason::DuplicateId
            ),
        ]
    );
    assert!(result.program.tracks[1].chain.is_empty());
    assert!(result.program.master.chain.is_empty());
}

#[test]
fn misplaced_devices_and_devices_on_the_wrong_chain_are_reported() {
    let mut t = track(1, "Arp", vec![]);
    t.channel.devices = vec![
        device(10, DeviceRole::MidiEffect, "Scale"),
        device(12, DeviceRole::Instrument, "Synth"),
        device(13, DeviceRole::Effect, "EQ"),
        device(14, DeviceRole::MidiEffect, "Late"),
        device(15, DeviceRole::Instrument, "Second"),
    ];
    let mut bus = track(2, "Bus", vec![]);
    bus.kind = TrackKind::Bus;
    bus.channel.devices = vec![
        device(20, DeviceRole::Instrument, "Synth"),
        device(21, DeviceRole::Effect, "Reverb"),
    ];
    let mut audio = track(3, "Audio", vec![]);
    audio.kind = TrackKind::Audio;
    audio.channel.devices = vec![device(30, DeviceRole::MidiEffect, "Arp")];
    let mut p = project(vec![t, bus, audio]);
    Arc::make_mut(&mut p.master).devices = vec![
        device(40, DeviceRole::Effect, "Limiter"),
        device(41, DeviceRole::Instrument, "Synth"),
    ];
    let result = compiled(&p);
    use DeviceRole::{Instrument, MidiEffect};
    assert_eq!(
        result.diagnostics,
        [
            device_diag(on_track(1, 0), 14, 3, MidiEffect, Reason::Misplaced),
            device_diag(on_track(1, 0), 15, 4, Instrument, Reason::Misplaced),
            device_diag(on_track(2, 1), 20, 0, Instrument, Reason::WrongChain),
            device_diag(on_track(3, 2), 30, 0, MidiEffect, Reason::WrongChain),
            device_diag(Chain::Master, 41, 1, Instrument, Reason::WrongChain),
        ]
    );
    // A misplaced device does not claim its id: a later valid device with that id plays.
    let mut p = project(vec![track(1, "A", vec![]), track(2, "B", vec![])]);
    Arc::make_mut(&mut p.tracks[0]).channel.devices = vec![
        device(10, DeviceRole::Effect, "EQ"),
        device(11, DeviceRole::Instrument, "Synth"),
    ];
    Arc::make_mut(&mut p.tracks[1]).channel.devices =
        vec![device(11, DeviceRole::Instrument, "Synth")];
    let result = compiled(&p);
    assert_eq!(
        result.diagnostics,
        [device_diag(
            on_track(1, 0),
            11,
            1,
            Instrument,
            Reason::Misplaced
        )]
    );
    assert_eq!(result.program.tracks[1].chain.len(), 1);
}

#[test]
fn midi_effects_beyond_the_stage_limit_are_reported() {
    let mut t = track(1, "Viele", vec![]);
    t.channel.devices = (0..MAX_STAGES as u64 + 2)
        .map(|i| device(100 + i, DeviceRole::MidiEffect, "Fx"))
        .chain([device(200, DeviceRole::Instrument, "Synth")])
        .collect();
    let result = compiled(&project(vec![t]));
    let stages = MAX_STAGES as u64;
    assert_eq!(
        result.diagnostics,
        [
            device_diag(
                on_track(1, 0),
                100 + stages,
                MAX_STAGES,
                DeviceRole::MidiEffect,
                Reason::MidiEffectLimit
            ),
            device_diag(
                on_track(1, 0),
                101 + stages,
                MAX_STAGES + 1,
                DeviceRole::MidiEffect,
                Reason::MidiEffectLimit
            ),
        ]
    );
    assert!(result.diagnostics.iter().all(|d| d.reason.is_limit()));
}

#[test]
fn devices_beyond_the_slot_table_are_reported_in_chain_order() {
    // Two tracks share the table; the slots go to the smallest ids, so the devices of the first
    // track (high ids) are the ones left out, and they are reported in chain order before the
    // master's.
    let mut a = track(1, "A", vec![]);
    a.channel.devices = (0..4)
        .map(|i| device(100_000 - i, DeviceRole::Effect, "Gain"))
        .collect();
    let mut b = track(2, "B", vec![]);
    b.channel.devices = (0..MAX_SLOTS as u64 - 2)
        .map(|i| device(10 + i, DeviceRole::Effect, "Gain"))
        .collect();
    let mut p = project(vec![a, b]);
    Arc::make_mut(&mut p.master).devices = vec![device(200_000, DeviceRole::Effect, "Limiter")];
    let result = compiled(&p);
    assert_eq!(result.program.slots.entries().len(), MAX_SLOTS);
    let effect = DeviceRole::Effect;
    assert_eq!(
        result.diagnostics,
        [
            device_diag(on_track(1, 0), 100_000, 0, effect, Reason::SlotLimit),
            device_diag(on_track(1, 0), 99_999, 1, effect, Reason::SlotLimit),
            device_diag(Chain::Master, 200_000, 0, effect, Reason::SlotLimit),
        ]
    );
}

#[test]
fn a_rebuild_keeps_the_slots_and_reports_the_new_devices_that_find_none() {
    let mut t = track(1, "Viele", vec![]);
    t.channel.devices = (0..MAX_SLOTS as u64)
        .map(|i| device(100 + i, DeviceRole::Effect, "Gain"))
        .collect();
    let mut p = project(vec![t]);
    let first = compiled(&p);
    assert_eq!(first.diagnostics, []);
    // A device with a smaller id than all others: in a fresh build it would win a slot, but the
    // previous program's devices keep theirs.
    Arc::make_mut(&mut p.tracks[0])
        .channel
        .devices
        .push(device(1, DeviceRole::Effect, "Gain"));
    let next = compile::build_with_diagnostics(&p, Some(&first.program), &NoEnv);
    assert_eq!(
        next.diagnostics,
        [device_diag(
            on_track(1, 0),
            1,
            MAX_SLOTS,
            DeviceRole::Effect,
            Reason::SlotLimit
        )]
    );
}

#[test]
fn diagnostics_are_deterministic_and_cover_everything_left_out() {
    // A project that hits every reason at once.
    let mut tracks: Vec<_> = (0..MAX_TRACKS as u64 + 1)
        .map(|i| track(i + 1, &format!("Spur {i}"), vec![]))
        .collect();
    tracks[0].channel.devices = vec![
        device(10, DeviceRole::Effect, "EQ"),
        device(11, DeviceRole::Instrument, "Synth"),
    ];
    tracks[1].channel.devices = vec![device(10, DeviceRole::Effect, "EQ")];
    tracks[2].kind = TrackKind::Bus;
    tracks[2].channel.devices = vec![device(20, DeviceRole::MidiEffect, "Arp")];
    tracks[3].channel.devices = (0..MAX_STAGES as u64 + 1)
        .map(|i| device(30 + i, DeviceRole::MidiEffect, "Fx"))
        .collect();
    tracks[4].channel.devices = (0..MAX_SLOTS as u64)
        .map(|i| device(1000 + i, DeviceRole::Effect, "Gain"))
        .collect();
    tracks.insert(5, track(3, "Doppelt", vec![]));
    let p = project(tracks);

    let first = compiled(&p);
    let second = compiled(&p);
    assert_eq!(first.diagnostics, second.diagnostics);
    let reasons: BTreeSet<_> = first
        .diagnostics
        .iter()
        .map(|d| format!("{:?}", d.reason))
        .collect();
    assert_eq!(reasons.len(), 6, "{reasons:?}");
    assert!(
        first.diagnostics.is_sorted_by_key(|d| match d.subject {
            Subject::Track { index, .. } => (index, 0),
            Subject::Device {
                chain: Chain::Track { index: t, .. },
                index,
                ..
            } => (t, index + 1),
            Subject::Device { index, .. } => (usize::MAX, index + 1),
        }),
        "{:?}",
        first.diagnostics
    );

    // Every device of a played track is in the program or has a diagnostic, never both.
    let played: BTreeSet<_> = first
        .program
        .tracks
        .iter()
        .flat_map(|t| t.chain.iter())
        .chain(&first.program.master.chain)
        .map(|s| s.device)
        .collect();
    let skipped_tracks: BTreeSet<usize> = first
        .diagnostics
        .iter()
        .filter_map(|d| match d.subject {
            Subject::Track { index, .. } => Some(index),
            Subject::Device { .. } => None,
        })
        .collect();
    let mut expected = 0;
    for (index, t) in p.tracks.iter().enumerate() {
        if skipped_tracks.contains(&index) {
            continue;
        }
        for (position, d) in t.channel.devices.iter().enumerate() {
            let reported = first.diagnostics.iter().any(|diag| {
                matches!(diag.subject, Subject::Device {
                    chain: Chain::Track { index: i, .. },
                    index: at,
                    ..
                } if i == index && at == position)
            });
            let in_program = played.contains(&d.id);
            // A duplicate id is in the program through its first occurrence.
            assert!(reported || in_program, "{:?} at {index}/{position}", d.id);
            expected += usize::from(reported);
        }
    }
    let device_diagnostics = first
        .diagnostics
        .iter()
        .filter(|d| matches!(d.subject, Subject::Device { .. }))
        .count();
    assert_eq!(device_diagnostics, expected);
    assert!(project_devices(&p).len() > played.len());
}
