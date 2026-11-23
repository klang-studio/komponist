//! Tests of [`Project::validate`] and the repair pass of loading (#209).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::edits::{self, ProjectProp};
use crate::fixtures::{automation_project, demo_project, demo_song, empty_project};
use crate::persist::{self, Loaded, PersistError, load, to_json};
use crate::project::{ClipContent, Project};
use crate::time::MAX_DOC_TICK;
use crate::validate::RepairKind;
use crate::{Hue, NoteId, Tick, TickRange};

fn demo_value() -> Value {
    serde_json::to_value(demo_project()).unwrap()
}

fn file(project: Value) -> String {
    json!({ "format": "komponist", "version": persist::FORMAT_VERSION, "project": project })
        .to_string()
}

fn load_value(project: Value) -> Result<Loaded, PersistError> {
    load(&file(project))
}

#[track_caller]
fn loaded(project: Value) -> Loaded {
    let loaded = load_value(project).expect("the file loads");
    loaded
        .project
        .validate()
        .expect("a loaded project is valid");
    loaded
}

/// Notes of the first MIDI clip of the demo (track 1).
const NOTES: &str = "/tracks/1/clips/0/content/takes/0/notes";

#[test]
fn fixtures_are_valid_and_load_unchanged() {
    for project in [
        demo_project(),
        demo_song(),
        empty_project(),
        automation_project(),
    ] {
        project.validate().unwrap();
        let loaded = load(&to_json(&project).unwrap()).unwrap();
        assert_eq!(loaded.project, project);
        assert!(loaded.repairs.is_empty(), "{}", loaded.repairs);
    }
}

#[test]
fn a_valid_file_reports_no_repairs() {
    let loaded = loaded(demo_value());
    assert!(loaded.repairs.is_empty());
    assert_eq!(loaded.repairs.total(), 0);
    assert_eq!(loaded.repairs.to_string(), "");
}

#[test]
fn notes_out_of_range_are_repaired_and_a_move_does_not_panic() {
    let mut value = demo_value();
    let notes = value.pointer_mut(NOTES).unwrap().as_array_mut().unwrap();
    let mut extra = |id: u64, field: &str, to: Value| {
        let mut note = notes[0].clone();
        note["id"] = id.into();
        note[field] = to;
        notes.push(note);
    };
    extra(9001, "pitch", 10.into());
    extra(9002, "pitch", 200.into());
    extra(9003, "velocity", 0.into());
    extra(9004, "start", (-5).into());
    extra(9005, "length", 0.into());
    extra(9006, "length", 1_000_000_000_000_000_i64.into());
    extra(9007, "start", 9_000_000_000_000_000_000_i64.into());
    let loaded = loaded(value);
    assert_eq!(loaded.repairs.count(RepairKind::DroppedNotes), 1);
    assert_eq!(loaded.repairs.count(RepairKind::NoteValues), 5);
    let clip = &loaded.project.tracks[1].clips[0];
    let ClipContent::Midi(m) = &clip.content else {
        panic!("a MIDI clip");
    };
    let all: Vec<NoteId> = m.active().notes.iter().map(|n| n.id).collect();
    assert!(!all.contains(&NoteId(9002)));
    let up = edits::move_notes(&loaded.project, clip.id, &all, Tick(10), 1).unwrap();
    up.validate().unwrap();
}

#[test]
fn edits_survive_notes_a_file_could_never_have_loaded_with() {
    // `clamp_note_move` clamped with min 10 > max -73 for pitches 10 and 200: a panic. It is total
    // now, whatever the document holds (also for projects built in memory).
    let mut project = demo_project();
    let clip = project.tracks[1].clips[0].id;
    let track = Arc::make_mut(&mut project.tracks[1]);
    let ClipContent::Midi(m) = &mut Arc::make_mut(&mut track.clips[0]).content else {
        panic!("a MIDI clip");
    };
    let notes = Arc::make_mut(&mut m.takes[0].notes);
    notes[0].pitch = 10;
    notes[1].pitch = 200;
    notes[2].start = Tick(-100);
    let all: Vec<NoteId> = notes.iter().map(|n| n.id).collect();
    for pitch_delta in [-100, -1, 0, 1, 100, i32::MIN, i32::MAX] {
        edits::move_notes(&project, clip, &all, Tick(i64::MIN / 4), pitch_delta).unwrap();
        edits::move_notes(&project, clip, &all, Tick(i64::MAX / 4), pitch_delta).unwrap();
        edits::copy_notes(&project, clip, &all, Tick(7), pitch_delta).unwrap();
    }
}

