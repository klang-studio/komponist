//! An audio file (WAV through hound; AIFF, FLAC, OGG Vorbis and MP3 through symphonia): streaming
//! reads, a seek, a whole-file decode and peaks. The first byte picks the file extension (a hint
//! for symphonia; the decoders sniff the content) and the seek target; the rest is the file.
#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;

const EXTENSIONS: [&str; 6] = ["wav", "aiff", "flac", "ogg", "mp3", "bin"];

/// A folder of this process in the temporary folder (set `TMPDIR` to keep it elsewhere).
fn dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("komponist-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fuzz scratch folder");
        dir
    })
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&pick, file)) = data.split_first() else {
        return;
    };
    let ext = EXTENSIONS[usize::from(pick & 7) % EXTENSIONS.len()];
    let seek = u64::from(pick >> 3) * 4099;
    komponist_audiofile::fuzzing::audio_bytes(dir(), file, ext, seek);
});
