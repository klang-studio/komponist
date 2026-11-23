//! Unit tests for every edit, plus [`check_invariants`] shared with the property tests.

use std::collections::HashSet;

use super::*;
use crate::fixtures::demo_project;
use crate::project::StepClip;

// ------------------------------------------------------------------------------------------------
// Fixture and helpers
// ------------------------------------------------------------------------------------------------

/// Asserts the invariants documented in [`crate::project`] that every edit must keep
/// ([`Project::validate`]).
#[track_caller]
pub(crate) fn check_invariants(p: &Project) {
    if let Err(error) = p.validate() {
        panic!("invalid project: {error}");
    }
}

/// Unwraps an edit result and checks the invariants.
#[track_caller]
fn ok(result: EditResult) -> Project {
    let p = result.expect("edit succeeds");
    check_invariants(&p);
    p
}

/// Unwraps an edit result with a value and checks the invariants.
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

fn sampler() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_SAMPLER.to_owned(),
        name: "Sampler".to_owned(),
        vendor: "komponist".to_owned(),
        version: "0.1.0".to_owned(),
        module: Box::default(),
    }
}

fn new_track(name: &str, instrument: Option<PluginRef>) -> NewTrack {
    NewTrack {
        name: Some(name.to_owned()),
        hue: None,
        instrument,
        index: None,
        arm_exclusive: false,
    }
}

fn nn(start: i64, length: i64, pitch: u8, velocity: u8) -> NewNote {
    NewNote {
        start: Tick(start),
        length: Tick(length),
        pitch,
        velocity,
    }
}

fn midi_clip(start: i64, length: i64, notes: Vec<NewNote>) -> NewClip {
    NewClip {
        start: Tick(start),
        length: Tick(length),
        name: "MIDI-Clip".to_owned(),
        content: NewClipContent::Midi {
            notes,
            take_name: String::new(),
        },
    }
}

fn step_clip(start: i64, length: i64) -> NewClip {
    NewClip {
        start: Tick(start),
        length: Tick(length),
        name: "Pattern 1".to_owned(),
        content: NewClipContent::Step { rows: Vec::new() },
    }
}

/// Tracks "A" (instrument "Synth" and effect "EQ"), "B" (no devices) and "C"; a MIDI clip on A at
/// bar 2 with three notes, a step clip on B at bar 1.
struct Fixture {
    p: Project,
    a: TrackId,
    b: TrackId,
    c: TrackId,
    midi: ClipId,
    step: ClipId,
    notes: Vec<NoteId>,
    synth: DeviceId,
    eq: DeviceId,
}

fn fixture() -> Fixture {
    let p = Project::new("Test");
    let (p, a) = add_track(&p, new_track("A", Some(plugin("Synth")))).unwrap();
    let (p, b) = add_track(&p, new_track("B", None)).unwrap();
    let (p, c) = add_track(&p, new_track("C", None)).unwrap();
    let synth = p.tracks[0].channel.devices[0].id;
    let (p, eq) = insert_device(
        &p,
        ChannelRef::Track(a),
        1,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("EQ"),
            state: None,
            sidechain: None,
        },
    )
    .unwrap();
    let notes = vec![
        nn(0, 480, 60, 100),
        nn(480, 240, 64, 90),
        nn(960, 960, 67, 80),
    ];
    let (p, midi) = add_clip(&p, a, midi_clip(3840, 3840, notes)).unwrap();
    let (p, step) = add_clip(&p, b, step_clip(0, 3840)).unwrap();
    let notes = note_ids(&p, midi);
    check_invariants(&p);
    Fixture {
        p,
        a,
        b,
        c,
        midi,
        step,
        notes,
        synth,
        eq,
    }
}

fn clip(p: &Project, id: ClipId) -> &Clip {
    p.find_clip(id).expect("clip exists").1
}

fn track_of(p: &Project, id: ClipId) -> TrackId {
    p.find_clip(id).expect("clip exists").0.id
}

fn take_notes(p: &Project, id: ClipId) -> &[Note] {
    match &clip(p, id).content {
        ClipContent::Midi(m) => &m.active().notes,
        ClipContent::Step(_) | ClipContent::Audio(_) => panic!("not a MIDI clip"),
    }
}

fn note_ids(p: &Project, id: ClipId) -> Vec<NoteId> {
    take_notes(p, id).iter().map(|n| n.id).collect()
}

/// `(start, length, pitch, velocity)` of the active take.
fn notes(p: &Project, id: ClipId) -> Vec<(i64, i64, u8, u8)> {
    take_notes(p, id)
        .iter()
        .map(|n| (n.start.0, n.length.0, n.pitch, n.velocity))
        .collect()
}

fn note(p: &Project, clip: ClipId, id: NoteId) -> Note {
    *take_notes(p, clip)
        .iter()
        .find(|n| n.id == id)
        .expect("note exists")
}

fn pattern(p: &Project, id: ClipId) -> &StepClip {
    match &clip(p, id).content {
        ClipContent::Step(s) => s,
        ClipContent::Midi(_) | ClipContent::Audio(_) => panic!("not a step clip"),
    }
}

fn track(p: &Project, id: TrackId) -> &Track {
    p.track(id).expect("track exists")
}

fn track_names(p: &Project) -> Vec<&str> {
    p.tracks.iter().map(|t| t.name.as_str()).collect()
}

fn chain(p: &Project, id: TrackId) -> Vec<(String, DeviceRole)> {
    track(p, id)
        .channel
        .devices
        .iter()
        .map(|d| (d.plugin.name.clone(), d.role))
        .collect()
}

fn chain_ids(p: &Project, id: TrackId) -> Vec<DeviceId> {
    track(p, id).channel.devices.iter().map(|d| d.id).collect()
}

// ------------------------------------------------------------------------------------------------
// Helpers for callers
// ------------------------------------------------------------------------------------------------

#[test]
fn next_hue_follows_the_auto_order() {
    let p = Project::new("x");
    assert_eq!(next_hue(&p), Hue::GREEN);
    let (p, _) = add_track(&p, new_track("1", None)).unwrap();
    let (p, _) = add_track(&p, new_track("2", None)).unwrap();
    assert_eq!(
        p.tracks.iter().map(|t| t.hue).collect::<Vec<_>>(),
        [Hue::GREEN, Hue::BLUE]
    );
    assert_eq!(next_hue(&p), Hue::VIOLET);
    // The demo uses 45, 250, 150, 300, 95, 200: the next free in the order is red.
    assert_eq!(next_hue(&demo_project()), Hue::RED);
}

#[test]
fn next_hue_cycles_when_all_are_used() {
    let mut p = Project::new("x");
    for (i, hue) in Hue::AUTO_ORDER.into_iter().enumerate() {
        let mut spec = new_track(&i.to_string(), None);
        spec.hue = Some(hue);
        p = add_track(&p, spec).unwrap().0;
    }
    // 13 tracks: ALL[13 % 14] = pink.
    assert_eq!(next_hue(&p), Hue::ALL[13]);
    let (p, _) = add_track(&p, new_track("x", None)).unwrap();
    assert_eq!(next_hue(&p), Hue::ALL[0]);
}

#[test]
fn unique_track_names() {
    let p = demo_project();
    assert_eq!(unique_track_name(&p, "Neu"), "Neu");
    assert_eq!(unique_track_name(&p, "Drums"), "Drums 2");
    let (p, _) = add_track(&p, new_track("Drums 2", None)).unwrap();
    assert_eq!(unique_track_name(&p, "Drums"), "Drums 3");
}

#[test]
fn next_take_numbers() {
    let f = fixture();
    assert_eq!(next_take_number(&f.p), 1);
    let p = ok(set_clip_prop(&f.p, f.midi, ClipProp::Name("Take 3".into())));
    assert_eq!(next_take_number(&p), 4);
    let (p, _) = ok_with(add_take(&p, f.midi, "Take 7".into()));
    assert_eq!(next_take_number(&p), 8);
    let (p, _) = ok_with(add_take(&p, f.midi, "Take 12a".into()));
    let (p, _) = ok_with(add_take(&p, f.midi, "● Aufnahme · Take 20".into()));
    assert_eq!(next_take_number(&p), 8);
    let (p, _) = ok_with(add_take(&p, f.midi, "Take 13 · incomplete".into()));
    assert_eq!(next_take_number(&p), 14);
}