#[test]
fn ticks_beyond_the_document_are_clamped() {
    let mut value = demo_value();
    // A step clip a quadrillion ticks long (an out-of-memory in `StepClip::hits`) and one that
    // starts so late that `start + length` overflowed.
    value["tracks"][0]["clips"][0]["length"] = 1_000_000_000_000_000_i64.into();
    value["tracks"][1]["clips"][0]["start"] = 9_000_000_000_000_000_000_i64.into();
    value["tracks"][1]["clips"][0]["loop_length"] = 9_000_000_000_000_000_000_i64.into();
    let loaded = loaded(value);
    let project = &loaded.project;
    let step = project.tracks[0].clips[0].clone();
    assert_eq!(step.length, MAX_DOC_TICK);
    let late = project.tracks[1]
        .clips
        .iter()
        .find(|c| c.start == MAX_DOC_TICK)
        .expect("the late clip");
    assert_eq!(late.loop_length, Some(MAX_DOC_TICK));
    assert_eq!(late.end(), MAX_DOC_TICK + late.length, "no overflow");
    assert!(loaded.repairs.count(RepairKind::ClipValues) >= 3);
    assert!(project.song_end() <= MAX_DOC_TICK + MAX_DOC_TICK);
}

#[test]
fn the_edits_keep_every_tick_inside_the_document() {
    let project = demo_project();
    let clip = project.tracks[1].clips[0].id;
    let other = project.tracks[2].clips[0].id;
    let max = i64::MAX;
    for edit in [
        edits::set_clip_length(&project, clip, Tick(max)).unwrap(),
        edits::set_clip_loop(&project, clip, Some(Tick(max)), Tick(max)).unwrap(),
        edits::move_clips(&project, &[clip, other], Tick(max), 0).unwrap(),
        edits::copy_clips(&project, &[clip], Tick(max), 0)
            .unwrap()
            .0,
        edits::set_clip_prop(&project, clip, edits::ClipProp::Start(Tick(max))).unwrap(),
        edits::add_clip(
            &project,
            project.tracks[1].id,
            edits::NewClip {
                name: "late".into(),
                start: Tick(max),
                length: Tick(max),
                content: edits::NewClipContent::Midi {
                    notes: vec![edits::NewNote {
                        start: Tick(max),
                        length: Tick(max),
                        pitch: 60,
                        velocity: 100,
                    }],
                    take_name: String::new(),
                },
            },
        )
        .unwrap()
        .0,
        edits::set_project_prop(
            &project,
            ProjectProp::LoopRange(TickRange::new(Tick(-5), Tick(max))),
        )
        .unwrap(),
    ] {
        edit.validate().unwrap();
    }
    // Moved to the very end, a duplicate lands there too instead of overflowing.
    let end = edits::move_clips(&project, &[clip], Tick(max), 0).unwrap();
    let (end, _) = edits::duplicate_clips(&end, &[clip]).unwrap();
    end.validate().unwrap();
    // Notes: moves, copies and resizes clamp.
    let notes: Vec<NoteId> = match &project.tracks[1].clips[0].content {
        ClipContent::Midi(m) => m.active().notes.iter().map(|n| n.id).collect(),
        _ => unreachable!("a MIDI clip"),
    };
    edits::move_notes(&project, clip, &notes, Tick(max), 0)
        .unwrap()
        .validate()
        .unwrap();
    edits::copy_notes(&project, clip, &notes, Tick(max), 0)
        .unwrap()
        .0
        .validate()
        .unwrap();
    edits::resize_notes(&project, clip, &notes, Tick(max))
        .unwrap()
        .validate()
        .unwrap();
    edits::resize_notes(&project, clip, &notes, Tick(i64::MIN))
        .unwrap()
        .validate()
        .unwrap();
    // A split behind the document is no split.
    let (same, rights) = edits::split_clips(&project, &[clip], Tick(max)).unwrap();
    assert!(rights.is_empty());
    same.validate().unwrap();
}

#[test]
fn duplicate_ids_get_new_ones_and_the_first_holder_keeps_its_id() {
    let mut value = demo_value();
    let first = value["tracks"][0]["id"].clone();
    value["tracks"][1]["id"] = first.clone();
    // A clip, a note and a device that share an id with the first track's.
    value["tracks"][2]["clips"][0]["id"] = first.clone();
    value["tracks"][2]["channel"]["devices"][0]["id"] = first.clone();
    value["tracks"][3]["sends"] = json!([]);
    let note_id = value.pointer(NOTES).unwrap()[0]["id"].clone();
    value.pointer_mut(NOTES).unwrap()[1]["id"] = note_id;
    let before = loaded(demo_value()).project;
    let loaded = loaded(value);
    let p = &loaded.project;
    assert_eq!(loaded.repairs.count(RepairKind::DuplicateIds), 4);
    assert_eq!(p.tracks[0].id.0, first.as_u64().unwrap(), "first holder");
    assert_ne!(p.tracks[1].id, p.tracks[0].id);
    assert!(p.tracks[1].id.0 >= before.next_id, "a fresh id");
    assert!(p.next_id > p.tracks[1].id.0);
    // Edits address each track by its own id.
    let renamed = edits::set_track_prop(
        p,
        &[p.tracks[1].id],
        edits::TrackProp::Name("Zwei".to_owned()),
    )
    .unwrap();
    assert_eq!(renamed.tracks[1].name, "Zwei");
    assert_ne!(renamed.tracks[0].name, "Zwei");
}

