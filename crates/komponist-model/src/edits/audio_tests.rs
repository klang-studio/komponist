//! Unit tests of the audio edits (epic #8, #9): track kinds, the overlap-cut rule, trimming,
//! tempo changes, input selection and the routing it adds.

use std::path::PathBuf;

use super::tests::check_invariants;
use super::*;
use crate::audio::{AudioFile, AudioIo, CLIP_GAIN_DB_MAX, ClipTimeMode, HardwareInput};
use crate::document::Document;
use crate::routing;

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

/// A 10 s stereo file at 48 kHz: 5 bars at 120 bpm (19 200 ticks).
fn file() -> AudioFile {
    AudioFile {
        abs: PathBuf::from("/t/loop.wav"),
        rel: PathBuf::new(),
        frames: 480_000,
        sample_rate: 48_000,
        channels: 2,
    }
}

fn new_audio(start: i64, length: i64) -> NewClip {
    NewClip {
        start: Tick(start),
        length: Tick(length),
        name: "Loop".to_owned(),
        content: NewClipContent::Audio {
            file: file(),
            offset: 0,
        },
    }
}

fn midi(start: i64, length: i64) -> NewClip {
    NewClip {
        start: Tick(start),
        length: Tick(length),
        name: "MIDI".to_owned(),
        content: NewClipContent::Midi {
            notes: Vec::new(),
            take_name: String::new(),
        },
    }
}

fn audio_track(p: &Project) -> (Project, TrackId) {
    ok_with(add_audio_track(p, NewAudioTrack::default()))
}

fn midi_track(p: &Project) -> (Project, TrackId) {
    ok_with(add_track(
        p,
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    ))
}

/// `(id, start, length, offset)` of the clips of `track`.
fn layout(p: &Project, track: TrackId) -> Vec<(ClipId, i64, i64, u64)> {
    p.track(track)
        .unwrap()
        .clips
        .iter()
        .map(|c| {
            let ClipContent::Audio(a) = &c.content else {
                panic!("not an audio clip");
            };
            (c.id, c.start.0, c.length.0, a.offset)
        })
        .collect()
}

fn audio_of(p: &Project, id: ClipId) -> &crate::AudioClip {
    match &p.find_clip(id).unwrap().1.content {
        ClipContent::Audio(a) => a,
        _ => panic!("not an audio clip"),
    }
}

fn bus(p: &Project) -> (Project, TrackId) {
    ok_with(add_bus(p, NewBus::default()))
}

#[test]
fn flattening_carries_clip_mixer_curves_to_the_existing_arrangement_lanes() {
    use crate::automation::{
        AutomationMode, AutomationOwner, AutomationTarget, Point, Points, effective_value_at,
    };
    use crate::edits::automation::{add_clip_automation, set_clip_points, set_lane_mode};
    use crate::edits::macros::{add_mapping, set_macro_value};
    use crate::macros::Mapping;
    use crate::params::ParamId;
    use crate::params::db_to_p;

    let (p, id) = midi_track(&Project::new("Flatten"));
    let p = ok(set_track_prop(
        &p,
        &[id],
        TrackProp::Channel(ChannelProp::VolumeDb(-12.0)),
    ));
    let p = ok(set_track_prop(
        &p,
        &[id],
        TrackProp::Channel(ChannelProp::Pan(-0.5)),
    ));
    let (p, bus) = bus(&p);
    let (p, send) = ok_with(add_send(&p, id, bus));
    let p = ok(set_send(&p, send, SendProp::LevelDb(-24.0)));
    let instrument = PluginRef {
        format: PluginFormat::Clap,
        id: "test.synth".into(),
        name: "Synth".into(),
        vendor: String::new(),
        version: String::new(),
        module: Box::default(),
    };
    let (p, device) = ok_with(replace_instrument(&p, id, instrument));
    let param = AutomationTarget::Param {
        device,
        param: ParamId::plugin(7),
    };
    let mapping = |target| Mapping {
        target,
        label: String::new(),
        min: 0.25,
        max: 1.0,
        invert: true,
        bend: 0.3,
    };
    let p = ok(add_mapping(&p, id, 0, mapping(param.clone())));
    let p = ok(add_mapping(&p, id, 0, mapping(AutomationTarget::Pan)));
    let p = ok(set_macro_value(&p, id, 0, 0.5));
    let (p, clip) = ok_with(add_clip(&p, id, midi(240, 1200)));
    let mut p = ok(set_clip_loop(&p, clip, Some(Tick(600)), Tick(1200)));
    let owner = AutomationOwner::Track(id);
    let lanes = [
        (
            AutomationTarget::Volume,
            AutomationMode::Touch,
            1.0 - db_to_p(-12.0),
        ),
        (AutomationTarget::Pan, AutomationMode::Off, 0.25),
        (
            AutomationTarget::Send { send },
            AutomationMode::Latch,
            1.0 - db_to_p(-24.0),
        ),
        (
            AutomationTarget::Macro { index: 0 },
            AutomationMode::Read,
            0.5,
        ),
    ];
    for (target, mode, _) in &lanes {
        p = ok(add_clip_automation(&p, clip, target.clone(), String::new()));
        p = ok(set_clip_points(
            &p,
            clip,
            target,
            Points::new(vec![
                Point::linear(Tick::ZERO, 0.4),
                Point::linear(Tick(600), 0.9),
            ]),
        ));
        let lane = p.track(id).unwrap().automation.lane(target).unwrap().id;
        p = ok(set_lane_mode(&p, owner, lane, *mode));
    }

    let (flattened, _) = ok_with(flatten_midi_track(&p, id, Tick(240), file()));
    let audio = flattened.track(id).unwrap();
    assert!(audio.is_audio());
    assert!(audio.channel.devices.is_empty());
    assert_eq!(audio.macros.driver(&param), None);
    assert_eq!(audio.macros.driver(&AutomationTarget::Pan), Some(0));
    assert_eq!(audio.sends, p.track(id).unwrap().sends);
    assert!(audio.clips.iter().all(|clip| clip.automation.is_empty()));
    for (target, mode, base) in &lanes {
        let original = p.track(id).unwrap().automation.lane(target).unwrap();
        let retained = audio.automation.lane(target).unwrap();
        assert_eq!(retained.id, original.id);
        assert_eq!(retained.mode, *mode);
        // Compare both curves with playback enabled, including the previously disabled pan lane.
        let before = ok(set_lane_mode(&p, owner, original.id, AutomationMode::Read));
        let after = ok(set_lane_mode(
            &flattened,
            owner,
            retained.id,
            AutomationMode::Read,
        ));
        for tick in [0, 239, 240, 619, 839, 840, 1439, 1440, 3840].map(Tick) {
            let expected =
                effective_value_at(before.track(id).unwrap(), target, tick).unwrap_or(*base);
            let actual = effective_value_at(after.track(id).unwrap(), target, tick).unwrap();
            assert!(
                (actual - expected).abs() < 1e-6,
                "{target:?} at {tick:?}: {actual} vs {expected}"
            );
        }
    }
}

