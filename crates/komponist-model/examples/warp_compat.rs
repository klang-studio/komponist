//! Exchange real v3 files with the pinned pre-Warp reader (scripts/check-warp-compat.sh).
use komponist_model::{AudioFile, AudioTake, ClipContent, Project, Tempo, Tick, edits, persist};
use std::path::{Path, PathBuf};

fn fixture() -> (Project, komponist_model::ClipId) {
    let (p, t) = edits::add_audio_track(
        &Project::new("Reader compatibility"),
        edits::NewAudioTrack::default(),
    )
    .unwrap();
    let (p, c) = edits::add_clip(
        &p,
        t,
        edits::NewClip {
            start: Tick(1000),
            length: Tick(15360),
            name: "Eight source seconds".into(),
            content: edits::NewClipContent::Audio {
                file: AudioFile::new(Path::new("/48k.wav"), None, 388800, 48000, 2),
                offset: 4800,
            },
        },
    )
    .unwrap();
    let (p, _) = edits::add_audio_take(
        &p,
        c,
        AudioTake {
            name: "44.1 kHz".into(),
            file: AudioFile::new(Path::new("/44k.wav"), None, 357210, 44100, 1),
            offset: 4410,
        },
    )
    .unwrap();
    (p, c)
}
// These fixtures intentionally exercise Warp in the pre-automation wire format.
fn v3_json(project: &Project) -> String {
    let text = persist::to_json(project).unwrap();
    assert!(project.master_automation.is_empty());
    assert!(
        project
            .tracks
            .iter()
            .all(|track| track.automation.is_empty()
                && track.clips.iter().all(|clip| clip.automation.is_empty()))
    );
    text.replacen(
        &format!("\"version\": {}", persist::FORMAT_VERSION),
        "\"version\": 3",
        1,
    )
}

fn prepare(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let (plain, _) = fixture();
    std::fs::write(dir.join("source-plain.komponist"), v3_json(&plain)).unwrap();
    for (name, ratio) in [
        ("050", 0.5),
        ("075", 0.75),
        ("090", 0.9),
        ("110", 1.1),
        ("150", 1.5),
        ("200", 2.0),
    ] {
        let (p, c) = fixture();
        let p = edits::set_audio_clip_prop(
            &p,
            c,
            edits::AudioClipProp::OriginalTempo(Tempo::new(120.0 * ratio)),
        )
        .unwrap();
        let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
        let p = edits::set_clip_length(&p, c, Tick((15360.0 * ratio).round() as i64)).unwrap();
        let text = v3_json(&p);
        assert_eq!(v3_json(&persist::from_json(&text).unwrap()), text);
        std::fs::write(dir.join(format!("source-{name}.komponist")), text).unwrap();
        let (p, _) = edits::split_clips(&p, &[c], Tick(1137)).unwrap();
        std::fs::write(
            dir.join(format!("source-{name}-split.komponist")),
            v3_json(&p),
        )
        .unwrap();
    }
    println!("Prepared 13 current-reader files, format v3; no audio IO");
}
fn verify(dir: &Path) {
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if !(name.starts_with("old-resaved-") || name.starts_with("old-generated-")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let p = persist::from_json(&text).unwrap();
        for c in p.tracks.iter().flat_map(|t| &t.clips) {
            if let ClipContent::Audio(a) = &c.content {
                assert!(a.warp.is_none(), "old save retains no unknown Warp data");
            }
        }
        assert_eq!(
            v3_json(&p),
            text,
            "non-Warp serialization changed for {name}"
        );
        count += 1;
    }
    assert_eq!(count, 15);
    println!(
        "Verified {count} old-reader files: no Warp data; byte-identical current serialization"
    );
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "prepare|verify DIR");
    let dir = PathBuf::from(&args[2]);
    match args[1].as_str() {
        "prepare" => prepare(&dir),
        "verify" => verify(&dir),
        _ => panic!("prepare|verify DIR"),
    }
}