#[test]
fn references_follow_the_first_holder_of_a_duplicate_track_id() {
    // A bus and a MIDI track with the same id: the outputs that name it keep naming the bus (the
    // first), the MIDI track gets a new id.
    let mut p = demo_project();
    let t: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
    p = edits::add_bus(&p, edits::NewBus::effect(vec![t[3]]))
        .unwrap()
        .0;
    let bus = p.tracks.last().unwrap().id;
    let mut value = serde_json::to_value(&p).unwrap();
    // Move the bus in front of the track that will share its id.
    let tracks = value["tracks"].as_array_mut().unwrap();
    let bus_value = tracks.pop().unwrap();
    tracks.insert(0, bus_value);
    value["tracks"][1]["id"] = bus.0.into();
    let loaded = loaded(value);
    let p = &loaded.project;
    assert_eq!(p.tracks[0].id, bus);
    assert!(p.tracks[0].is_bus());
    assert_ne!(p.tracks[1].id, bus);
    assert!(p.tracks.iter().skip(1).all(|t| t.id != bus));
}

#[test]
fn chains_are_ordered_and_hold_one_instrument() {
    let mut value = demo_value();
    // Track 1: instrument, effect. Reverse it, and add a second instrument.
    let devices = value["tracks"][1]["channel"]["devices"]
        .as_array_mut()
        .unwrap();
    let mut second = devices[0].clone();
    second["id"] = 9100.into();
    devices.reverse();
    devices.push(second);
    // The master gets an instrument.
    let mut instrument = value["tracks"][0]["channel"]["devices"][0].clone();
    instrument["id"] = 9101.into();
    value["master"]["devices"] = json!([instrument]);
    let loaded = loaded(value);
    let chain = &loaded.project.tracks[1].channel.devices;
    assert_eq!(chain[0].role, crate::DeviceRole::Instrument);
    assert!(
        chain[1..]
            .iter()
            .all(|d| d.role == crate::DeviceRole::Effect)
    );
    assert_eq!(loaded.repairs.count(RepairKind::ChainOrder), 1);
    assert_eq!(loaded.repairs.count(RepairKind::DroppedDevices), 2);
    assert!(loaded.project.master.devices.is_empty());
}

#[test]
fn mixer_values_loop_range_and_count_in_are_clamped() {
    let mut value = demo_value();
    value["tracks"][0]["channel"]["volume_db"] = 99.0.into();
    value["tracks"][0]["channel"]["pan"] = (-5.0).into();
    value["tracks"][1]["height"] = 0.into();
    value["tracks"][2]["hue"] = 33.into();
    value["master"]["volume_db"] = (-1000.0).into();
    value["count_in_bars"] = 200.into();
    value["loop_range"] = json!({ "start": -5, "end": 1_000_000_000_000_000_i64 });
    let loaded = loaded(value);
    let p = &loaded.project;
    assert_eq!(p.tracks[0].channel.volume_db, crate::project::VOLUME_DB_MAX);
    assert_eq!(p.tracks[0].channel.pan, -1.0);
    assert_eq!(p.tracks[1].height, crate::project::TRACK_HEIGHT_MIN);
    assert_eq!(p.tracks[2].hue, Hue::RED, "the nearest token hue");
    assert_eq!(p.master.volume_db, crate::project::VOLUME_DB_MIN);
    assert_eq!(p.count_in_bars, edits::MAX_COUNT_IN_BARS);
    assert_eq!(p.loop_range, TickRange::new(Tick::ZERO, MAX_DOC_TICK));
    assert_eq!(loaded.repairs.count(RepairKind::LoopRange), 1);
    assert_eq!(loaded.repairs.count(RepairKind::CountIn), 1);
    assert_eq!(loaded.repairs.count(RepairKind::MixerValues), 4);
    // An empty loop range starts over with the default.
    let mut value = demo_value();
    value["loop_range"] = json!({ "start": 10, "end": 10 });
    assert_eq!(
        loaded_range(value),
        TickRange::new(Tick::ZERO, Tick::from_steps(64))
    );
}

