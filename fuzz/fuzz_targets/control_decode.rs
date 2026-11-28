//! A control message payload from the bridge (or the app): postcard, decoded by
//! `komponist_ipc::control::decode_message`.
#![no_main]

libfuzzer_sys::fuzz_target!(|payload: &[u8]| {
    komponist_ipc::control::check_payload(payload);
});
