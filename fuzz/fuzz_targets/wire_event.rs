//! One event slot of a shared-memory ring, as a hostile bridge can fill it.
#![no_main]

use komponist_ipc::shm::{WireEvent, check_wire_event};

libfuzzer_sys::fuzz_target!(|bytes: [u8; 32]| {
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    check_wire_event(&WireEvent {
        offset: word(0),
        kind: word(4),
        note_id: word(8),
        param_id: word(12),
        value: f64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        data: bytes[24..28].try_into().unwrap(),
        reserved: word(28),
    });
});
