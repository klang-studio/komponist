use komponist_native_clap_sdk::display::{Display, DisplaySource};

use super::{SampleParameters, SampledDefinition};

impl<T: SampledDefinition> DisplaySource for SampleParameters<T> {
    fn paint(&self, display: &mut Display) {
        if let Some(paint) = T::DISPLAY {
            paint(&self.params, display);
        }
    }
}