// ------------------------------------------------------------------------------------------------
// Project
// ------------------------------------------------------------------------------------------------

#[test]
fn project_name_and_tempo() {
    let p = Project::new("x");
    let q = ok(set_project_prop(&p, ProjectProp::Name("  Nacht  ".into())));
    assert_eq!(q.name, "Nacht");
    assert_eq!(
        set_project_prop(&p, ProjectProp::Name("  ".into())),
        Err(EditError::Invalid("empty name"))
    );
    for bpm in [Tempo::MIN_BPM, 124.5, Tempo::MAX_BPM] {
        let q = ok(set_project_prop(&p, ProjectProp::Tempo(Tempo::new(bpm))));
        assert_eq!(q.tempo.bpm, bpm);
    }
    for bpm in [39.99, 240.01, f64::NAN, f64::INFINITY] {
        assert!(set_project_prop(&p, ProjectProp::Tempo(Tempo::new(bpm))).is_err());
    }
}

#[test]
fn project_meter() {
    let p = Project::new("x");
    let six_eight = Meter { num: 6, den: 8 };
    assert_eq!(
        ok(set_project_prop(&p, ProjectProp::Meter(six_eight))).meter,
        six_eight
    );
    for (num, den) in [(0, 4), (33, 4), (4, 3), (4, 0), (4, 32)] {
        assert_eq!(
            set_project_prop(&p, ProjectProp::Meter(Meter { num, den })),
            Err(EditError::Invalid("meter not supported")),
            "{num}/{den}"
        );
    }
}

#[test]
fn project_loop_and_transport() {
    let p = Project::new("x");
    let range = |s, e| ProjectProp::LoopRange(TickRange::new(Tick(s), Tick(e)));
    let q = ok(set_project_prop(&p, range(-100, 3840)));
    assert_eq!(q.loop_range, TickRange::new(Tick(0), Tick(3840)));
    assert!(set_project_prop(&p, range(3840, 3840)).is_err());
    assert!(set_project_prop(&p, range(-200, -100)).is_err());
    let q = ok(set_project_prop(&p, ProjectProp::LoopOn(true)));
    assert!(q.loop_on);
    let q = ok(set_project_prop(&p, ProjectProp::Metronome(true)));
    assert!(q.metronome);
    let q = ok(set_project_prop(&p, ProjectProp::CountInBars(9)));
    assert_eq!(q.count_in_bars, 4);
    let q = ok(set_project_prop(&p, ProjectProp::CountInBars(0)));
    assert_eq!(q.count_in_bars, 0);
}

