//! The byte stream a hostile bridge writes into the control socket: frame headers, fragments and
//! payloads, received by the app end.
#![no_main]

libfuzzer_sys::fuzz_target!(|wire: &[u8]| {
    komponist_ipc::control::receive_hostile_stream(wire).expect("socket pair");
});