#[test]
fn flattening_accepts_a_loop_whose_actual_point_count_fits() {
    use crate::automation::{AutomationTarget, Point, Points};
    use crate::edits::automation::{add_clip_automation, set_clip_points};

    let (p, id) = midi_track(&Project::new("Flatten"));
    let (p, clip) = ok_with(add_clip(&p, id, midi(240, 1200)));
    let p = ok(set_clip_loop(&p, clip, Some(Tick(2)), Tick(400_000)));
    let target = AutomationTarget::Pan;
    let p = ok(add_clip_automation(&p, clip, target.clone(), String::new()));
    let p = ok(set_clip_points(
        &p,
        clip,
        &target,
        Points::new(vec![
            Point::linear(Tick::ZERO, 0.4),
            Point::linear(Tick(2), 0.9),
        ]),
    ));
    let track = p.track(id).unwrap();
    assert!(crate::automation::clip_expansion_points(track, &target) > 1_000_000);
    assert_eq!(validate_flatten_automation(track), Ok(()));
    let (flattened, _) = ok_with(flatten_midi_track(&p, id, Tick(240), file()));
    let lane = flattened
        .track(id)
        .unwrap()
        .automation
        .lane(&target)
        .unwrap();
    assert!(lane.points.as_slice().len() < 1_000_000);
}

#[test]
fn flattening_refuses_to_expand_an_enormous_disabled_modulating_loop() {
    use crate::automation::{AutomationMode, AutomationOwner, AutomationTarget, Point, Points};
    use crate::edits::automation::{add_clip_automation, set_clip_points, set_lane_mode};

    let (p, id) = midi_track(&Project::new("Flatten"));
    let (p, clip) = ok_with(add_clip(&p, id, midi(240, 1200)));
    let p = ok(set_clip_loop(&p, clip, Some(Tick(2)), Tick(60_000_000)));
    let target = AutomationTarget::Pan;
    let p = ok(add_clip_automation(&p, clip, target.clone(), String::new()));
    let lane = p.track(id).unwrap().automation.lane(&target).unwrap().id;
    let p = ok(set_lane_mode(
        &p,
        AutomationOwner::Track(id),
        lane,
        AutomationMode::Off,
    ));

    // Constant values have a compact representation even with this many repetitions.
    let p = ok(set_clip_points(
        &p,
        clip,
        &target,
        Points::new(vec![Point::linear(Tick::ZERO, 0.4)]),
    ));
    assert_eq!(validate_flatten_automation(p.track(id).unwrap()), Ok(()));
    let p = ok(set_clip_points(
        &p,
        clip,
        &target,
        Points::new(vec![
            Point::linear(Tick::ZERO, 0.4),
            Point::linear(Tick(1), 0.9),
        ]),
    ));
    assert_eq!(
        split_clips(&p, &[clip], Tick(30_000_240)),
        Err(EditError::AutomationExpansionTooLarge),
    );
    let mut doc = Document::new(p);
    let before = doc.project().clone();
    assert_eq!(
        validate_flatten_automation(before.track(id).unwrap()),
        Err(EditError::AutomationExpansionTooLarge),
    );
    assert_eq!(
        doc.apply_with("flatten", |p| flatten_midi_track(p, id, Tick(240), file())),
        Err(EditError::AutomationExpansionTooLarge),
    );
    assert!(Arc::ptr_eq(doc.project(), &before));
    assert!(!doc.undo());
}

