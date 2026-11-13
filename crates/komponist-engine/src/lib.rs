//! The realtime engine of komponist.
//!
//! Transport, sequencer, mixer, metronome and the `Processor` trait run inside `Engine::process` on
//! the audio thread, which never allocates, locks or blocks. The program compiler turns a project
//! into the immutable program the engine plays. Offline render and WAV export live here too. The
//! engine knows no UI, no IPC and no plugin format.
//!
//! Scaffold only: the contracts arrive with WP-01, the implementation with WP-23 and WP-24.

#[cfg(test)]
mod tests {
    /// Proves that the sibling crate `../klang-dsp` resolves and links.
    #[test]
    fn klang_dsp_links() {
        assert_eq!(klang_dsp::units::db_to_gain(0.0), 1.0);
    }
}
