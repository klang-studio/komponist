//! Shared worker determinism checks for test processors and device integrations.

use komponist_engine::{Command, MidiInEvent, MidiPort, Processor, Program, SlotId};
use komponist_model::{Tick, TrackId};

use super::engine_common::{BAR, RATE, Rig};

/// A program and the processors of its slots.
pub type Setup = (Program, Vec<(u32, Box<dyn Processor>)>);

/// Plays `make`'s program with `workers` worker threads: `frames` frames in calls of varying
/// length (with live MIDI, a locate and a solo change on the way) and then an offline render; returns
/// the output.
pub fn play(workers: usize, frames: usize, make: fn() -> Setup) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(RATE, 256);
    let status = rig.engine.set_workers(workers, None);
    assert_eq!(status.threads, workers);
    assert_eq!(rig.engine.workers(), workers);
    let (program, processors) = make();
    for (slot, processor) in processors {
        rig.send(Command::InsertProcessor {
            slot: SlotId(slot),
            processor,
        });
    }
    rig.set_program(&program);
    rig.send(Command::Play);
    let blocks = [256, 256, 100, 512, 37, 2048, 3000];
    let mut done = 0;
    let mut call = 0;
    while done < frames {
        let n = blocks[call % blocks.len()].min(frames - done);
        let midi = [MidiInEvent {
            frame: (n / 2) as u32,
            port: MidiPort(0),
            data: [0x90, 60 + (call % 12) as u8, 100],
        }];
        rig.process_with(n, &midi, 0);
        match call {
            7 => rig.send(Command::Locate(Tick(BAR / 3))),
            11 => rig.send(Command::SetTrackSolo {
                track: TrackId(4),
                solo: true,
            }),
            15 => rig.send(Command::SetTrackSolo {
                track: TrackId(4),
                solo: false,
            }),
            _ => {}
        }
        done += n;
        call += 1;
    }
    rig.render(frames / 2, 333);
    rig.send(Command::Stop);
    rig.process(1024, 256);
    // Not vacuous: the workers took part.
    let stats = rig.engine.worker_stats();
    assert_eq!(
        stats.worker_jobs() > 0,
        workers > 0,
        "{}",
        stats.worker_jobs()
    );
    (rig.l, rig.r)
}

pub fn assert_bit_exact(a: &(Vec<f32>, Vec<f32>), b: &(Vec<f32>, Vec<f32>), what: &str) {
    for (channel, (x, y)) in [(&a.0, &b.0), (&a.1, &b.1)].into_iter().enumerate() {
        assert_eq!(x.len(), y.len(), "{what}: channel {channel} length");
        if let Some(i) = x
            .iter()
            .zip(y)
            .position(|(p, q)| p.to_bits() != q.to_bits())
        {
            panic!("{what}: channel {channel} differs first at frame {i}");
        }
    }
    // Not vacuous: the material sounds.
    assert!(a.0.iter().any(|x| x.abs() > 0.01), "{what}: silent");
}
