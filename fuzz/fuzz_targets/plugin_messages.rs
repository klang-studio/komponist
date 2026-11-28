//! Plugin-supplied messages of the komponist CLAP extensions: sampler snapshot, waveform markers,
//! device assets and their edits, device canvas display lists, pad activity frames.
#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    komponist_plugin_api::fuzzing::plugin_messages(bytes);
});
