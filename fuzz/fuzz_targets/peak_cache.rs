//! A peak cache record from the cache folder (`KPK3`).
#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    komponist_audiofile::fuzzing::peak_cache(bytes);
});
