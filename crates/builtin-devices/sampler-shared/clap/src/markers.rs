//! Main-thread waveform marker editing over the sampler's saved slice settings.

use std::ffi::c_void;
use std::ops::Range;

use clap_sys::plugin::clap_plugin;
use komponist_ipc::waveform_markers::{
    Edit, EditStatus, Extension, MAX_EDIT_BYTES, MAX_MARKERS, MAX_SNAPSHOT_BYTES, Preparation,
    Request, SliceOptions as WireOptions, Snapshot,
};
use komponist_sampler_shared::{
    PadStatus, SyncStatus,
    settings::{MAX_FADE_MS, MAX_SLICES, Settings, Slice, SliceOptions},
};
use nice_plug::wrapper::clap::Wrapper;

use super::{MAX_SAMPLE_ASSET_BYTES, SampleAssets, SampleParameters, Sampled, SampledDefinition};

const _: () = assert!(MAX_SLICES == MAX_MARKERS);
const _: () = assert!(MAX_FADE_MS == komponist_ipc::waveform_markers::MAX_FADE_MS);

#[derive(Default)]
pub(super) struct MarkerState {
    revision: u64,
    pub settings: Settings,
}

impl MarkerState {
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

pub(super) fn extension<T: SampledDefinition>() -> *const c_void {
    let table: &'static Extension = &Extension {
        snapshot: Some(snapshot::<T>),
        edit: Some(edit::<T>),
    };
    std::ptr::from_ref(table).cast()
}

unsafe extern "C" fn snapshot<T: SampledDefinition>(
    plugin: *const c_void,
    slot: u32,
    out: *mut u8,
    capacity: u32,
) -> u32 {
    if plugin.is_null() || slot != 0 || out.is_null() || capacity == 0 {
        return 0;
    }
    // SAFETY: This table is registered by the wrapper for its live CLAP instance. The caller
    // supplies a writable buffer of at least capacity bytes and retains it through the call.
    unsafe {
        Wrapper::<Sampled<T>>::with_custom_extension_data::<SampleParameters<T>, _>(
            plugin.cast::<clap_plugin>(),
            |params| {
                let capacity = (capacity as usize).min(MAX_SNAPSHOT_BYTES);
                let out = std::slice::from_raw_parts_mut(out, capacity);
                params
                    .marker_snapshot()
                    .and_then(|snapshot| snapshot.encode(out))
                    .map_or(0, |len| len as u32)
            },
        )
    }
    .unwrap_or(0)
}

unsafe extern "C" fn edit<T: SampledDefinition>(
    plugin: *const c_void,
    slot: u32,
    input: *const u8,
    length: u32,
) -> u32 {
    let length = length as usize;
    if plugin.is_null() || slot != 0 || input.is_null() || !(1..=MAX_EDIT_BYTES).contains(&length) {
        return EditStatus::Invalid as u32;
    }
    // SAFETY: The caller supplies a readable buffer of length bytes through this callback.
    let Some(request) = Request::decode(unsafe { std::slice::from_raw_parts(input, length) })
    else {
        return EditStatus::Invalid as u32;
    };
    // SAFETY: This table is registered by the wrapper for its live CLAP instance.
    unsafe {
        Wrapper::<Sampled<T>>::with_custom_extension_data::<SampleParameters<T>, _>(
            plugin.cast::<clap_plugin>(),
            |params| params.edit_marker(request),
        )
    }
    .unwrap_or(EditStatus::Invalid) as u32
}

impl<T: SampledDefinition> SampleParameters<T> {
    fn slice_mode(&self) -> bool {
        self.params.plain_value_by_id("mode") == Some(2.0)
    }

    fn marker_duration(&self) -> Option<f64> {
        let view = self.samples.view(0);
        (view.status == PadStatus::Ready && view.length_s.is_finite() && view.length_s > 0.0)
            .then_some(view.length_s)
    }

    fn marker_snapshot(&self) -> Option<Snapshot> {
        let assets = self
            .assets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = assets.markers.as_ref()?;
        let slice_mode = self.slice_mode();
        let plain = |id| self.params.plain_value_by_id(id).unwrap_or_default();
        Some(Snapshot {
            revision: state.revision,
            editable: slice_mode && self.marker_duration().is_some(),
            positions: state
                .settings
                .slices
                .iter()
                .map(|slice| slice.time)
                .collect(),
            slice_options: slice_mode.then(|| {
                std::iter::once(state.settings.first)
                    .chain(state.settings.slices.iter().map(|slice| slice.options))
                    .map(|options| WireOptions {
                        gate: options.gate,
                        reverse: options.reverse,
                        fade_in_ms: options.fade_in_ms,
                        fade_out_ms: options.fade_out_ms,
                    })
                    .collect()
            }),
            default_options: Some(WireOptions {
                gate: false,
                reverse: Some(plain("direction") == 1.0),
                fade_in_ms: Some(plain("fade_in") as f32),
                fade_out_ms: Some(plain("fade_out") as f32),
            }),
            preparation: if plain("sync") >= 0.5 {
                match self.samples.sync_status() {
                    Some(SyncStatus::Preparing) => Some(Preparation::Preparing),
                    Some(SyncStatus::Unavailable) => Some(Preparation::Unavailable),
                    _ => None,
                }
            } else {
                None
            },
        })
    }