fn loaded_range(value: Value) -> TickRange {
    loaded(value).project.loop_range
}

#[test]
fn step_values_are_clamped() {
    let mut value = demo_value();
    let s = &mut value["tracks"][0]["clips"][0]["content"];
    s["swing"] = 7.0.into();
    s["rows"][0]["note"] = 200.into();
    s["rows"][0]["steps"][0]["velocity"] = 0.into();
    s["rows"][0]["steps"][1]["probability"] = 250.into();
    s["rows"][0]["steps"][2]["ratchet"] = 9.into();
    let loaded = loaded(value);
    assert_eq!(loaded.repairs.count(RepairKind::StepValues), 5);
}

#[test]
fn a_clip_that_cannot_loop_loses_its_loop_length() {
    let mut value = demo_value();
    value["tracks"][0]["clips"][0]["loop_length"] = 480.into();
    let loaded = loaded(value);
    assert_eq!(loaded.project.tracks[0].clips[0].loop_length, None);
    assert_eq!(loaded.repairs.count(RepairKind::ClipLoops), 1);
}

#[test]
fn automation_repairs_are_reported_not_printed() {
    let p = automation_project();
    let mut value = serde_json::to_value(&p).unwrap();
    let lanes = value["tracks"][2]["automation"]["lanes"]
        .as_array_mut()
        .unwrap();
    lanes[0]["points"] = json!([[50, 0.2], [10, 0.1]]);
    let mut duplicate = lanes[0].clone();
    duplicate["id"] = 777_777.into();
    lanes.push(duplicate);
    lanes[0]["height"] = 999.into();
    let loaded = loaded(value);
    assert_eq!(loaded.repairs.count(RepairKind::AutomationPoints), 2);
    assert_eq!(loaded.repairs.count(RepairKind::AutomationLanes), 1);
    assert_eq!(loaded.repairs.count(RepairKind::MixerValues), 1);
    // Points keep inside the document.
    let mut value = serde_json::to_value(&p).unwrap();
    value["tracks"][2]["automation"]["lanes"][0]["points"] = json!([
        [-9_000_000_000_000_000_000_i64, 0.2],
        [9_000_000_000_000_000_000_i64, 0.1]
    ]);
    let loaded = self::loaded(value);
    let points = loaded.project.tracks[2].automation.lanes[0]
        .points
        .as_slice();
    assert_eq!(points[0].tick, -MAX_DOC_TICK);
    assert_eq!(points[1].tick, MAX_DOC_TICK);
}

#[test]
fn macro_repairs_are_reported() {
    let mut value = demo_value();
    value["tracks"][0]["macros"] = json!([{
        "value": 3.0,
        "mappings": [
            { "target": { "kind": "pan" }, "label": "Pan", "min": 0.9, "max": 0.1 },
            { "target": { "kind": "pan" }, "label": "Pan again", "min": 0.0, "max": 1.0 },
        ],
    }]);
    let loaded = loaded(value);
    // One range swapped, one duplicate mapping dropped.
    assert_eq!(loaded.repairs.count(RepairKind::MacroMappings), 2);
    assert_eq!(loaded.repairs.total(), 2);
    let text = loaded.repairs.to_string();
    assert!(text.contains("2 × macro mapping"), "{text}");
}

#[test]
fn what_cannot_be_repaired_is_still_rejected() {
    for (pointer, value) in [
        ("/tempo/bpm", json!(0.5)),
        ("/meter/num", json!(0)),
        ("/tracks/1/clips/0/content/takes", json!([])),
        ("/tracks/1/clips/0/content/active_take", json!(5)),
        ("/tracks/0/clips/0/content/length_steps", json!(0)),
    ] {
        let mut v = demo_value();
        *v.pointer_mut(pointer).unwrap() = value;
        assert!(
            matches!(load_value(v), Err(PersistError::Malformed(_))),
            "{pointer}"
        );
    }
}

#[test]
fn validate_names_what_is_broken() {
    let mut p = demo_project();
    assert!(p.validate().is_ok());
    p.loop_range = TickRange::new(Tick(5), Tick(5));
    let error = p.validate().unwrap_err();
    assert_eq!(error.to_string(), "loop range out of bounds");
    let mut p = demo_project();
    let id = p.tracks[0].id;
    Arc::make_mut(&mut p.tracks[1]).id = id;
    assert!(p.validate().unwrap_err().to_string().contains("used twice"));
    let mut p: Project = demo_project();
    p.next_id = 1;
    assert!(p.validate().unwrap_err().to_string().contains("next_id"));
}
