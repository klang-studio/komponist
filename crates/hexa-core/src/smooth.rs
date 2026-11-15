//! One-pole smoothing of parameters towards their patch values, once per
//! block, see [`klang_dsp::one_pole`].

pub(crate) use klang_dsp::one_pole::chase;

/// The `amount` for [`chase`] that follows a step with time constant
/// `time` seconds, called every `block_seconds`.
#[inline]
pub(crate) fn amount(block_seconds: f32, time: f32) -> f32 {
    1.0 - klang_dsp::one_pole::time_coeff(block_seconds, time)
}