// --- Tracks and kinds -------------------------------------------------------------------------

#[test]
fn a_new_audio_track_has_defaults() {
    let (p, t) = audio_track(&Project::new("T"));
    let track = p.track(t).unwrap();
    assert_eq!(track.kind, TrackKind::Audio);
    assert_eq!(track.name, "Audio");
    assert_eq!(track.audio, AudioIo::default());
    assert_eq!(track.audio.monitor, MonitorMode::Off);
    assert_eq!(track.output, TrackOutput::Master);
    let (p, t2) = audio_track(&p);
    assert_eq!(p.track(t2).unwrap().name, "Audio 2");
}

#[test]
fn clips_go_only_on_tracks_of_their_kind() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, m) = midi_track(&p);
    let (p, b) = bus(&p);
    let refused = |r: Result<(Project, ClipId), EditError>, track, clip| {
        assert_eq!(r.unwrap_err(), EditError::ClipNotAllowed { track, clip });
    };
    refused(add_clip(&p, a, midi(0, 960)), a, ClipKind::Midi);
    refused(
        add_clip(
            &p,
            a,
            NewClip {
                content: NewClipContent::Step { rows: Vec::new() },
                ..midi(0, 960)
            },
        ),
        a,
        ClipKind::Step,
    );
    refused(add_clip(&p, m, new_audio(0, 960)), m, ClipKind::Audio);
    assert!(add_clip(&p, b, new_audio(0, 960)).is_err());
    let (p, clip) = ok_with(add_clip(&p, a, new_audio(0, 960)));
    // Moving between kinds is refused too, in both directions.
    let (p, mc) = ok_with(add_clip(&p, m, midi(0, 960)));
    assert_eq!(
        move_clips(&p, &[clip], Tick(0), 1).unwrap_err(),
        EditError::ClipNotAllowed {
            track: m,
            clip: ClipKind::Audio
        }
    );
    assert_eq!(
        move_clips(&p, &[mc], Tick(0), -1).unwrap_err(),
        EditError::ClipNotAllowed {
            track: a,
            clip: ClipKind::Midi
        }
    );
}

#[test]
fn an_audio_track_takes_audio_effects_only() {
    let (p, a) = audio_track(&Project::new("T"));
    let plugin = |role| NewDevice {
        role,
        plugin: crate::PluginRef {
            format: crate::PluginFormat::Clap,
            id: "x".into(),
            name: "X".into(),
            vendor: String::new(),
            version: String::new(),
            module: Box::default(),
        },
        state: None,
        sidechain: None,
        preset: None,
    };
    for role in [DeviceRole::Instrument, DeviceRole::MidiEffect] {
        assert!(insert_device(&p, ChannelRef::Track(a), 0, plugin(role)).is_err());
    }
    ok_with(insert_device(
        &p,
        ChannelRef::Track(a),
        0,
        plugin(DeviceRole::Effect),
    ));
    assert!(replace_instrument(&p, a, plugin(DeviceRole::Instrument).plugin).is_err());
}

// --- Clips ------------------------------------------------------------------------------------

#[test]
fn an_audio_clip_is_clamped_to_its_file_and_validated() {
    let (p, a) = audio_track(&Project::new("T"));
    // The file lasts 19 200 ticks at 120 bpm.
    let (p, id) = ok_with(add_clip(&p, a, new_audio(0, 100_000)));
    assert_eq!(layout(&p, a), vec![(id, 0, 19_200, 0)]);
    let a_clip = audio_of(&p, id);
    assert_eq!(a_clip.original_tempo.bpm, 120.0);
    assert_eq!(a_clip.time_mode, ClipTimeMode::Fixed);
    let mut bad = new_audio(0, 960);
    bad.content = NewClipContent::Audio {
        file: file(),
        offset: 480_000,
    };
    assert!(matches!(add_clip(&p, a, bad), Err(EditError::Invalid(_))));
    let mut empty = file();
    empty.frames = 0;
    let mut bad = new_audio(0, 960);
    bad.content = NewClipContent::Audio {
        file: empty,
        offset: 0,
    };
    assert!(matches!(add_clip(&p, a, bad), Err(EditError::Invalid(_))));
    // Stretching past the end clamps; the length edit never exceeds the file.
    let p = ok(set_clip_length(&p, id, Tick(5000)));
    assert_eq!(layout(&p, a)[0].2, 5000);
    let p = ok(set_clip_length(&p, id, Tick(1_000_000)));
    assert_eq!(layout(&p, a)[0].2, 19_200);
}

