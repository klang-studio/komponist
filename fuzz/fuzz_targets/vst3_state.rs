//! A saved VST3 state blob (`KPV3`) from a project file.
#![no_main]

libfuzzer_sys::fuzz_target!(|blob: &[u8]| {
    komponist_hosting::fuzzing::vst3_state(blob);
});
