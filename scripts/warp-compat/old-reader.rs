//! Runs against unmodified historical komponist-model source, never the current model.
use legacy_model::{AudioFile, ClipContent, Project, Tick, edits, persist};
use std::path::{Path, PathBuf};
fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("exchange directory"));
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("source-")
        })
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 13);
    for path in cases {
        let name = path.file_name().unwrap().to_string_lossy();
        let text = std::fs::read_to_string(&path).unwrap();
        let wire: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(wire["version"], 3);
        let p = persist::from_json(&text).unwrap();
        for original in wire["project"]["tracks"][0]["clips"].as_array().unwrap() {
            let id = legacy_model::ClipId(original["id"].as_u64().unwrap());
            let clip = p.find_clip(id).unwrap().1;
            let ClipContent::Audio(a) = &clip.content else {
                panic!("audio")
            };
            let content = &original["content"];
            assert_eq!(a.time_mode, legacy_model::ClipTimeMode::Fixed);
            assert_eq!(
                a.speed(p.tempo),
                1.0,
                "actual old reader selects unwarped playback"
            );
            assert_eq!(a.offset, content["offset"].as_u64().unwrap());
            assert_eq!(a.takes.len(), 2);
            assert_eq!(a.active_take, 1);
            let written = Tick(original["length"].as_i64().unwrap());
            assert_eq!(clip.length, written.min(a.max_length(p.tempo).max(Tick(1))));
            if name == "source-150.komponist" {
                assert_eq!(written, Tick(23040));
                assert_eq!(clip.length, Tick(15360));
            }
            if name == "source-075.komponist" {
                assert_eq!(written, Tick(11520));
                assert_eq!(clip.length, Tick(11520));
            }
            println!(
                "{name} clip {}: written {}, old-reader {}, fixed speed {}",
                id.0,
                written.0,
                clip.length.0,
                a.speed(p.tempo)
            );
        }
        let saved = persist::to_json(&p).unwrap();
        let value: serde_json::Value = serde_json::from_str(&saved).unwrap();
        for c in value["project"]["tracks"][0]["clips"].as_array().unwrap() {
            assert!(c["content"].get("warp").is_none());
        }
        std::fs::write(dir.join(format!("old-resaved-{name}")), saved).unwrap();
    }
    let (p, t) =
        edits::add_audio_track(&Project::new("Old file"), edits::NewAudioTrack::default()).unwrap();
    let (p, c) = edits::add_clip(
        &p,
        t,
        edits::NewClip {
            start: Tick::ZERO,
            length: Tick(3840),
            name: "Old audio".into(),
            content: edits::NewClipContent::Audio {
                file: AudioFile::new(Path::new("/old.wav"), None, 192000, 48000, 2),
                offset: 4800,
            },
        },
    )
    .unwrap();
    std::fs::write(
        dir.join("old-generated-fixed.komponist"),
        persist::to_json(&p).unwrap(),
    )
    .unwrap();
    let p = edits::set_audio_clip_prop(
        &p,
        c,
        edits::AudioClipProp::TimeMode(legacy_model::ClipTimeMode::RePitch),
    )
    .unwrap();
    std::fs::write(
        dir.join("old-generated-repitch.komponist"),
        persist::to_json(&p).unwrap(),
    )
    .unwrap();
}