#[test]
fn audio_props_clamp_and_keep_the_fades_inside_the_clip() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, id) = ok_with(add_clip(&p, a, new_audio(0, 3840)));
    let p = ok(set_audio_clip_prop(&p, id, AudioClipProp::GainDb(99.0)));
    assert_eq!(audio_of(&p, id).gain_db, CLIP_GAIN_DB_MAX);
    let p = ok(set_audio_clip_prop(
        &p,
        id,
        AudioClipProp::FadeIn(Tick(3000)),
    ));
    let p = ok(set_audio_clip_prop(
        &p,
        id,
        AudioClipProp::FadeOut(Tick(3000)),
    ));
    let a_clip = audio_of(&p, id);
    assert_eq!((a_clip.fade_in.0, a_clip.fade_out.0), (3000, 840));
    assert!(set_audio_clip_prop(&p, id, AudioClipProp::GainDb(f32::NAN)).is_err());
    assert!(set_audio_clip_prop(&p, id, AudioClipProp::OriginalTempo(Tempo::new(1.0))).is_err());
    // MIDI-only properties are refused on audio clips; audio props on MIDI clips.
    assert!(matches!(
        set_clip_prop(&p, id, ClipProp::Transpose(3)),
        Err(EditError::WrongClipKind { .. })
    ));
    assert!(matches!(
        set_clip_loop(&p, id, Some(Tick(960)), Tick(3840)),
        Err(EditError::WrongClipKind { .. })
    ));
    let (q, m) = midi_track(&p);
    let (q, mc) = ok_with(add_clip(&q, m, midi(0, 960)));
    assert!(matches!(
        set_audio_clip_prop(&q, mc, AudioClipProp::GainDb(0.0)),
        Err(EditError::WrongClipKind {
            expected: ClipKind::Audio,
            ..
        })
    ));
}

#[test]
fn re_pitch_changes_what_the_length_means_and_clamps_it() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, id) = ok_with(add_clip(&p, a, new_audio(0, 19_200)));
    // Original tempo 60 bpm: the file is 10 s = 2400 ticks at 60 bpm ... no: 10 s at 60 bpm is
    // 10 beats = 9600 ticks, so the clip shrinks to that.
    let p = ok(set_audio_clip_prop(
        &p,
        id,
        AudioClipProp::OriginalTempo(Tempo::new(60.0)),
    ));
    assert_eq!(
        layout(&p, a)[0].2,
        19_200,
        "Fixed mode ignores the original tempo"
    );
    let p = ok(set_audio_clip_prop(
        &p,
        id,
        AudioClipProp::TimeMode(ClipTimeMode::RePitch),
    ));
    assert_eq!(layout(&p, a)[0].2, 9600);
}

// --- The overlap-cut rule ---------------------------------------------------------------------

/// A track with one long clip A from tick 0 to 7680 (offset 0).
fn one_clip() -> (Project, TrackId, ClipId) {
    let (p, t) = audio_track(&Project::new("T"));
    let (p, c) = ok_with(add_clip(&p, t, new_audio(0, 7680)));
    (p, t, c)
}

#[test]
fn a_clip_on_top_cuts_the_end_of_the_one_below() {
    let (p, t, below) = one_clip();
    let (p, top) = ok_with(add_clip(&p, t, new_audio(5760, 3840)));
    assert_eq!(
        layout(&p, t),
        vec![(below, 0, 5760, 0), (top, 5760, 3840, 0)]
    );
}

#[test]
fn a_clip_on_top_cuts_the_start_and_the_window_follows() {
    let (p, t, below) = one_clip();
    let (p, top) = ok_with(add_clip(&p, t, new_audio(0, 3840)));
    // 3840 ticks at 120 bpm = 2 s = 96 000 frames.
    assert_eq!(
        layout(&p, t),
        vec![(top, 0, 3840, 0), (below, 3840, 3840, 96_000)]
    );
}

#[test]
fn a_clip_in_the_middle_splits_the_one_below_and_a_covered_one_goes() {
    let (p, t, below) = one_clip();
    let (p, top) = ok_with(add_clip(&p, t, new_audio(1920, 960)));
    let l = layout(&p, t);
    assert_eq!(l.len(), 3);
    assert_eq!(l[0], (below, 0, 1920, 0));
    assert_eq!(l[1], (top, 1920, 960, 0));
    assert_eq!((l[2].1, l[2].2, l[2].3), (2880, 4800, 72_000));
    assert_ne!(l[2].0, below, "the right part has a new id");
    // A clip covering everything removes what it covers.
    let (p, big) = ok_with(add_clip(&p, t, new_audio(0, 7680)));
    assert_eq!(layout(&p, t), vec![(big, 0, 7680, 0)]);
}

#[test]
fn cut_edges_lose_their_fades_and_the_rest_keeps_them() {
    let (p, t, below) = one_clip();
    let p = ok(set_audio_clip_prop(
        &p,
        below,
        AudioClipProp::FadeIn(Tick(100)),
    ));
    let p = ok(set_audio_clip_prop(
        &p,
        below,
        AudioClipProp::FadeOut(Tick(200)),
    ));
    let (p, _) = ok_with(add_clip(&p, t, new_audio(3840, 960)));
    let left = audio_of(&p, below);
    assert_eq!((left.fade_in.0, left.fade_out.0), (100, 0));
}