#[test]
fn master_values() {
    let p = demo_project();
    let q = ok(set_master_prop(&p, ChannelProp::VolumeDb(-3.5)));
    assert_eq!(q.master.volume_db, -3.5);
    assert!(
        q.tracks
            .iter()
            .zip(&p.tracks)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    let q = ok(set_master_prop(
        &p,
        ChannelProp::VolumeDb(f32::NEG_INFINITY),
    ));
    assert_eq!(q.master.volume_db, VOLUME_DB_MIN);
    let q = ok(set_master_prop(&p, ChannelProp::VolumeDb(12.0)));
    assert_eq!(q.master.volume_db, VOLUME_DB_MAX);
    assert!(set_master_prop(&p, ChannelProp::VolumeDb(f32::NAN)).is_err());
    let q = ok(set_master_prop(&p, ChannelProp::Pan(-2.0)));
    assert_eq!(q.master.pan, -1.0);
    let q = ok(set_master_prop(&p, ChannelProp::Mute(true)));
    assert!(q.master.mute);
    let q = ok(set_master_prop(&p, ChannelProp::Solo(true)));
    assert!(!q.master.solo);
    assert!(Arc::ptr_eq(&q.master, &p.master));
}

// ------------------------------------------------------------------------------------------------
// Tracks
// ------------------------------------------------------------------------------------------------

#[test]
fn add_track_defaults() {
    let p = demo_project();
    let next = p.next_id;
    let spec = NewTrack {
        name: None,
        hue: None,
        instrument: Some(plugin("Vital")),
        index: None,
        arm_exclusive: false,
    };
    let (q, id) = ok_with(add_track(&p, spec));
    assert_eq!(id, TrackId(next));
    let t = q.tracks.last().unwrap();
    assert_eq!(
        (t.id, t.name.as_str(), t.hue, t.height),
        (id, "Vital", Hue::RED, TRACK_HEIGHT_DEFAULT)
    );
    assert_eq!(t.kind, TrackKind::Midi);
    assert_eq!(
        (t.input.clone(), t.midi_channel),
        (MidiInputSel::All, MidiChannelSel::Omni)
    );
    assert_eq!(t.channel.volume_db, 0.0);
    assert!(!t.arm && !t.monitor && t.clips.is_empty());
    let d = &t.channel.devices[0];
    assert_eq!(
        (d.id, d.role, d.bypass),
        (DeviceId(next + 1), DeviceRole::Instrument, false)
    );
    assert_eq!(d.state, None);
    assert_eq!(q.next_id, next + 2);
    // Other tracks stay shared.
    assert!(
        p.tracks
            .iter()
            .zip(&q.tracks)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
}

#[test]
fn add_track_names() {
    let p = demo_project();
    let named = |instrument: Option<PluginRef>| {
        let spec = NewTrack {
            name: None,
            hue: None,
            instrument,
            index: None,
            arm_exclusive: false,
        };
        add_track(&p, spec)
            .unwrap()
            .0
            .tracks
            .last()
            .unwrap()
            .name
            .clone()
    };
    assert_eq!(named(Some(sampler())), "Drums 2");
    assert_eq!(named(None), "MIDI");
    assert_eq!(named(Some(plugin(" "))), "MIDI");
    let (q, _) = ok_with(add_track(&p, new_track("  Bass ", None)));
    assert_eq!(
        q.tracks.last().unwrap().name,
        "Bass",
        "explicit names are kept"
    );
    assert_eq!(
        add_track(&p, new_track(" ", None)),
        Err(EditError::Invalid("empty name"))
    );
}

/// The words of an English app: the model writes no language of its own.
const EN: DefaultNames = DefaultNames {
    track: "MIDI",
    drums: "Drums",
    bus: "Bus",
    group: "Group",
    audio: "Audio",
};

#[test]
fn default_names_come_from_the_names_the_caller_passes() {
    // Without names the model writes German, as it always did.
    assert_eq!(DefaultNames::default(), DefaultNames::DE);
    assert_eq!(DefaultNames::DE.group, "Gruppe");
    let p = demo_project();
    let spec = |instrument| NewTrack {
        name: None,
        hue: None,
        instrument,
        index: None,
        arm_exclusive: false,
    };
    let last = |p: &Project| p.tracks.last().unwrap().name.clone();
    assert_eq!(last(&add_track_in(&p, spec(None), &EN).unwrap().0), "MIDI");
    let words = DefaultNames {
        track: "Spur",
        drums: "Schlagzeug",
        ..EN
    };
    assert_eq!(
        last(&add_track_in(&p, spec(None), &words).unwrap().0),
        "Spur"
    );
    let (q, _) = add_track_in(&p, spec(Some(sampler())), &words).unwrap();
    assert_eq!(last(&q), "Schlagzeug");
    // An explicit name wins over the words.
    let (q, _) = add_track_in(&p, new_track("Bass", None), &words).unwrap();
    assert_eq!(last(&q), "Bass");
    // Buses and groups: the base word, numbered from 1.
    let (q, group) = add_bus_in(&p, NewBus::group(vec![p.tracks[0].id]), &EN).unwrap();
    assert_eq!(q.track(group).unwrap().name, "Group 1");
    let (q, bus) = add_bus_in(&q, NewBus::effect(vec![]), &EN).unwrap();
    assert_eq!(q.track(bus).unwrap().name, "Bus 1");
    let (q, again) = add_bus_in(&q, NewBus::group(vec![q.tracks[2].id]), &EN).unwrap();
    assert_eq!(q.track(again).unwrap().name, "Group 2");
}

#[test]
fn add_track_hue_index_and_arm() {
    let f = fixture();
    let mut spec = new_track("D", None);
    spec.hue = Some(Hue(46));
    assert_eq!(
        add_track(&f.p, spec.clone()),
        Err(EditError::Invalid("hue is not a token hue"))
    );
    spec.hue = Some(Hue::VERMILION);
    spec.index = Some(1);
    let (p, _) = ok_with(add_track(&f.p, spec.clone()));
    assert_eq!(track_names(&p), ["A", "D", "B", "C"]);
    assert_eq!(p.tracks[1].hue, Hue::VERMILION);
    spec.index = Some(99);
    assert_eq!(
        track_names(&ok_with(add_track(&f.p, spec.clone())).0),
        ["A", "B", "C", "D"]
    );

    let p = ok(set_track_prop(&f.p, &[f.a, f.b], TrackProp::Arm(true)));
    spec.index = None;
    spec.arm_exclusive = true;
    let (p, d) = ok_with(add_track(&p, spec));
    let armed: Vec<(bool, bool)> = p.tracks.iter().map(|t| (t.arm, t.monitor)).collect();
    // Arming switched monitoring on for A and B; disarming keeps it.
    assert_eq!(
        armed,
        [(false, true), (false, true), (false, false), (true, true)]
    );
    assert_eq!(p.tracks[3].id, d);
}

#[test]
fn remove_tracks_with_everything() {
    let f = fixture();
    let p = ok(remove_tracks(&f.p, &[f.a, f.c]));
    assert_eq!(track_names(&p), ["B"]);
    assert!(p.find_clip(f.midi).is_none() && p.find_device(f.synth).is_none());
    assert_eq!(
        remove_tracks(&f.p, &[f.a, TrackId(999)]),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
}

#[test]
fn move_tracks_keeps_relative_order() {
    let f = fixture();
    let (p, d) = add_track(&f.p, new_track("D", None)).unwrap();
    // Display order wins over the order of the list.
    let q = ok(move_tracks(&p, &[d, f.b], 0));
    assert_eq!(track_names(&q), ["B", "D", "A", "C"]);
    let q = ok(move_tracks(&p, &[f.a], 2));
    assert_eq!(track_names(&q), ["B", "C", "A", "D"]);
    let q = ok(move_tracks(&p, &[f.a, f.b], 99));
    assert_eq!(track_names(&q), ["C", "D", "A", "B"]);
    assert!(move_tracks(&p, &[TrackId(999)], 0).is_err());
}

#[test]
fn track_props() {
    let f = fixture();
    let set = |prop| set_track_prop(&f.p, &[f.a], prop);
    assert_eq!(
        track(&ok(set(TrackProp::Name(" Lead ".into()))), f.a).name,
        "Lead"
    );
    assert!(set(TrackProp::Name(String::new())).is_err());
    assert_eq!(
        track(&ok(set(TrackProp::Hue(Hue::PINK))), f.a).hue,
        Hue::PINK
    );
    assert!(set(TrackProp::Hue(Hue(0))).is_err());
    let p = ok(set(TrackProp::Channel(ChannelProp::VolumeDb(-80.0))));
    assert_eq!(track(&p, f.a).channel.volume_db, VOLUME_DB_MIN);
    let p = ok(set(TrackProp::Channel(ChannelProp::Pan(0.25))));
    assert_eq!(track(&p, f.a).channel.pan, 0.25);
    let p = ok(set(TrackProp::Channel(ChannelProp::Solo(true))));
    assert!(track(&p, f.a).channel.solo && p.any_solo());
    let p = ok(set(TrackProp::Arm(true)));
    assert!(track(&p, f.a).arm && track(&p, f.a).monitor);
    let p = ok(set_track_prop(&p, &[f.a], TrackProp::Arm(false)));
    assert!(!track(&p, f.a).arm && track(&p, f.a).monitor);
    let p = ok(set_track_prop(&p, &[f.a], TrackProp::Monitor(false)));
    assert!(!track(&p, f.a).monitor);
    let input = MidiInputSel::Device("Arturia KeyStep".into());
    let p = ok(set(TrackProp::Input(input.clone())));
    assert_eq!(track(&p, f.a).input, input);
    let p = ok(set(TrackProp::MidiChannel(MidiChannelSel::Channel(15))));
    assert_eq!(track(&p, f.a).midi_channel, MidiChannelSel::Channel(15));
    assert!(set(TrackProp::MidiChannel(MidiChannelSel::Channel(16))).is_err());
    assert_eq!(
        track(&ok(set(TrackProp::Height(10))), f.a).height,
        TRACK_HEIGHT_MIN
    );
    assert_eq!(
        track(&ok(set(TrackProp::Height(500))), f.a).height,
        TRACK_HEIGHT_MAX
    );
    assert_eq!(
        set_track_prop(&f.p, &[f.a, TrackId(999)], TrackProp::Arm(true)),
        Err(EditError::TrackNotFound(TrackId(999)))
    );
}

#[test]
fn track_props_apply_to_all_listed_and_share_unchanged() {
    let f = fixture();
    let p = ok(set_track_prop(
        &f.p,
        &[f.a, f.b, f.c],
        TrackProp::Channel(ChannelProp::Mute(true)),
    ));
    assert!(p.tracks.iter().all(|t| t.channel.mute));
    // Muting again changes nothing: every track stays the same `Arc`.
    let q = ok(set_track_prop(
        &p,
        &[f.a, f.b, f.c],
        TrackProp::Channel(ChannelProp::Mute(true)),
    ));
    assert!(
        p.tracks
            .iter()
            .zip(&q.tracks)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
}

#[test]
fn exclusive_solo_and_arm() {
    let f = fixture();
    let p = ok(set_track_prop(
        &f.p,
        &[f.a, f.b],
        TrackProp::Channel(ChannelProp::Solo(true)),
    ));
    let p = ok(set_exclusive(&p, f.c, ExclusiveFlag::Solo));
    let solo: Vec<bool> = p.tracks.iter().map(|t| t.channel.solo).collect();
    assert_eq!(solo, [false, false, true]);
    let p = ok(set_exclusive(&p, f.b, ExclusiveFlag::Arm));
    let arm: Vec<(bool, bool)> = p.tracks.iter().map(|t| (t.arm, t.monitor)).collect();
    assert_eq!(arm, [(false, false), (true, true), (false, false)]);
    assert!(set_exclusive(&p, TrackId(999), ExclusiveFlag::Arm).is_err());
}

// ------------------------------------------------------------------------------------------------
// Clips
// ------------------------------------------------------------------------------------------------

#[test]
fn add_midi_clip() {
    let f = fixture();
    let next = f.p.next_id;
    let spec = NewClip {
        name: "Take 1".into(),
        content: NewClipContent::Midi {
            notes: vec![
                nn(480, 0, 200, 0),
                nn(-5, 240, 60, 200),
                nn(0, 240, 55, 100),
            ],
            take_name: "Take 1".into(),
        },
        ..midi_clip(-100, 0, Vec::new())
    };
    let (p, id) = ok_with(add_clip(&f.p, f.c, spec));
    assert_eq!(id, ClipId(next));
    let c = clip(&p, id);
    assert_eq!(
        (c.start, c.length, c.name.as_str()),
        (Tick(0), Tick(1), "Take 1")
    );
    assert_eq!(
        (c.loop_length, c.mute, c.transpose, c.velocity_pct),
        (None, false, 0, 0)
    );
    let ClipContent::Midi(m) = &c.content else {
        panic!("MIDI clip");
    };
    assert_eq!((m.takes.len(), m.active_take), (1, 0));
    assert_eq!(m.active().name, "Take 1");
    // Clamped, sorted, ids in input order.
    assert_eq!(
        notes(&p, id),
        [(0, 240, 55, 100), (0, 240, 60, 127), (480, 1, 127, 1)]
    );
    assert_eq!(
        note_ids(&p, id),
        [NoteId(next + 3), NoteId(next + 2), NoteId(next + 1)]
    );
    assert_eq!(track_of(&p, id), f.c);
    assert!(add_clip(&f.p, TrackId(999), midi_clip(0, 1, Vec::new())).is_err());
}

#[test]
fn add_step_clip_with_default_or_given_rows() {
    let f = fixture();
    let s = pattern(&f.p, f.step);
    let rows: Vec<(&str, u8)> = s.rows.iter().map(|r| (r.name.as_str(), r.note)).collect();
    assert_eq!(rows, DEFAULT_STEP_ROWS);
    assert_eq!(
        (s.length_steps, s.resolution, s.swing),
        (16, StepResolution::Sixteenth, 0.0)
    );
    assert!(s.rows.iter().all(|r| !r.mute && !r.solo));
    assert!(
        s.rows
            .iter()
            .flat_map(|r| r.steps.iter())
            .all(|st| *st == Step::default())
    );

    let spec = NewClip {
        content: NewClipContent::Step {
            rows: vec![
                StepRowSpec {
                    name: "Pad 1".into(),
                    note: 36,
                },
                StepRowSpec {
                    name: "Pad 2".into(),
                    note: 200,
                },
            ],
        },
        ..step_clip(0, 3840)
    };
    let (p, id) = ok_with(add_clip(&f.p, f.c, spec));
    let rows: Vec<(&str, u8)> = pattern(&p, id)
        .rows
        .iter()
        .map(|r| (r.name.as_str(), r.note))
        .collect();
    assert_eq!(rows, [("Pad 1", 36), ("Pad 2", 127)]);
}

#[test]
fn clips_stay_sorted_on_their_track() {
    let f = fixture();
    let (p, early) = ok_with(add_clip(&f.p, f.a, midi_clip(0, 960, Vec::new())));
    let (p, same) = ok_with(add_clip(&p, f.a, midi_clip(3840, 960, Vec::new())));
    let ids: Vec<ClipId> = track(&p, f.a).clips.iter().map(|c| c.id).collect();
    assert_eq!(ids, [early, f.midi, same]);
}

#[test]
fn remove_clips_anywhere() {
    let f = fixture();
    let p = ok(remove_clips(&f.p, &[f.midi, f.step]));
    assert!(p.tracks.iter().all(|t| t.clips.is_empty()));
    assert!(Arc::ptr_eq(&p.tracks[2], &f.p.tracks[2]));
    assert_eq!(
        remove_clips(&f.p, &[f.midi, ClipId(999)]),
        Err(EditError::ClipNotFound(ClipId(999)))
    );
}

#[test]
fn move_clips_in_time_and_across_tracks() {
    let f = fixture();
    let p = ok(move_clips(&f.p, &[f.midi], Tick(960), 2));
    let c = clip(&p, f.midi);
    assert_eq!((c.start, track_of(&p, f.midi)), (Tick(4800), f.c));
    assert!(track(&p, f.a).clips.is_empty());
    assert_eq!(take_notes(&p, f.midi), take_notes(&f.p, f.midi));
    // The group is clamped as a whole: the step clip at 0 stops the move left, the bottom track
    // stops the move down.
    let p = ok(move_clips(&f.p, &[f.midi, f.step], Tick(-10_000), 5));
    assert_eq!(clip(&p, f.midi).start, Tick(3840));
    assert_eq!(clip(&p, f.step).start, Tick(0));
    assert_eq!((track_of(&p, f.midi), track_of(&p, f.step)), (f.b, f.c));
    let p = ok(move_clips(&f.p, &[f.step], Tick(0), -5));
    assert_eq!(track_of(&p, f.step), f.a);
    // Nothing to move: the project stays shared.
    let p = ok(move_clips(&f.p, &[f.step], Tick(-5), 0));
    assert!(
        p.tracks
            .iter()
            .zip(&f.p.tracks)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    assert!(move_clips(&f.p, &[ClipId(999)], Tick(0), 0).is_err());
}

#[test]
fn copy_clips_with_new_ids() {
    let f = fixture();
    let (p, copies) = ok_with(copy_clips(&f.p, &[f.step, f.midi, f.step], Tick(3840), 1));
    assert_eq!(copies.len(), 2, "repeated ids copy once");
    let (step_copy, midi_copy) = (copies[0], copies[1]);
    assert_eq!(clip(&p, f.midi), clip(&f.p, f.midi), "the original stays");
    let c = clip(&p, midi_copy);
    assert_eq!((c.start, track_of(&p, midi_copy)), (Tick(7680), f.b));
    assert_eq!(notes(&p, midi_copy), notes(&p, f.midi));
    let fresh: HashSet<NoteId> = note_ids(&p, midi_copy).into_iter().collect();
    assert!(f.notes.iter().all(|n| !fresh.contains(n)));
    assert_eq!(track_of(&p, step_copy), f.c);
    assert_eq!(pattern(&p, step_copy), pattern(&p, f.step));
    // Clamped like a move.
    let (p, copies) = ok_with(copy_clips(&f.p, &[f.midi], Tick(-10_000), -3));
    assert_eq!(clip(&p, copies[0]).start, Tick(0));
    assert_eq!(track_of(&p, copies[0]), f.a);
}

#[test]
fn duplicate_clips_behind_themselves() {
    let f = fixture();
    let (p, copies) = ok_with(duplicate_clips(&f.p, &[f.midi, f.step]));
    assert_eq!(clip(&p, copies[0]).start, Tick(7680));
    assert_eq!(track_of(&p, copies[0]), f.a);
    assert_eq!(clip(&p, copies[1]).start, Tick(3840));
    assert_eq!(track_of(&p, copies[1]), f.b);
    assert_eq!(notes(&p, copies[0]), notes(&p, f.midi));
    assert_ne!(note_ids(&p, copies[0]), f.notes);
    assert!(duplicate_clips(&f.p, &[ClipId(999)]).is_err());
}

#[test]
fn clip_length_and_loop() {
    let f = fixture();
    let p = ok(set_clip_length(&f.p, f.midi, Tick(960)));
    let c = clip(&p, f.midi);
    assert_eq!((c.start, c.length), (Tick(3840), Tick(960)));
    assert_eq!(notes(&p, f.midi), notes(&f.p, f.midi), "content is kept");
    assert_eq!(
        clip(&ok(set_clip_length(&f.p, f.midi, Tick(-5))), f.midi).length,
        Tick(1)
    );

    let p = ok(set_clip_loop(&f.p, f.midi, Some(Tick(1920)), Tick(7680)));
    let c = clip(&p, f.midi);
    assert_eq!((c.loop_length, c.length), (Some(Tick(1920)), Tick(7680)));
    let c = clip(
        &ok(set_clip_loop(&p, f.midi, Some(Tick(0)), Tick(0))),
        f.midi,
    )
    .clone();
    assert_eq!((c.loop_length, c.length), (Some(Tick(1)), Tick(1)));
    let c = clip(&ok(set_clip_loop(&p, f.midi, None, Tick(3840))), f.midi).clone();
    assert_eq!((c.loop_length, c.length), (None, Tick(3840)));

    assert_eq!(
        set_clip_loop(&f.p, f.step, Some(Tick(960)), Tick(3840)),
        Err(EditError::WrongClipKind {
            clip: f.step,
            expected: ClipKind::Midi
        })
    );
    let p = ok(set_clip_loop(&f.p, f.step, None, Tick(7680)));
    assert_eq!(clip(&p, f.step).length, Tick(7680));
}

#[test]
fn split_a_midi_clip() {
    let f = fixture();
    let next = f.p.next_id;
    // Notes at 0 (480 long), 480 (240) and 960 (960); split at clip-local 600.
    let (p, rights) = ok_with(split_clips(&f.p, &[f.midi], Tick(3840 + 600)));
    assert_eq!(rights, [ClipId(next)]);
    let (left, right) = (clip(&p, f.midi), clip(&p, rights[0]));
    assert_eq!((left.start, left.length), (Tick(3840), Tick(600)));
    assert_eq!((right.start, right.length), (Tick(4440), Tick(3240)));
    assert_eq!(right.name, left.name);
    assert_eq!(notes(&p, f.midi), [(0, 480, 60, 100), (480, 120, 64, 90)]);
    assert_eq!(note_ids(&p, f.midi), f.notes[..2]);
    assert_eq!(notes(&p, rights[0]), [(360, 960, 67, 80)]);
    assert_ne!(
        note_ids(&p, rights[0])[0],
        f.notes[2],
        "moved notes get new ids"
    );
    assert_eq!(track_of(&p, rights[0]), f.a);
}

#[test]
fn split_skips_clips_not_containing_the_position() {
    let f = fixture();
    for at in [3840, 7680, 100_000] {
        let (p, rights) = ok_with(split_clips(&f.p, &[f.midi], Tick(at)));
        assert!(rights.is_empty());
        assert_eq!(clip(&p, f.midi), clip(&f.p, f.midi));
    }
    assert!(split_clips(&f.p, &[ClipId(999)], Tick(0)).is_err());
}

#[test]
fn split_unrolls_a_looped_clip() {
    let f = fixture();
    // Loop the first 720 ticks (notes at 0 and 480, the second cut to 240) over 1920 ticks.
    let p = ok(set_clip_loop(&f.p, f.midi, Some(Tick(720)), Tick(1920)));
    let (p, rights) = ok_with(split_clips(&p, &[f.midi], Tick(3840 + 1000)));
    let (left, right) = (clip(&p, f.midi), clip(&p, rights[0]));
    assert_eq!((left.loop_length, right.loop_length), (None, None));
    // Repetitions at 0, 720, 1440; the note at 960 (beyond the loop) never played.
    assert_eq!(
        notes(&p, f.midi),
        [(0, 480, 60, 100), (480, 240, 64, 90), (720, 280, 60, 100)]
    );
    assert_eq!(
        notes(&p, rights[0]),
        [(200, 240, 64, 90), (440, 480, 60, 100)],
        "local to the right part: 1200 → 200, 1440 → 440 (cut at 1920)"
    );
}

#[test]
fn split_a_step_clip_keeps_what_plays() {
    let f = fixture();
    let mut p = f.p.clone();
    for (row, step, prop) in [
        (0, 0, StepProp::On(true)),
        (0, 5, StepProp::On(true)),
        (1, 6, StepProp::On(true)),
        (1, 6, StepProp::Ratchet(3)),
        (2, 15, StepProp::On(true)),
    ] {
        p = ok(set_step(&p, f.step, row, step, prop));
    }
    p = ok(set_clip_length(&p, f.step, Tick(2 * 3840)));
    // What sounds: (start, length, note, velocity, probability); step indices differ after the
    // rotation.
    let sound = |hits: Vec<crate::StepHit>, offset: Tick| -> Vec<(Tick, Tick, u8, u8, u8)> {
        hits.into_iter()
            .map(|h| {
                (
                    h.start + offset,
                    h.length,
                    h.note,
                    h.velocity,
                    h.probability,
                )
            })
            .collect()
    };
    let before = sound(
        pattern(&p, f.step).hits(clip(&p, f.step).length),
        Tick::ZERO,
    );
    // Split after 6 steps (an even shift, so swing would agree as well).
    let (q, rights) = ok_with(split_clips(&p, &[f.step], Tick(6 * 240)));
    let (left, right) = (clip(&q, f.step), clip(&q, rights[0]));
    let mut after = sound(pattern(&q, f.step).hits(left.length), Tick::ZERO);
    after.extend(sound(
        pattern(&q, rights[0]).hits(right.length),
        Tick(6 * 240),
    ));
    after.sort_by_key(|&(s, _, n, _, _)| (s, n));
    assert_eq!(after, before);
    assert!(pattern(&q, rights[0]).rows[1].steps[0].on);
    assert_eq!(pattern(&q, rights[0]).rows[1].steps[0].ratchet, 3);
}

#[test]
fn mute_clips() {
    let f = fixture();
    let p = ok(set_clips_muted(&f.p, &[f.midi, f.step], true));
    assert!(clip(&p, f.midi).mute && clip(&p, f.step).mute);
    let q = ok(set_clips_muted(&p, &[f.midi], true));
    assert!(
        Arc::ptr_eq(&q.tracks[0], &p.tracks[0]),
        "unchanged clips stay shared"
    );
    let p = ok(set_clips_muted(&p, &[f.midi], false));
    assert!(!clip(&p, f.midi).mute);
    assert!(set_clips_muted(&f.p, &[ClipId(999)], true).is_err());
}

#[test]
fn clip_props() {
    let f = fixture();
    let set = |prop| set_clip_prop(&f.p, f.midi, prop);
    assert_eq!(
        clip(&ok(set(ClipProp::Name(" Lead A ".into()))), f.midi).name,
        "Lead A"
    );
    assert!(set(ClipProp::Name(String::new())).is_err());
    assert_eq!(
        clip(&ok(set(ClipProp::Transpose(60))), f.midi).transpose,
        48
    );
    assert_eq!(
        clip(&ok(set(ClipProp::Transpose(-12))), f.midi).transpose,
        -12
    );
    assert_eq!(
        clip(&ok(set(ClipProp::VelocityPct(127))), f.midi).velocity_pct,
        100
    );
    assert_eq!(
        clip(&ok(set(ClipProp::VelocityPct(-128))), f.midi).velocity_pct,
        -100
    );
    let (p, early) = ok_with(add_clip(&f.p, f.a, midi_clip(7680, 960, Vec::new())));
    let p = ok(set_clip_prop(&p, f.midi, ClipProp::Start(Tick(9000))));
    let ids: Vec<ClipId> = track(&p, f.a).clips.iter().map(|c| c.id).collect();
    assert_eq!(ids, [early, f.midi], "re-sorted after a start change");
    let p = ok(set_clip_prop(&p, f.midi, ClipProp::Start(Tick(-1))));
    assert_eq!(clip(&p, f.midi).start, Tick(0));
    assert!(set_clip_prop(&f.p, ClipId(999), ClipProp::Transpose(1)).is_err());
}

// ------------------------------------------------------------------------------------------------
// Notes
// ------------------------------------------------------------------------------------------------

#[test]
fn add_and_remove_notes() {
    let f = fixture();
    let next = f.p.next_id;
    let (p, ids) = ok_with(add_notes(
        &f.p,
        f.midi,
        &[nn(240, 240, 62, 0), nn(0, 120, 72, 64)],
    ));
    assert_eq!(ids, [NoteId(next), NoteId(next + 1)]);
    assert_eq!(note(&p, f.midi, ids[0]).velocity, 1);
    assert_eq!(take_notes(&p, f.midi).len(), 5);
    let p = ok(remove_notes(&p, f.midi, &[ids[1], f.notes[0]]));
    assert_eq!(
        notes(&p, f.midi),
        [(240, 240, 62, 1), (480, 240, 64, 90), (960, 960, 67, 80)]
    );
    assert_eq!(
        remove_notes(&f.p, f.midi, &[NoteId(999)]),
        Err(EditError::NoteNotFound(NoteId(999)))
    );
    assert_eq!(
        add_notes(&f.p, f.step, &[nn(0, 1, 60, 100)]),
        Err(EditError::WrongClipKind {
            clip: f.step,
            expected: ClipKind::Midi
        })
    );
}

#[test]
fn move_notes_as_a_group() {
    let f = fixture();
    let p = ok(move_notes(&f.p, f.midi, &f.notes[1..], Tick(240), 12));
    assert_eq!(
        notes(&p, f.midi),
        [(0, 480, 60, 100), (720, 240, 76, 90), (1200, 960, 79, 80)]
    );
    // Clamped by the earliest and the highest selected note.
    let p = ok(move_notes(&f.p, f.midi, &f.notes[1..], Tick(-10_000), 100));
    assert_eq!(
        notes(&p, f.midi)[..2],
        [(0, 480, 60, 100), (0, 240, 124, 90)]
    );
    assert_eq!(
        note(&p, f.midi, f.notes[2]),
        Note {
            start: Tick(480),
            pitch: 127,
            ..note(&f.p, f.midi, f.notes[2])
        }
    );
    let p = ok(move_notes(&f.p, f.midi, &f.notes, Tick(0), -100));
    assert_eq!(notes(&p, f.midi)[0].2, 0);
    assert!(move_notes(&f.p, f.midi, &[NoteId(999)], Tick(0), 1).is_err());
}

#[test]
fn copy_notes_keeps_originals() {
    let f = fixture();
    let next = f.p.next_id;
    let (p, ids) = ok_with(copy_notes(
        &f.p,
        f.midi,
        &[f.notes[2], f.notes[0]],
        Tick(1920),
        -1,
    ));
    assert_eq!(ids, [NoteId(next), NoteId(next + 1)]);
    assert_eq!(note(&p, f.midi, ids[0]).start, Tick(2880));
    assert_eq!(note(&p, f.midi, ids[1]).pitch, 59);
    assert_eq!(take_notes(&p, f.midi).len(), 5);
    for id in &f.notes {
        assert_eq!(note(&p, f.midi, *id), note(&f.p, f.midi, *id));
    }
}

#[test]
fn resize_notes_together() {
    let f = fixture();
    let p = ok(resize_notes(&f.p, f.midi, &f.notes, Tick(-300)));
    assert_eq!(
        notes(&p, f.midi).iter().map(|n| n.1).collect::<Vec<_>>(),
        [180, 1, 660]
    );
    let p = ok(resize_notes(&f.p, f.midi, &f.notes[..1], Tick(240)));
    assert_eq!(notes(&p, f.midi)[0].1, 720);
}

#[test]
fn note_velocities() {
    let f = fixture();
    let p = ok(set_notes_velocity(
        &f.p,
        f.midi,
        &f.notes,
        VelocityChange::Set(0),
    ));
    assert!(take_notes(&p, f.midi).iter().all(|n| n.velocity == 1));
    let p = ok(set_notes_velocity(
        &f.p,
        f.midi,
        &f.notes,
        VelocityChange::Offset(30),
    ));
    assert_eq!(
        notes(&p, f.midi).iter().map(|n| n.3).collect::<Vec<_>>(),
        [127, 120, 110]
    );
    let p = ok(set_notes_velocity(
        &f.p,
        f.midi,
        &f.notes[2..],
        VelocityChange::Offset(-500),
    ));
    assert_eq!(notes(&p, f.midi)[2].3, 1);
}

#[test]
fn quantize_like_the_prototype() {
    let f = fixture();
    let (p, ids) = ok_with(add_notes(
        &f.p,
        f.midi,
        &[nn(130, 50, 50, 100), nn(355, 370, 51, 100)],
    ));
    // Grid 1/8 (480): 130 → 0, 355 → 480; lengths 50 → 480 (at least one grid), 370 → 480.
    let p = ok(quantize_notes(&p, f.midi, Some(&ids), Tick(480)));
    assert_eq!(note(&p, f.midi, ids[0]).start, Tick(0));
    assert_eq!(note(&p, f.midi, ids[0]).length, Tick(480));
    assert_eq!(note(&p, f.midi, ids[1]).start, Tick(480));
    assert_eq!(note(&p, f.midi, ids[1]).length, Tick(480));
    assert_eq!(
        note(&p, f.midi, f.notes[1]).length,
        Tick(240),
        "not selected"
    );
    // All notes at 1/4: halves round up (480 → 960).
    let p = ok(quantize_notes(&f.p, f.midi, None, Tick(960)));
    assert_eq!(
        notes(&p, f.midi),
        [(0, 960, 60, 100), (960, 960, 64, 90), (960, 960, 67, 80)]
    );
    assert_eq!(
        quantize_notes(&f.p, f.midi, None, Tick(0)),
        Err(EditError::Invalid("grid must be positive"))
    );
}

#[test]
fn note_edits_work_on_the_active_take() {
    let f = fixture();
    let (p, take) = ok_with(add_take(&f.p, f.midi, "Take 2".into()));
    assert_eq!(take, 1);
    assert!(take_notes(&p, f.midi).is_empty());
    let (p, ids) = ok_with(add_notes(&p, f.midi, &[nn(0, 240, 60, 100)]));
    assert_eq!(
        remove_notes(&p, f.midi, &f.notes[..1]),
        Err(EditError::NoteNotFound(f.notes[0]))
    );
    let p = ok(select_take(&p, f.midi, 0));
    assert_eq!(note_ids(&p, f.midi), f.notes);
    let p = ok(select_take(&p, f.midi, 1));
    assert_eq!(note_ids(&p, f.midi), ids);
}

// ------------------------------------------------------------------------------------------------
// Steps
// ------------------------------------------------------------------------------------------------

#[test]
fn step_values() {
    let f = fixture();
    let set = |prop| set_step(&f.p, f.step, 3, 31, prop);
    let step = |p: &Project| pattern(p, f.step).rows[3].steps[31];
    let p = ok(set_step(&f.p, f.step, 3, 31, StepProp::Velocity(70)));
    let p = ok(set_step(&p, f.step, 3, 31, StepProp::On(true)));
    assert_eq!(
        (step(&p).on, step(&p).velocity),
        (true, 70),
        "on keeps the velocity"
    );
    assert_eq!(step(&ok(set(StepProp::Velocity(0)))).velocity, 1);
    assert_eq!(step(&ok(set(StepProp::Probability(150)))).probability, 100);
    assert_eq!(step(&ok(set(StepProp::Ratchet(0)))).ratchet, 1);
    assert_eq!(step(&ok(set(StepProp::Ratchet(9)))).ratchet, 4);
    assert_eq!(
        set_step(&f.p, f.step, 8, 0, StepProp::On(true)),
        Err(EditError::IndexOutOfRange {
            what: "step row",
            index: 8
        })
    );
    assert_eq!(
        set_step(&f.p, f.step, 0, 32, StepProp::On(true)),
        Err(EditError::IndexOutOfRange {
            what: "step",
            index: 32
        })
    );
    assert_eq!(
        set_step(&f.p, f.midi, 0, 0, StepProp::On(true)),
        Err(EditError::WrongClipKind {
            clip: f.midi,
            expected: ClipKind::Step
        })
    );
}

#[test]
fn doubling_the_length_copies_the_pattern() {
    let f = fixture();
    let p = ok(set_step(&f.p, f.step, 0, 4, StepProp::On(true)));
    let p = ok(set_step(&p, f.step, 0, 4, StepProp::Ratchet(2)));
    let doubled = ok(set_step_clip_prop(
        &p,
        f.step,
        StepClipProp::LengthSteps(32),
    ));
    let row = &pattern(&doubled, f.step).rows[0];
    assert_eq!(pattern(&doubled, f.step).length_steps, 32);
    assert_eq!(row.steps[20], row.steps[4]);
    assert!(row.steps[20].on && row.steps[20].ratchet == 2);
    // Back to 16 keeps the hidden steps.
    let halved = ok(set_step_clip_prop(
        &doubled,
        f.step,
        StepClipProp::LengthSteps(16),
    ));
    assert!(pattern(&halved, f.step).rows[0].steps[20].on);
    // With a hidden step already on, doubling copies nothing.
    let p = ok(set_step(&p, f.step, 5, 30, StepProp::On(true)));
    let doubled = ok(set_step_clip_prop(
        &p,
        f.step,
        StepClipProp::LengthSteps(32),
    ));
    assert!(!pattern(&doubled, f.step).rows[0].steps[20].on);
    // Not a doubling: nothing copied either.
    let p = ok(set_step_clip_prop(
        &f.p,
        f.step,
        StepClipProp::LengthSteps(8),
    ));
    let p = ok(set_step(&p, f.step, 0, 1, StepProp::On(true)));
    let longer = ok(set_step_clip_prop(
        &p,
        f.step,
        StepClipProp::LengthSteps(12),
    ));
    assert!(!pattern(&longer, f.step).rows[0].steps[9].on);
}

#[test]
fn pattern_props() {
    let f = fixture();
    let set = |prop| set_step_clip_prop(&f.p, f.step, prop);
    assert_eq!(
        pattern(&ok(set(StepClipProp::LengthSteps(0))), f.step).length_steps,
        1
    );
    assert_eq!(
        pattern(&ok(set(StepClipProp::LengthSteps(64))), f.step).length_steps,
        32
    );
    let p = ok(set(StepClipProp::Resolution(StepResolution::EighthTriplet)));
    assert_eq!(
        pattern(&p, f.step).resolution,
        StepResolution::EighthTriplet
    );
    assert_eq!(
        pattern(&ok(set(StepClipProp::Swing(0.12))), f.step).swing,
        0.12
    );
    assert_eq!(
        pattern(&ok(set(StepClipProp::Swing(2.0))), f.step).swing,
        0.75
    );
    assert_eq!(
        pattern(&ok(set(StepClipProp::Swing(-1.0))), f.step).swing,
        0.0
    );
    assert!(set(StepClipProp::Swing(f32::NAN)).is_err());
    assert!(set_step_clip_prop(&f.p, f.midi, StepClipProp::Swing(0.1)).is_err());
}

#[test]
fn step_row_props() {
    let f = fixture();
    let set = |prop| set_step_row(&f.p, f.step, 2, prop);
    let row = |p: &Project| pattern(p, f.step).rows[2].clone();
    assert_eq!(
        row(&ok(set(StepRowProp::Name(" Clap 2 ".into())))).name,
        "Clap 2"
    );
    assert!(set(StepRowProp::Name(String::new())).is_err());
    assert_eq!(row(&ok(set(StepRowProp::Note(200)))).note, 127);
    assert!(row(&ok(set(StepRowProp::Mute(true)))).mute);
    assert!(row(&ok(set(StepRowProp::Solo(true)))).solo);
    assert!(set_step_row(&f.p, f.step, 8, StepRowProp::Mute(true)).is_err());
}

#[test]
fn replace_step_rows_by_index() {
    let f = fixture();
    let p = ok(set_step(&f.p, f.step, 1, 4, StepProp::On(true)));
    let p = ok(set_step_row(&p, f.step, 1, StepRowProp::Mute(true)));
    let specs: Vec<StepRowSpec> = (0..3)
        .map(|i| StepRowSpec {
            name: format!("Pad {}", i + 1),
            note: 36 + i,
        })
        .collect();
    let q = ok(set_step_rows(&p, f.step, &specs));
    let rows = &pattern(&q, f.step).rows;
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[1].name.as_str(), rows[1].note), ("Pad 2", 37));
    assert!(
        rows[1].steps[4].on && rows[1].mute,
        "steps and flags stay by index"
    );
    let mut more = specs.clone();
    more.push(StepRowSpec {
        name: "Pad 4".into(),
        note: 39,
    });
    let q = ok(set_step_rows(&q, f.step, &more));
    let rows = &pattern(&q, f.step).rows;
    assert_eq!(rows.len(), 4);
    assert!(rows[3].steps.iter().all(|s| *s == Step::default()));
}

#[test]
fn step_clip_becomes_a_midi_clip() {
    let p = demo_project();
    let beat_b = p.tracks[0].clips[2].id;
    let before = clip(&p, beat_b).clone();
    let q = ok(step_clip_to_midi(&p, beat_b));
    let c = clip(&q, beat_b);
    assert_eq!(
        (c.id, &c.name, c.start, c.length),
        (before.id, &before.name, before.start, before.length)
    );
    assert_eq!(c.kind(), ClipKind::Midi);
    // Beat B has 21 hits per bar, four bars.
    let n = notes(&q, beat_b);
    assert_eq!(n.len(), 4 * 21);
    assert_eq!(
        &n[..3],
        [(0, 240, 36, 120), (0, 240, 39, 90), (480, 240, 39, 50)]
    );
    assert_eq!(
        n.iter()
            .filter(|x| x.0 == 3840 + 3 * 240)
            .collect::<Vec<_>>(),
        [&(3840 + 720, 240, 40, 70)]
    );
    assert!(q.next_id == p.next_id + 84);
    assert_eq!(
        step_clip_to_midi(&q, beat_b),
        Err(EditError::WrongClipKind {
            clip: beat_b,
            expected: ClipKind::Step
        })
    );
}

#[test]
fn step_conversion_applies_swing_ratchets_rows_and_probability() {
    let f = fixture();
    let mut p = f.p.clone();
    for (row, step, prop) in [
        (0, 0, StepProp::On(true)),
        (0, 1, StepProp::On(true)),
        (0, 1, StepProp::Ratchet(3)),
        (1, 2, StepProp::On(true)),
        (1, 2, StepProp::Probability(0)),
        (2, 3, StepProp::On(true)),
        (2, 3, StepProp::Probability(25)),
        (3, 0, StepProp::On(true)),
    ] {
        p = ok(set_step(&p, f.step, row, step, prop));
    }
    p = ok(set_step_row(&p, f.step, 3, StepRowProp::Mute(true)));
    p = ok(set_step_clip_prop(&p, f.step, StepClipProp::Swing(0.5)));
    p = ok(set_step_clip_prop(&p, f.step, StepClipProp::LengthSteps(4)));
    p = ok(set_clip_length(&p, f.step, Tick(5 * 240 + 100)));
    let q = ok(step_clip_to_midi(&p, f.step));
    // Swing 0.5 delays odd steps by 60 ticks; step 1 has three hits of 80 ticks, later ones at 75 %.
    // Row 1 (probability 0) and row 3 (muted) are left out. The pattern (4 steps) repeats at 960;
    // its step 1 (delayed to 1260) is cut at the clip end (1300) after its first hit.
    assert_eq!(
        notes(&q, f.step),
        [
            (0, 240, 36, 100),
            (300, 80, 36, 100),
            (380, 80, 36, 75),
            (460, 80, 36, 75),
            (780, 240, 39, 100),
            (960, 240, 36, 100),
            (1260, 40, 36, 100),
        ]
    );
    // A swung step that starts at or after the clip end is left out.
    let p = ok(set_clip_length(&p, f.step, Tick(1250)));
    let q = ok(step_clip_to_midi(&p, f.step));
    assert_eq!(notes(&q, f.step).last(), Some(&(960, 240, 36, 100)));
    // Solo: only soloed rows.
    let p = ok(set_step_row(&p, f.step, 2, StepRowProp::Solo(true)));
    let q = ok(step_clip_to_midi(&p, f.step));
    assert_eq!(notes(&q, f.step), [(780, 240, 39, 100)]);
}

// ------------------------------------------------------------------------------------------------
// Devices
// ------------------------------------------------------------------------------------------------

#[test]
fn find_devices() {
    let f = fixture();
    let (channel, index, device) = f.p.find_device(f.eq).unwrap();
    assert_eq!(
        (channel, index, device.id),
        (ChannelRef::Track(f.a), 1, f.eq)
    );
    let (p, limiter) = ok_with(insert_device(
        &f.p,
        ChannelRef::Master,
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("Limiter"),
            state: None,
            sidechain: None,
        },
    ));
    assert_eq!(
        p.find_device(limiter).map(|(c, i, _)| (c, i)),
        Some((ChannelRef::Master, 0))
    );
    assert!(p.find_device(DeviceId(999)).is_none());
}

#[test]
fn insert_devices() {
    let f = fixture();
    let effect = |name: &str, state: Option<Arc<[u8]>>| NewDevice {
        role: DeviceRole::Effect,
        plugin: plugin(name),
        state,
        sidechain: None,
    };
    let instrument = |name: &str| NewDevice {
        role: DeviceRole::Instrument,
        plugin: plugin(name),
        state: None,
        sidechain: None,
    };
    // An effect never goes before the instrument; the index is clamped to the end.
    let state: Arc<[u8]> = Arc::from(&b"preset"[..]);
    let (p, hall) = ok_with(insert_device(
        &f.p,
        ChannelRef::Track(f.a),
        0,
        effect("Hall", Some(state.clone())),
    ));
    assert_eq!(
        chain(&p, f.a),
        [
            ("Synth".into(), DeviceRole::Instrument),
            ("Hall".into(), DeviceRole::Effect),
            ("EQ".into(), DeviceRole::Effect)
        ]
    );
    let d = p.find_device(hall).unwrap().2;
    assert_eq!((d.bypass, d.state.clone()), (false, Some(state)));
    let (p, _) = ok_with(insert_device(
        &p,
        ChannelRef::Track(f.a),
        99,
        effect("Delay", None),
    ));
    assert_eq!(chain(&p, f.a).last().unwrap().0, "Delay");
    // Instruments: only first, only one.
    assert_eq!(
        insert_device(&f.p, ChannelRef::Track(f.a), 0, instrument("Other")),
        Err(EditError::Invalid("the track already has an instrument"))
    );
    let (p, _) = ok_with(insert_device(
        &f.p,
        ChannelRef::Track(f.b),
        0,
        effect("EQ", None),
    ));
    assert_eq!(
        insert_device(&p, ChannelRef::Track(f.b), 1, instrument("Synth")),
        Err(EditError::Invalid(
            "an instrument goes right behind the MIDI effects"
        ))
    );
    let (p, _) = ok_with(insert_device(
        &p,
        ChannelRef::Track(f.b),
        0,
        instrument("Synth"),
    ));
    assert_eq!(chain(&p, f.b)[0], ("Synth".into(), DeviceRole::Instrument));
    assert_eq!(
        insert_device(&f.p, ChannelRef::Master, 0, instrument("Synth")),
        Err(EditError::Invalid("the master takes effects only"))
    );
    assert!(insert_device(&f.p, ChannelRef::Track(TrackId(999)), 0, effect("EQ", None)).is_err());
}

/// WP-88: a track chain is `[MIDI effects…] → instrument → [effects…]`, whatever order the
/// inserts, moves and instrument replacements come in.
#[test]
fn midi_effects_stay_in_front_of_the_instrument() {
    let f = fixture();
    let spec = |role, name: &str| NewDevice {
        role,
        plugin: plugin(name),
        state: None,
        sidechain: None,
    };
    let names = |p: &Project, t| -> Vec<String> { chain(p, t).into_iter().map(|c| c.0).collect() };
    // Dropped right of the instrument, a MIDI effect still goes into the MIDI part.
    let (p, arp) = ok_with(insert_device(
        &f.p,
        ChannelRef::Track(f.a),
        2,
        spec(DeviceRole::MidiEffect, "Arp"),
    ));
    assert_eq!(names(&p, f.a), ["Arp", "Synth", "EQ"]);
    let (p, scale) = ok_with(insert_device(
        &p,
        ChannelRef::Track(f.a),
        0,
        spec(DeviceRole::MidiEffect, "Scale"),
    ));
    assert_eq!(names(&p, f.a), ["Scale", "Arp", "Synth", "EQ"]);
    // An effect never goes into the MIDI part or before the instrument.
    let (p, _) = ok_with(insert_device(
        &p,
        ChannelRef::Track(f.a),
        0,
        spec(DeviceRole::Effect, "Hall"),
    ));
    assert_eq!(names(&p, f.a), ["Scale", "Arp", "Synth", "Hall", "EQ"]);
    // Reordering stays inside the part.
    let moved = ok(move_device(&p, scale, 1));
    assert_eq!(names(&moved, f.a), ["Arp", "Scale", "Synth", "Hall", "EQ"]);
    assert!(move_device(&p, arp, 3).is_err());
    assert!(move_device(&p, arp, 2).is_err());
    let hall = chain_ids(&p, f.a)[3];
    assert!(move_device(&p, hall, 1).is_err());
    assert_eq!(
        move_device(&p, f.synth, 0),
        Err(EditError::Invalid("an instrument cannot move"))
    );
    // The instrument is replaced in place, or inserted right behind the MIDI effects.
    let (q, _) = ok_with(replace_instrument(&p, f.a, plugin("Other")));
    assert_eq!(names(&q, f.a), ["Scale", "Arp", "Other", "Hall", "EQ"]);
    let q = ok(remove_device(&q, chain_ids(&q, f.a)[2]));
    let (q, _) = ok_with(replace_instrument(&q, f.a, plugin("Back")));
    assert_eq!(names(&q, f.a), ["Scale", "Arp", "Back", "Hall", "EQ"]);
    // An instrument may only be inserted right behind them; the master takes audio effects only.
    let q = ok(remove_device(&q, chain_ids(&q, f.a)[2]));
    assert!(
        insert_device(
            &q,
            ChannelRef::Track(f.a),
            3,
            spec(DeviceRole::Instrument, "X")
        )
        .is_err()
    );
    assert!(
        insert_device(
            &q,
            ChannelRef::Track(f.a),
            2,
            spec(DeviceRole::Instrument, "X")
        )
        .is_ok()
    );
    assert_eq!(
        insert_device(
            &p,
            ChannelRef::Master,
            0,
            spec(DeviceRole::MidiEffect, "Arp")
        ),
        Err(EditError::Invalid("the master takes effects only"))
    );
    // The helpers read the same order.
    let channel = &track(&p, f.a).channel;
    assert_eq!(channel.midi_effect_count(), 2);
    assert_eq!(
        channel.instrument().map(|(i, d)| (i, d.id)),
        Some((2, f.synth))
    );
    assert_eq!(channel.first_effect_index(), 3);
}

#[test]
fn replace_or_insert_the_instrument() {
    let f = fixture();
    let p = ok(set_device_state(&f.p, f.synth, Some(Arc::from(&b"x"[..]))));
    let (p, id) = ok_with(replace_instrument(&p, f.a, sampler()));
    assert_ne!(id, f.synth);
    let d = &track(&p, f.a).channel.devices[0];
    assert_eq!(
        (d.id, d.plugin.name.as_str(), d.state.clone()),
        (id, "Sampler", None)
    );
    assert_eq!(track(&p, f.a).channel.devices.len(), 2);
    let (p, _) = ok_with(insert_device(
        &p,
        ChannelRef::Track(f.b),
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("EQ"),
            state: None,
            sidechain: None,
        },
    ));
    let (p, id) = ok_with(replace_instrument(&p, f.b, plugin("Synth")));
    assert_eq!(
        chain(&p, f.b),
        [
            ("Synth".into(), DeviceRole::Instrument),
            ("EQ".into(), DeviceRole::Effect)
        ]
    );
    assert_eq!(track(&p, f.b).channel.devices[0].id, id);
    assert!(replace_instrument(&p, TrackId(999), sampler()).is_err());
}

#[test]
fn remove_and_move_devices() {
    let f = fixture();
    let p = ok(remove_device(&f.p, f.synth));
    assert_eq!(chain(&p, f.a), [("EQ".into(), DeviceRole::Effect)]);
    assert_eq!(
        remove_device(&f.p, DeviceId(999)),
        Err(EditError::DeviceNotFound(DeviceId(999)))
    );

    let (p, hall) = ok_with(insert_device(
        &f.p,
        ChannelRef::Track(f.a),
        2,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("Hall"),
            state: None,
            sidechain: None,
        },
    ));
    let q = ok(move_device(&p, hall, 1));
    assert_eq!(chain(&q, f.a)[1].0, "Hall");
    let q = ok(move_device(&q, hall, 99));
    assert_eq!(chain(&q, f.a)[2].0, "Hall");
    assert_eq!(
        move_device(&p, hall, 0),
        Err(EditError::Invalid(
            "an effect cannot go before the instrument"
        ))
    );
    assert_eq!(
        move_device(&p, f.synth, 1),
        Err(EditError::Invalid("an instrument cannot move"))
    );
    // Without an instrument, index 0 is fine.
    let p = ok(remove_device(&p, f.synth));
    assert_eq!(chain(&ok(move_device(&p, hall, 0)), f.a)[0].0, "Hall");
}

