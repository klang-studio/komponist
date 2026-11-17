use super::tests::check_invariants;
use super::*;
use crate::{AudioClip, AudioFile, AudioTake, AudioWarp, ClipTimeMode, Document, persist};
use std::path::Path;

fn ok(r: EditResult) -> Project {
    let p = r.unwrap();
    check_invariants(&p);
    p
}
fn audio(p: &Project, id: ClipId) -> &AudioClip {
    match &p.find_clip(id).unwrap().1.content {
        ClipContent::Audio(a) => a,
        _ => panic!("audio"),
    }
}
fn setup() -> (Project, ClipId) {
    let (p, t) = add_audio_track(&Project::new("Warp edits"), NewAudioTrack::default()).unwrap();
    let (p, c) = add_clip(
        &p,
        t,
        NewClip {
            start: Tick(1000),
            length: Tick(3840),
            name: "Window".into(),
            content: NewClipContent::Audio {
                file: AudioFile::new(Path::new("/48k.wav"), None, 384000, 48000, 2),
                offset: 480,
            },
        },
    )
    .unwrap();
    let (p, _) = add_audio_take(
        &p,
        c,
        AudioTake {
            name: "44.1k".into(),
            file: AudioFile::new(Path::new("/44k.wav"), None, 352800, 44100, 1),
            offset: 441,
        },
    )
    .unwrap();
    let p = ok(set_audio_clip_prop(
        &p,
        c,
        AudioClipProp::OriginalTempo(Tempo::new(123.45)),
    ));
    (ok(set_audio_clip_warp(&p, c, true)), c)
}

#[test]
fn optional_wire_uses_fixed_legacy_mode_and_no_format_bump() {
    let (p, c) = setup();
    let text = persist::to_json(&p).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(wire["version"], 3);
    let value = &wire["project"]["tracks"][0]["clips"][0]["content"];
    assert_eq!(value["time_mode"], "fixed");
    assert_eq!(
        value["warp"],
        serde_json::json!({"version":1,"enabled":true,"phase":0})
    );
    let loaded = persist::from_json(&text).unwrap();
    assert_eq!(audio(&loaded, c), audio(&p, c));
    assert_eq!(persist::to_json(&loaded).unwrap(), text);
}

#[test]
fn split_and_overlap_cuts_keep_every_takes_exact_mapping_and_source_links() {
    let (p, c) = setup();
    let before = audio(&p, c).clone();
    let (cut, right) = split_clips(&p, &[c], Tick(1013)).unwrap();
    check_invariants(&cut);
    let r = audio(&cut, right[0]);
    assert_eq!(r.warp.unwrap().phase(), Tick(13));
    for i in 0..r.takes.len() {
        assert_eq!(r.takes[i].file, before.takes[i].file);
        for t in [0, 1, 17, 100, 3827] {
            assert_eq!(
                r.warp_map(i).unwrap().source_frame(Tick(t)),
                before.warp_map(i).unwrap().source_frame(Tick(13 + t))
            );
        }
    }
    let (twice, last) = split_clips(&cut, &right, Tick(1047)).unwrap();
    let (direct, one) = split_clips(&p, &[c], Tick(1047)).unwrap();
    assert_eq!(audio(&twice, last[0]), audio(&direct, one[0]));
    let top = NewClip {
        start: Tick(1100),
        length: Tick(100),
        name: "Top".into(),
        content: NewClipContent::Audio {
            file: before.file.clone(),
            offset: 0,
        },
    };
    let track = p.find_clip(c).unwrap().0.id;
    let (covered, _) = add_clip(&p, track, top).unwrap();
    check_invariants(&covered);
    let right = covered
        .track(track)
        .unwrap()
        .clips
        .iter()
        .find(|clip| clip.start == Tick(1200))
        .unwrap();
    let ClipContent::Audio(right) = &right.content else {
        panic!("audio")
    };
    assert_eq!(right.warp.unwrap().phase(), Tick(200));
    for i in 0..right.takes.len() {
        assert_eq!(
            right.takes[i].offset,
            before.warp_map(i).unwrap().source_frame(Tick(200)).unwrap()
        );
    }
}

