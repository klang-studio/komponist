//! The `IBStream` calls a hostile VST3 plugin makes on the host's stream in `getState` and
//! `setState`. The first byte is the length of the stream's initial content, which follows; the
//! rest encodes the calls.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&len, rest)) = data.split_first() else {
        return;
    };
    let (initial, ops) = rest.split_at(usize::from(len).min(rest.len()));
    komponist_hosting::fuzzing::vst3_stream(initial, ops);
});
