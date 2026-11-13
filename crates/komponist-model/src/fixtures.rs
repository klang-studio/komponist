//! Ready-made projects for the app, tests and the demo player.

use crate::project::Project;

/// Name of the demo project.
pub const DEMO_PROJECT_NAME: &str = "Nachtfahrt";

/// The demo project "Nachtfahrt", ported 1:1 from `demoProject()` in `ks-engine.js`.
///
/// 124 bpm, 4/4, loop on over bars 9–13 (prototype `loopS = 128`, `loopE = 192` steps → ticks
/// 30720 .. 46080), metronome off, count-in 1 bar. Six tracks in this order, with the prototype's
/// hues, volumes, pans, clips (bar positions → ticks, note positions in 1/16 steps → ticks via
/// [`crate::time::Tick::from_steps`]) and device chains:
///
/// | Track | Hue | dB | Pan | Instrument | Effects |
/// |---|---|---|---|---|---|
/// | Drums | 45 | −6 | 0 | klopfer (builtin) | LSP Compressor |
/// | Bass | 250 | −4 | 0 | pisynth (builtin) | LSP Para EQ x8 |
/// | Lead | 150 | −3 | 0 | pisynth (builtin) | LSP Para EQ x8, TAL-Chorus-LX (bypassed), Dragonfly Hall Reverb |
/// | Pad | 300 | −9 | −0.1 | Surge XT (CLAP) | LSP Para EQ x8, Dragonfly Hall Reverb |
/// | Keys | 95 | −8 | 0.2 | Dexed (VST3) | – |
/// | Arp | 200 | −10 | 0 | Vital (VST3), track muted | – |
///
/// Drum clips are step clips with the default rows ("Beat A" / "Beat B" patterns from `beat()`,
/// 16 steps at 1/16, velocities from the prototype's step values). Third-party plugins are
/// referenced by their real CLAP ids / VST3 class ids where known, so they show as missing on
/// machines without them. Ids are allocated in document order starting at 1.
pub fn demo_project() -> Project {
    todo!("WP-12")
}

/// An empty project as created by Ctrl N (S10): [`Project::new`] with
/// [`crate::project::DEFAULT_PROJECT_NAME`].
pub fn empty_project() -> Project {
    Project::new(crate::project::DEFAULT_PROJECT_NAME)
}
