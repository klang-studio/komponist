//! Shared sequencer workload for engine and device integration tests.

use komponist_engine::program::SeqNote;

use super::engine_common::{BAR, note};

pub fn material(track: u32) -> Vec<SeqNote> {
    let mut notes = Vec::new();
    for half in 0..8i64 {
        let root = 48 + ((half as u32 * 5 + track) % 12) as u8;
        for (k, interval) in [0u8, 4, 7, 11].into_iter().enumerate() {
            notes.push(SeqNote {
                velocity: 90,
                ..note(
                    half * BAR / 2 + k as i64 * 10,
                    BAR / 2 - 30,
                    root + interval,
                )
            });
        }
    }
    for step in 0..64i64 {
        notes.push(SeqNote {
            velocity: 70,
            ..note(
                step * BAR / 16,
                BAR / 32,
                72 + ((step * 7 + i64::from(track)) % 12) as u8,
            )
        });
    }
    notes.sort_by_key(|n| (n.start, n.key));
    notes
}
