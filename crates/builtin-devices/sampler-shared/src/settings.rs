//! Slice settings shared by the sample loader and the chromatic sampler.

/// At most 127 interior markers fit the 128-note MIDI range.
pub const MAX_SLICES: usize = 127;

/// Overrides for the slice that begins at a boundary. Missing values inherit
/// the sample-wide direction and fades used by existing projects.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliceOptions {
    pub reverse: Option<bool>,
    pub fade_in_ms: Option<f32>,
    pub fade_out_ms: Option<f32>,
    pub gate: bool,
}

pub const MAX_FADE_MS: f32 = 10_000.0;

/// An interior boundary in source seconds and the options of the slice to its right.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slice {
    pub time: f64,
    pub options: SliceOptions,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// The first slice begins at the playback start.
    pub first: SliceOptions,
    pub slices: Vec<Slice>,
}

impl Settings {
    pub fn from_markers(markers: impl IntoIterator<Item = f64>) -> Self {
        let mut settings = Self {
            slices: markers
                .into_iter()
                .map(|time| Slice {
                    time,
                    ..Slice::default()
                })
                .collect(),
            ..Self::default()
        };
        settings.normalize();
        settings
    }

    pub fn options(&self, boundary: Option<usize>) -> SliceOptions {
        boundary.map_or(self.first, |index| self.slices[index].options)
    }

    pub fn options_mut(&mut self, boundary: Option<usize>) -> &mut SliceOptions {
        match boundary {
            Some(index) => &mut self.slices[index].options,
            None => &mut self.first,
        }
    }

    /// Sorts, deduplicates and limits the interior source positions.
    pub fn normalize(&mut self) {
        self.slices
            .retain(|slice| slice.time.is_finite() && slice.time > 0.0);
        self.slices.sort_by(|a, b| a.time.total_cmp(&b.time));
        self.slices.dedup_by(|a, b| a.time == b.time);
        self.slices.truncate(MAX_SLICES);
    }
}
