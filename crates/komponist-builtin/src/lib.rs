//! Built-in processors of komponist.
//!
//! The pisynth adapter (`synth_core::Synth` as an in-process `Processor`) and the metronome voice.
//!
//! Scaffold only: the implementation arrives with WP-25.

#[cfg(test)]
mod tests {
    /// Proves that pisynth's `synth-core` (`../pisynth/crates/synth-core`) resolves and links with
    /// the `alloc` and `serde` features.
    #[test]
    fn synth_core_links() {
        let preset = synth_core::preset(0).expect("factory bank has a first preset");
        assert!(!preset.name.is_empty());
        // `Synth::boxed` exists only with the `alloc` feature.
        let _synth = synth_core::Synth::<1>::boxed(48_000.0, preset.patch);
    }
}