    fn edit_marker(&self, request: Request) -> EditStatus {
        let mut assets = self
            .assets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let SampleAssets { document, markers } = &mut *assets;
        let Some((state, codec)) = markers.as_mut().zip(T::SLICE_SETTINGS) else {
            return EditStatus::Unavailable;
        };
        if request.revision != state.revision {
            return EditStatus::Stale;
        }
        let Some(view_duration) = self.marker_duration().filter(|_| self.slice_mode()) else {
            return EditStatus::Unavailable;
        };
        let (duration, sample_rate) =
            self.samples
                .waveform_peaks()
                .map_or((view_duration, 48_000), |peaks| {
                    let rate = peaks.sample_rate();
                    (peaks.frames() as f64 / f64::from(rate), rate)
                });
        let plain = |id| self.params.plain_value_by_id(id).unwrap_or_default();
        let trim = plain("start") * duration..plain("end") * duration;
        let spacing = 1.0 / f64::from(sample_rate);
        let mut settings = state.settings.clone();
        let status = update(
            &mut settings,
            request.operation,
            duration,
            trim,
            spacing,
            || self.samples.transients(),
        );
        if status != EditStatus::Applied {
            return status;
        }
        let updated = (codec.write)(document.as_bytes(), &settings);
        if updated.len() > MAX_SAMPLE_ASSET_BYTES {
            return EditStatus::Invalid;
        }
        let Ok(updated) = String::from_utf8(updated) else {
            return EditStatus::Invalid;
        };
        self.samples.set_general_settings(&settings);
        *document = updated;
        state.settings = settings;
        state.invalidate();
        EditStatus::Applied
    }
}

fn update(
    settings: &mut Settings,
    operation: Edit,
    duration: f64,
    trim: Range<f64>,
    spacing: f64,
    transients: impl FnOnce() -> Vec<f64>,
) -> EditStatus {
    match operation {
        Edit::Insert { seconds } => {
            let slices = &mut settings.slices;
            let index = slices.partition_point(|slice| slice.time < seconds);
            let previous = index.checked_sub(1).map(|i| &slices[i]);
            if slices.len() == MAX_MARKERS
                || !(trim.start < seconds && seconds < trim.end)
                || previous.is_some_and(|slice| seconds - slice.time < spacing)
                || slices
                    .get(index)
                    .is_some_and(|slice| slice.time - seconds < spacing)
            {
                return EditStatus::Invalid;
            }
            let options = previous.map_or(settings.first, |slice| slice.options);
            slices.insert(
                index,
                Slice {
                    time: seconds,
                    options,
                },
            );
        }
        Edit::Move { index, seconds } => {
            let slices = &mut settings.slices;
            let index = usize::from(index);
            let Some(current) = slices.get(index) else {
                return EditStatus::Invalid;
            };
            if seconds == current.time {
                return EditStatus::Unchanged;
            }
            // Preserve the legacy drag limits, including already saved positions closer than
            // one frame and the ability to reach the exact source end on the last marker.
            let previous = index.checked_sub(1).map(|i| &slices[i]);
            let next = slices.get(index + 1);
            let lower = previous
                .map_or(spacing, |slice| slice.time + spacing)
                .min(current.time);
            let upper = next
                .map_or(duration, |next| next.time - spacing)
                .max(current.time);
            if !(lower..=upper).contains(&seconds)
                || previous.is_some_and(|slice| seconds <= slice.time)
                || next.is_some_and(|slice| seconds >= slice.time)
            {
                return EditStatus::Invalid;
            }
            slices[index].time = seconds;
        }
        Edit::Remove { index } => {
            let index = usize::from(index);
            if index >= settings.slices.len() {
                return EditStatus::Invalid;
            }
            settings.slices.remove(index);
        }
        Edit::SetSliceOptions { slice, options } => {
            let slice = usize::from(slice);
            if slice > settings.slices.len() {
                return EditStatus::Invalid;
            }
            let current = settings.options_mut(slice.checked_sub(1));
            let updated = SliceOptions {
                gate: options.gate,
                reverse: options.reverse,
                fade_in_ms: options.fade_in_ms,
                fade_out_ms: options.fade_out_ms,
            };
            if *current == updated {
                return EditStatus::Unchanged;
            }
            *current = updated;
        }
        Edit::DetectSlices => {
            let slices: Vec<_> = transients()
                .into_iter()
                .filter(|&time| trim.start < time && time < trim.end)
                .take(MAX_MARKERS)
                .map(|time| {
                    let options = settings
                        .slices
                        .binary_search_by(|slice| slice.time.total_cmp(&time))
                        .ok()
                        .map_or_else(SliceOptions::default, |index| {
                            settings.slices[index].options
                        });
                    Slice { time, options }
                })
                .collect();
            if slices == settings.slices {
                return EditStatus::Unchanged;
            }
            settings.slices = slices;
        }
    }
    EditStatus::Applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_uses_strict_trim_and_preserves_the_matching_and_first_slice_options() {
        let mut settings = Settings::from_markers([0.1, 0.5, 0.9]);
        settings.first.gate = true;
        settings.slices[1].options.reverse = Some(true);
        let detect = |settings: &mut Settings| {
            update(settings, Edit::DetectSlices, 1.0, 0.1..0.9, 0.001, || {
                vec![0.1, 0.5, 0.75, 0.9]
            })
        };
        assert_eq!(detect(&mut settings), EditStatus::Applied);
        assert!(settings.first.gate);
        assert_eq!(settings.slices[0].time, 0.5);
        assert_eq!(settings.slices[0].options.reverse, Some(true));
        assert_eq!(settings.slices[1].time, 0.75);
        assert_eq!(settings.slices[1].options, SliceOptions::default());
        assert_eq!(settings.slices.len(), 2);
        assert_eq!(detect(&mut settings), EditStatus::Unchanged);
    }
}
