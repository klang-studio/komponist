//! A plugin's native layout descriptor (JSON), parsed and checked against an inventory.
#![no_main]

libfuzzer_sys::fuzz_target!(|bytes: &[u8]| {
    komponist_plugin_api::fuzzing::native_layout(bytes);
});
