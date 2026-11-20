//! Pitch conversions, see [`klang_dsp::units`].

pub use klang_dsp::units::note_to_freq;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_notes() {
        assert!((note_to_freq(69.0) - 440.0).abs() < 1e-3);
        assert!((note_to_freq(81.0) - 880.0).abs() < 1e-2);
        assert!((note_to_freq(60.0) - 261.6256).abs() < 1e-2);
    }
}