#[test]
fn device_bypass_and_state() {
    let f = fixture();
    let p = ok(set_device_bypass(&f.p, f.eq, true));
    assert!(p.find_device(f.eq).unwrap().2.bypass);
    let state: Arc<[u8]> = Arc::from(&[0u8, 1, 2, 255][..]);
    let p = ok(set_device_state(&p, f.eq, Some(state.clone())));
    assert_eq!(p.find_device(f.eq).unwrap().2.state, Some(state));
    let p = ok(set_device_state(&p, f.eq, None));
    assert_eq!(p.find_device(f.eq).unwrap().2.state, None);
    assert!(set_device_bypass(&f.p, DeviceId(999), true).is_err());
    assert!(Arc::ptr_eq(&p.tracks[1], &f.p.tracks[1]));
}

// ------------------------------------------------------------------------------------------------
// Takes
// ------------------------------------------------------------------------------------------------

#[test]
fn takes() {
    let f = fixture();
    let (p, index) = ok_with(add_take(&f.p, f.midi, "Take 1".into()));
    let ClipContent::Midi(m) = &clip(&p, f.midi).content else {
        panic!("MIDI clip");
    };
    assert_eq!((index, m.active_take, m.takes.len()), (1, 1, 2));
    assert_eq!(m.active().name, "Take 1");
    assert_eq!(
        select_take(&p, f.midi, 2),
        Err(EditError::IndexOutOfRange {
            what: "take",
            index: 2
        })
    );
    assert!(add_take(&f.p, f.step, "Take 1".into()).is_err());
    assert!(select_take(&f.p, f.step, 0).is_err());
}

#[test]
fn edits_on_the_demo_keep_invariants() {
    check_invariants(&demo_project());
    check_invariants(&crate::fixtures::empty_project());
}