#[test]
fn trim_in_and_back_restores_phase_offsets_and_duration_without_drift() {
    let (p, c) = setup();
    let original = audio(&p, c).clone();
    let trimmed = ok(trim_audio_clip_start(&p, c, Tick(1013)));
    assert_eq!(audio(&trimmed, c).warp.unwrap().phase(), Tick(13));
    let restored = ok(trim_audio_clip_start(&trimmed, c, Tick(1000)));
    assert_eq!(audio(&restored, c), &original);
    assert_eq!(restored.find_clip(c).unwrap().1.length, Tick(3840));
    let right_trim = ok(set_clip_length(&p, c, Tick(1111)));
    assert_eq!(audio(&right_trim, c).warp, original.warp);
    assert_eq!(audio(&right_trim, c).takes, original.takes);
    let extended = ok(set_clip_length(&right_trim, c, Tick(3840)));
    assert_eq!(audio(&extended, c), &original);
}

#[test]
fn tempo_changes_keep_warp_ticks_and_original_tempo_changes_reanchor_at_source_start() {
    let (p, c) = setup();
    let before = p.find_clip(c).unwrap().1.clone();
    let p = ok(set_project_prop(&p, ProjectProp::Tempo(Tempo::new(80.0))));
    assert_eq!(p.find_clip(c).unwrap().1.length, before.length);
    assert_eq!(
        audio(&p, c).warp_map(0).unwrap().duration_ratio(p.tempo),
        Ok(123.45 / 80.0)
    );
    let p = ok(trim_audio_clip_start(&p, c, Tick(1013)));
    let offsets = audio(&p, c).takes.clone();
    let p = ok(set_audio_clip_prop(
        &p,
        c,
        AudioClipProp::OriginalTempo(Tempo::new(100.0)),
    ));
    assert_eq!(audio(&p, c).warp.unwrap().phase(), Tick::ZERO);
    assert_eq!(audio(&p, c).takes, offsets);
    assert_eq!(
        audio(&p, c).warp_map(0).unwrap().source_frames(Tick(960)),
        Ok(28800)
    );
}

#[test]
fn legacy_mode_edits_disable_warp_and_gestures_undo_redo_complete_state() {
    let (p, c) = setup();
    let mut doc = Document::new(p.clone());
    doc.begin_gesture("Warp source and trim");
    doc.apply("Source", |p| {
        set_audio_clip_prop(p, c, AudioClipProp::SourceOffset(960))
    })
    .unwrap();
    doc.apply("Trim", |p| trim_audio_clip_start(p, c, Tick(1021)))
        .unwrap();
    doc.end_gesture();
    let after = doc.project().as_ref().clone();
    assert_ne!(after, p);
    assert!(doc.undo());
    assert_eq!(doc.project().as_ref(), &p);
    assert!(doc.redo());
    assert_eq!(doc.project().as_ref(), &after);
    let fixed = ok(set_audio_clip_prop(
        &p,
        c,
        AudioClipProp::TimeMode(ClipTimeMode::Fixed),
    ));
    assert!(!audio(&fixed, c).warp_enabled());
    let repitch = ok(set_audio_clip_prop(
        &p,
        c,
        AudioClipProp::TimeMode(ClipTimeMode::RePitch),
    ));
    assert!(!audio(&repitch, c).warp_enabled());
    assert_eq!(audio(&repitch, c).time_mode, ClipTimeMode::RePitch);
    let enabled = ok(set_audio_clip_warp(&repitch, c, true));
    assert!(audio(&enabled, c).warp_enabled());
    assert_eq!(audio(&enabled, c).time_mode, ClipTimeMode::Fixed);
    let disabled = ok(set_audio_clip_warp(&enabled, c, false));
    assert_eq!(audio(&disabled, c).warp, Some(AudioWarp::new(false)));
}