#[test]
fn moving_copying_and_stretching_cut_too_in_one_undo_step() {
    let (p, t, below) = one_clip();
    let (p, top) = ok_with(add_clip(&p, t, new_audio(7680, 1920)));
    // Move the top clip back over the end of the first one.
    let moved = ok(move_clips(&p, &[top], Tick(-1920), 0));
    assert_eq!(
        layout(&moved, t),
        vec![(below, 0, 5760, 0), (top, 5760, 1920, 0)]
    );
    // Copy it onto the first one's middle.
    let (copied, copies) = ok_with(copy_clips(&p, &[top], Tick(-7680 + 1920), 0));
    let l = layout(&copied, t);
    assert_eq!(l.len(), 4);
    assert_eq!((l[1].0, l[1].1), (copies[0], 1920));
    // Stretching the first clip's neighbour to the left is a trim; stretching right cuts.
    let (q, n) = ok_with(add_clip(&p, t, new_audio(9600, 960)));
    let q = ok(set_clip_length(&q, top, Tick(2400)));
    assert_eq!(layout(&q, t).last().unwrap().0, n);
    assert_eq!(
        layout(&q, t).last().unwrap().1,
        10_080,
        "n loses its first 480 ticks"
    );
    // One undo step restores everything.
    let mut doc = Document::new(p.clone());
    doc.apply("move", |p| move_clips(p, &[top], Tick(-1920), 0))
        .unwrap();
    assert!(doc.undo());
    assert_eq!(**doc.project(), p);
}

#[test]
fn duplicating_a_clip_cuts_the_neighbour_behind_it() {
    let (p, t, a) = one_clip();
    let (p, b) = ok_with(add_clip(&p, t, new_audio(7680, 3840)));
    let (p, copies) = ok_with(duplicate_clips(&p, &[a]));
    assert_eq!(
        layout(&p, t),
        vec![(a, 0, 7680, 0), (copies[0], 7680, 7680, 0)]
    );
    assert!(p.find_clip(b).is_none() || layout(&p, t).iter().all(|c| c.0 != b || c.1 >= 15_360));
}

#[test]
fn trimming_the_start_moves_the_window_not_the_content() {
    let (p, t, c) = one_clip();
    let p = ok(trim_audio_clip_start(&p, c, Tick(1920)));
    assert_eq!(layout(&p, t), vec![(c, 1920, 5760, 48_000)]);
    // Extending back again restores the window; it never goes before the file start.
    let p = ok(trim_audio_clip_start(&p, c, Tick(0)));
    assert_eq!(layout(&p, t), vec![(c, 0, 7680, 0)]);
    let p = ok(trim_audio_clip_start(&p, c, Tick(-500)));
    assert_eq!(layout(&p, t)[0].1, 0);
    // A trim over a neighbour cuts it.
    let mut d_clip = new_audio(7680, 1920);
    d_clip.content = NewClipContent::Audio {
        file: file(),
        offset: 96_000,
    };
    let (p, d) = ok_with(add_clip(&p, t, d_clip));
    let p = ok(trim_audio_clip_start(&p, d, Tick(5760)));
    assert_eq!(layout(&p, t).last().unwrap(), &(d, 5760, 3840, 48_000));
    assert_eq!(layout(&p, t)[0], (c, 0, 5760, 0));
    // Before the start of the file there is nothing to reveal.
    assert_eq!(layout(&p, t).last().unwrap().1, 5760);
    assert!(matches!(
        trim_audio_clip_start(&Project::new("x"), c, Tick(0)),
        Err(EditError::ClipNotFound(_))
    ));
}

#[test]
fn splitting_an_audio_clip_advances_the_right_window() {
    let (p, t, c) = one_clip();
    let (p, rights) = ok_with(split_clips(&p, &[c], Tick(3840)));
    let l = layout(&p, t);
    assert_eq!(l[0], (c, 0, 3840, 0));
    assert_eq!(
        (l[1].0, l[1].1, l[1].2, l[1].3),
        (rights[0], 3840, 3840, 96_000)
    );
}

// --- Tempo ------------------------------------------------------------------------------------

#[test]
fn tempo_changes_keep_seconds_for_fixed_clips_and_ticks_for_re_pitch() {
    let (p, t) = audio_track(&Project::new("T"));
    let (p, fixed) = ok_with(add_clip(&p, t, new_audio(0, 3840)));
    let (p, pitched) = ok_with(add_clip(&p, t, new_audio(3840, 3840)));
    let p = ok(set_audio_clip_prop(
        &p,
        pitched,
        AudioClipProp::TimeMode(ClipTimeMode::RePitch),
    ));
    // 120 → 60 bpm: the fixed clip's 2 s become 4 s = 3840 ticks at 60 bpm → 1920 ticks would be
    // 2 s; length in ticks halves (a tick lasts twice as long).
    let slower = ok(set_project_prop(&p, ProjectProp::Tempo(Tempo::new(60.0))));
    let l = layout(&slower, t);
    assert_eq!(l[0].2, 1920, "fixed: same seconds, fewer ticks");
    assert_eq!(
        l.iter().find(|c| c.0 == pitched).unwrap().2,
        3840,
        "re-pitch: same ticks"
    );
    assert_eq!(l.iter().find(|c| c.0 == fixed).unwrap().1, 0);
    // 120 → 240 bpm: the fixed clip would double to 7680 ticks, but the clip behind it, which
    // starts on the grid, wins: the fixed clip is cut where it starts.
    let faster = ok(set_project_prop(&p, ProjectProp::Tempo(Tempo::new(240.0))));
    let l = layout(&faster, t);
    assert_eq!(l.len(), 2);
    assert_eq!((l[0].0, l[0].1, l[0].2), (fixed, 0, 3840));
    assert_eq!((l[1].0, l[1].1, l[1].2), (pitched, 3840, 3840));
}

