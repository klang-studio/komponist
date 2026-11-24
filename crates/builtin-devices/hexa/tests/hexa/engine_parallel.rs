//! Hexa renders bit-identically with and without engine workers.

use komponist_engine::Processor;
use komponist_hexa::{HexaProcessor, preset_patch, presets};
use komponist_model::{Tick, TickRange};

use crate::engine_common::{BAR, RATE, program, track};
use crate::material::material;
use crate::parallel_support::{Setup, assert_bit_exact, play};

/// 8 hexa tracks with the bench's stress material (presets E-Piano, Strings, Supersaw, Init).
fn hexas() -> Setup {
    let names = ["E-Piano", "Strings", "Supersaw", "Init"];
    let mut processors: Vec<(u32, Box<dyn Processor>)> = Vec::new();
    let tracks = (0..8)
        .map(|i| {
            let preset = presets().find(|x| x.name == names[i as usize % 4]).unwrap();
            let mut p = HexaProcessor::new(preset_patch(preset.index).unwrap());
            p.activate(RATE, 256).unwrap();
            processors.push((i, Box::new(p)));
            track(u64::from(i) + 1, i, material(i))
        })
        .collect();
    let mut p = program(120.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(4 * BAR));
    p.loop_on = true;
    (p, processors)
}

#[test]
fn hexa_is_bit_exact_with_workers() {
    let frames = 48_000;
    let single = play(0, frames, hexas);
    let parallel = play(3, frames, hexas);
    assert_bit_exact(&single, &parallel, "hexa, 3 workers");
}