#[test]
fn exhausted_inactive_takes_stay_linked_and_cannot_be_selected() {
    let (p, c) = setup();
    let (p, index) = add_audio_take(
        &p,
        c,
        AudioTake {
            name: "Short".into(),
            file: AudioFile::new(Path::new("/short.wav"), None, 22050, 44100, 1),
            offset: 0,
        },
    )
    .unwrap();
    let p = ok(select_audio_take(&p, c, 0));
    let p = ok(set_clip_length(&p, c, Tick(10000)));
    let (p, right) = split_clips(&p, &[c], Tick(6000)).unwrap();
    check_invariants(&p);
    let r = audio(&p, right[0]);
    assert!(r.takes[index].offset > r.takes[index].file.frames);
    assert!(select_audio_take(&p, right[0], index).is_err());
    let text = persist::to_json(&p).unwrap();
    assert_eq!(persist::from_json(&text).unwrap(), p);
    let p = ok(remove_audio_take(&p, right[0], index));
    assert_eq!(audio(&p, right[0]).takes.len(), 2);
}

#[test]
fn moves_copies_and_duplicates_preserve_source_mapping_instead_of_reanchoring_it() {
    let (p, c) = setup();
    let p = ok(trim_audio_clip_start(&p, c, Tick(1013)));
    let original = audio(&p, c).clone();
    let moved = ok(move_clips(&p, &[c], Tick(20000), 0));
    assert_eq!(audio(&moved, c), &original);
    let (copied, ids) = copy_clips(&p, &[c], Tick(20000), 0).unwrap();
    check_invariants(&copied);
    assert_eq!(audio(&copied, ids[0]), &original);
    let (duplicated, ids) = duplicate_clips(&p, &[c]).unwrap();
    check_invariants(&duplicated);
    assert_eq!(audio(&duplicated, ids[0]), &original);
}

#[test]
fn take_selection_relinking_and_offsets_keep_native_units_and_shared_warp_settings() {
    let (p, c) = setup();
    let p = ok(trim_audio_clip_start(&p, c, Tick(1013)));
    let phase = audio(&p, c).warp;
    let p = ok(select_audio_take(&p, c, 0));
    assert_eq!(audio(&p, c).warp, phase);
    let before = audio(&p, c).takes.clone();
    let p = ok(set_audio_clip_prop(&p, c, AudioClipProp::SourceOffset(960)));
    assert_eq!(audio(&p, c).takes[0].offset, 960);
    assert_eq!(audio(&p, c).takes[1], before[1]);
    assert_eq!(audio(&p, c).warp, phase);
    let p = ok(set_audio_clip_file(
        &p,
        c,
        AudioFile::new(Path::new("/96k.wav"), None, 96000, 96000, 2),
    ));
    assert_eq!(audio(&p, c).offset, 960);
    assert_eq!(audio(&p, c).file.sample_rate, 96000);
    assert_eq!(audio(&p, c).takes[1], before[1]);
    assert_eq!(audio(&p, c).warp, phase);
    assert_eq!(
        p.find_clip(c).unwrap().1.length,
        audio(&p, c).max_length(p.tempo)
    );
}

#[test]
fn invalid_warp_files_reject_incompatible_modes_versions_and_spans() {
    let (p, _) = setup();
    let value: serde_json::Value = serde_json::from_str(&persist::to_json(&p).unwrap()).unwrap();
    for (field, bad) in [
        ("version", serde_json::json!(2)),
        ("phase", serde_json::json!(crate::MAX_WARP_TICK)),
        ("phase", serde_json::json!(i64::MAX)),
        ("enabled", serde_json::json!("true")),
    ] {
        let mut wire = value.clone();
        wire["project"]["tracks"][0]["clips"][0]["content"]["warp"][field] = bad;
        assert!(persist::from_json(&wire.to_string()).is_err());
    }
    for length in [0, -1] {
        let mut wire = value.clone();
        wire["project"]["tracks"][0]["clips"][0]["length"] = serde_json::json!(length);
        assert!(persist::from_json(&wire.to_string()).is_err());
    }
    let mut wire = value;
    wire["project"]["tracks"][0]["clips"][0]["content"]["time_mode"] = serde_json::json!("repitch");
    assert!(persist::from_json(&wire.to_string()).is_err());
}