// --- Input and monitoring ---------------------------------------------------------------------

#[test]
fn input_and_monitoring_are_set_on_audio_tracks_only() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, m) = midi_track(&p);
    let hw = AudioInput::Hardware(HardwareInput {
        first: 2,
        stereo: true,
    });
    let q = ok(set_track_prop(&p, &[a], TrackProp::AudioInput(hw)));
    assert_eq!(q.track(a).unwrap().audio.input, hw);
    let q = ok(set_track_prop(
        &q,
        &[a],
        TrackProp::MonitorMode(MonitorMode::Auto),
    ));
    assert_eq!(q.track(a).unwrap().audio.monitor, MonitorMode::Auto);
    assert!(set_track_prop(&p, &[m], TrackProp::AudioInput(hw)).is_err());
    assert!(set_track_prop(&p, &[m], TrackProp::MonitorMode(MonitorMode::On)).is_err());
    // Arm on an audio track leaves the MIDI monitor flag alone; MIDI input props are ignored.
    let q = ok(set_track_prop(&q, &[a], TrackProp::Arm(true)));
    assert!(q.track(a).unwrap().arm && !q.track(a).unwrap().monitor);
    let q = ok(set_track_prop(&q, &[a], TrackProp::Monitor(true)));
    assert!(!q.track(a).unwrap().monitor);
    let q = ok(set_exclusive(&q, a, ExclusiveFlag::Arm));
    assert!(!q.track(a).unwrap().monitor);
}

#[test]
fn recording_a_bus_is_an_input_edge_that_counts_for_cycles() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, b) = bus(&p);
    let p = ok(set_track_prop(
        &p,
        &[a],
        TrackProp::AudioInput(AudioInput::Bus(b)),
    ));
    assert_eq!(p.track(a).unwrap().input_bus(), Some(b));
    let edges = routing::edges(&p);
    assert!(edges.iter().any(|e| e.kind == routing::EdgeKind::Input
        && e.from == ChannelRef::Track(b)
        && e.to == ChannelRef::Track(a)));
    // The track must not output into the bus it records, nor into one that feeds it.
    assert_eq!(
        set_output(&p, &[a], TrackOutput::Bus(b)).unwrap_err(),
        EditError::RoutingCycle
    );
    assert!(!routing::output_targets(&p, a).contains(&TrackOutput::Bus(b)));
    // A second audio track may output into it: only a→b would close the loop.
    let (p2, a2) = audio_track(&p);
    ok(set_output(&p2, &[a2], TrackOutput::Bus(b)));
    // A bus fed by the track cannot become its input.
    let (p3, b2) = bus(&p);
    let p3 = ok(set_output(&p3, &[a], TrackOutput::Bus(b2)));
    assert_eq!(
        set_track_prop(&p3, &[a], TrackProp::AudioInput(AudioInput::Bus(b2))).unwrap_err(),
        EditError::RoutingCycle
    );
    assert!(!routing::input_sources(&p3, a).contains(&b2));
    assert!(routing::input_sources(&p3, a).contains(&b));
    // Only a bus can be recorded, and it must exist.
    let (p4, m) = midi_track(&p);
    assert_eq!(
        set_track_prop(&p4, &[a], TrackProp::AudioInput(AudioInput::Bus(m))).unwrap_err(),
        EditError::NotABus(m)
    );
    assert!(
        set_track_prop(
            &p,
            &[a],
            TrackProp::AudioInput(AudioInput::Bus(TrackId(999)))
        )
        .is_err()
    );
    // The input edge does not take part in solo gating.
    let g = routing::gates(&p);
    assert!(g.output_open(a));
}

#[test]
fn removing_the_recorded_bus_clears_the_input_and_duplicates_follow_copied_buses() {
    let (p, a) = audio_track(&Project::new("T"));
    let (p, b) = bus(&p);
    let p = ok(set_track_prop(
        &p,
        &[a],
        TrackProp::AudioInput(AudioInput::Bus(b)),
    ));
    let (dup, copies) = ok_with(duplicate_tracks(&p, &[a, b]));
    assert_eq!(dup.track(copies[0]).unwrap().input_bus(), Some(copies[1]));
    let gone = ok(remove_tracks(&p, &[b]));
    assert_eq!(gone.track(a).unwrap().audio.input, AudioInput::None);
    let (dup, copies) = ok_with(duplicate_tracks(&p, &[a]));
    assert_eq!(dup.track(copies[0]).unwrap().input_bus(), Some(b));
}

#[test]
fn rewriting_audio_files_keeps_untouched_tracks_shared() {
    let (p, t, c) = one_clip();
    let (p, m) = midi_track(&p);
    assert!(rewrite_audio_files(&p, |_| None).is_none());
    let q = rewrite_audio_files(&p, |f| {
        Some(f.moved_to(
            std::path::Path::new("/p/S/loop.wav"),
            std::path::Path::new("/p"),
        ))
    })
    .unwrap();
    assert_eq!(audio_of(&q, c).file.rel, PathBuf::from("S/loop.wav"));
    assert!(std::sync::Arc::ptr_eq(
        q.track(m).unwrap(),
        p.track(m).unwrap()
    ));
    assert_eq!(q.track(t).unwrap().clips.len(), 1);
}

