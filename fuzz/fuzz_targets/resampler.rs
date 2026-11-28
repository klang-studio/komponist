//! The stream resampler fed with chunks at a file's rate, which comes from an untrusted header.
#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    komponist_audiofile::fuzzing::resampler(bytes);
});