#[test]
fn a_clip_can_be_relinked_to_another_file() {
    let (p, _, c) = one_clip();
    let mut shorter = file();
    shorter.frames = 96_000; // 2 s = 3840 ticks
    let q = ok(set_audio_clip_file(&p, c, shorter.clone()));
    assert_eq!(q.find_clip(c).unwrap().1.length, Tick(3840));
    shorter.frames = 0;
    assert!(set_audio_clip_file(&p, c, shorter).is_err());
}

// --- Audio recording takes (#13) --------------------------------------------------------------

fn take(name: &str, rate: u32, frames: u64, offset: u64) -> crate::AudioTake {
    crate::AudioTake {
        name: name.into(),
        file: AudioFile {
            abs: format!("/takes/{name}.wav").into(),
            sample_rate: rate,
            frames,
            ..file()
        },
        offset,
    }
}

fn two_takes() -> (Project, TrackId, ClipId) {
    let (p, track) = audio_track(&Project::new("Takes"));
    let (p, clip) = ok_with(add_clip(&p, track, new_audio(3840, 7680)));
    let (p, index) = ok_with(add_audio_take(
        &p,
        clip,
        take("Take 2", 44100, 441000, 44100),
    ));
    assert_eq!(index, 1);
    (p, track, clip)
}

#[test]
fn audio_takes_append_select_rename_remove_and_keep_shared_settings() {
    let (p, _, clip) = two_takes();
    let p = ok(set_audio_clip_prop(&p, clip, AudioClipProp::GainDb(-3.0)));
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::FadeIn(Tick(240)),
    ));
    assert_eq!(audio_of(&p, clip).active_take, 1);
    assert_eq!(audio_of(&p, clip).file.sample_rate, 44100);
    assert_eq!(next_take_number(&p), 3);
    let p = ok(rename_audio_take(&p, clip, 0, "Take 19".into()));
    assert_eq!(next_take_number(&p), 20);
    let p = ok(select_audio_take(&p, clip, 0));
    assert_eq!(audio_of(&p, clip).file.sample_rate, 48000);
    assert_eq!(
        (audio_of(&p, clip).gain_db, audio_of(&p, clip).fade_in),
        (-3.0, Tick(240))
    );
    let p = ok(remove_audio_take(&p, clip, 0));
    let a = audio_of(&p, clip);
    assert_eq!(
        (a.takes.len(), a.active_take, a.active().name.as_str()),
        (1, 0, "Take 2")
    );
    assert!(remove_audio_take(&p, clip, 0).is_err());
    assert!(select_audio_take(&p, clip, 1).is_err());
    assert!(rename_audio_take(&p, clip, 0, " ".into()).is_err());
    assert!(add_audio_take(&p, clip, take("empty", 48000, 1, 1)).is_err());
    // Structural changes are normal immutable edits, so undo restores every source.
    let mut doc = Document::new(p.clone());
    doc.apply("append", |p| {
        add_audio_take(p, clip, take("Take 20", 48000, 480000, 0)).map(|(p, _)| p)
    })
    .unwrap();
    doc.undo();
    assert_eq!(doc.project().as_ref(), &p);
    doc.redo();
    assert_eq!(audio_of(doc.project(), clip).takes.len(), 2);
}

#[test]
fn selecting_a_short_audio_take_shrinks_length_and_fades_without_moving_start() {
    let (p, _, clip) = two_takes();
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::FadeOut(Tick(3000)),
    ));
    let (p, _) = ok_with(add_audio_take(&p, clip, take("Short", 48000, 24000, 0)));
    let c = p.find_clip(clip).unwrap().1;
    assert_eq!((c.start, c.length), (Tick(3840), Tick(960)));
    assert_eq!(audio_of(&p, clip).fade_out, Tick(960));
    assert_eq!(audio_of(&p, clip).takes[0].file.frames, 480000);
}

#[test]
fn splitting_and_trimming_audio_updates_every_take_at_its_own_sample_rate() {
    let (p, _, clip) = two_takes();
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::FadeIn(Tick(480)),
    ));
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::FadeOut(Tick(960)),
    ));
    let (p, rights) = ok_with(split_clips(&p, &[clip], Tick(7680)));
    let left = audio_of(&p, clip);
    let right = audio_of(&p, rights[0]);
    assert_eq!(
        left.takes.iter().map(|t| t.offset).collect::<Vec<_>>(),
        [0, 44100]
    );
    assert_eq!(
        right.takes.iter().map(|t| t.offset).collect::<Vec<_>>(),
        [96000, 132300]
    );
    assert_eq!((left.active_take, right.active_take), (1, 1));
    assert_eq!((left.fade_out, right.fade_in), (Tick::ZERO, Tick::ZERO));
    let p = ok(trim_audio_clip_start(&p, rights[0], Tick(8640)));
    assert_eq!(
        audio_of(&p, rights[0])
            .takes
            .iter()
            .map(|t| t.offset)
            .collect::<Vec<_>>(),
        [120000, 154350]
    );
    let p = ok(trim_audio_clip_start(&p, rights[0], Tick(7680)));
    assert_eq!(
        audio_of(&p, rights[0])
            .takes
            .iter()
            .map(|t| t.offset)
            .collect::<Vec<_>>(),
        [96000, 132300]
    );
}

#[test]
fn overlap_cuts_keep_short_inactive_takes_and_refuse_selecting_exhausted_windows() {
    let (p, track, clip) = two_takes();
    // Keep a short inactive pass while selecting the original long pass for playback.
    let (p, _) = ok_with(add_audio_take(&p, clip, take("Short", 48000, 24000, 0)));
    let p = ok(select_audio_take(&p, clip, 0));
    let p = ok(set_clip_length(&p, clip, Tick(7680)));
    let (p, _) = ok_with(add_clip(&p, track, new_audio(4800, 2880)));
    let right = p
        .track(track)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.start == Tick(7680))
        .unwrap();
    let a = audio_of(&p, right.id);
    assert_eq!(a.takes.len(), 3);
    assert_eq!(
        a.takes[2].offset, 96000,
        "retain true offset beyond short source EOF"
    );
    assert!(select_audio_take(&p, right.id, 2).is_err());
    // The extended edge comes back to the original timeline alignment of every pass.
    let p = ok(trim_audio_clip_start(&p, right.id, Tick(3840)));
    let c = p
        .track(track)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.start == Tick(3840))
        .unwrap();
    assert_eq!(audio_of(&p, c.id).takes[2].offset, 0);
}

#[test]
fn source_offset_edits_only_the_active_take_and_preserve_timeline_start() {
    let (p, _, clip) = two_takes();
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::SourceOffset(88200),
    ));
    let a = audio_of(&p, clip);
    assert_eq!((a.takes[0].offset, a.takes[1].offset), (0, 88200));
    assert_eq!(p.find_clip(clip).unwrap().1.start, Tick(3840));
    let p = ok(set_audio_clip_prop(
        &p,
        clip,
        AudioClipProp::SourceOffset(u64::MAX),
    ));
    assert_eq!(audio_of(&p, clip).offset, 440999);
    assert_eq!(p.find_clip(clip).unwrap().1.length, Tick(1));
}

#[test]
fn rewriting_audio_links_visits_inactive_takes_and_keeps_unchanged_arcs() {
    let (p, track, clip) = two_takes();
    let mut seen = Vec::new();
    assert!(
        rewrite_audio_files(&p, |f| {
            seen.push(f.abs.clone());
            None
        })
        .is_none()
    );
    assert_eq!(seen.len(), 2);
    let original_active = audio_of(&p, clip).file.clone();
    let q = rewrite_audio_files(&p, |f| {
        (f.sample_rate == 48000).then(|| AudioFile {
            rel: "Takes/original.wav".into(),
            ..f.clone()
        })
    })
    .unwrap();
    assert!(!Arc::ptr_eq(
        p.track(track).unwrap(),
        q.track(track).unwrap()
    ));
    assert_eq!(
        audio_of(&q, clip).takes[0].file.rel,
        PathBuf::from("Takes/original.wav")
    );
    assert_eq!(audio_of(&q, clip).file, original_active);
}

#[test]
fn exhausted_audio_identifiers_reject_tracks_and_overlap_splits_atomically() {
    let mut empty = Project::new("Exhausted audio");
    empty.next_id = u64::MAX;
    assert_eq!(
        add_audio_track(&empty, NewAudioTrack::default()).unwrap_err(),
        EditError::IdsExhausted
    );

    let (p, track) = audio_track(&Project::new("Overlap"));
    let (mut p, _) = ok_with(add_clip(&p, track, new_audio(0, 3840)));
    p.next_id = u64::MAX - 2; // one ID for the new clip, no room for the victim's right half
    let mut doc = Document::new(p);
    let before = Arc::clone(doc.project());
    assert_eq!(
        doc.apply_with("overlap", |p| add_clip(p, track, new_audio(960, 960))),
        Err(EditError::IdsExhausted)
    );
    assert!(Arc::ptr_eq(&before, doc.project()));
    assert_eq!(doc.revision(), 0);
}

#[test]
fn overlap_repair_rejects_an_exhausted_file_instead_of_panicking() {
    let (p, track) = audio_track(&Project::new("Overlap repair"));
    let (p, _) = ok_with(add_clip(&p, track, new_audio(0, 3840)));
    let (mut p, clip) = ok_with(add_clip(&p, track, new_audio(5000, 960)));
    let t = Arc::make_mut(&mut p.tracks[0]);
    let c = t.clips.iter_mut().find(|c| c.id == clip).unwrap();
    Arc::make_mut(c).start = Tick(960); // hand-edited file; repair needs a second victim piece
    p.next_id = u64::MAX - 1;
    let text = crate::persist::to_json(&p).unwrap();
    let error = crate::persist::from_json(&text).unwrap_err();
    assert!(
        matches!(error, crate::persist::PersistError::Malformed(message) if message.contains("identifiers exhausted"))
    );
}
