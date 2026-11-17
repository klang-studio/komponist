//! Non-RT source metadata and peak worker access, rechecked once per second.
use crate::views::ViewCtx;
use komponist_audiofile::{FilePool, PeakData, PeakState, PoolFile};
use komponist_model::{AudioClip, AudioFile};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
pub struct Source {
    key: Option<(AudioFile, Option<PathBuf>)>,
    checked: Option<Instant>,
    /// Shared decoding/peak handle, absent for missing or unreadable files.
    pub file: Option<Arc<PoolFile>>,
    /// PCM bit depth where the container exposes it (WAV).
    pub bits: Option<u16>,
}
impl Source {
    pub fn refresh(&mut self, a: &AudioClip, ctx: &ViewCtx<'_>, egui: &egui::Context) {
        let key = (a.file.clone(), ctx.project_dir.map(ToOwned::to_owned));
        if self.key.as_ref() != Some(&key)
            || self
                .checked
                .is_none_or(|i| i.elapsed() >= Duration::from_secs(1))
        {
            self.key = Some(key);
            self.checked = Some(Instant::now());
            self.file = a
                .file
                .resolve(ctx.project_dir)
                .and_then(|p| FilePool::global().open(&p).ok());
            self.bits = self.file.as_ref().and_then(|f| {
                hound::WavReader::open(f.path())
                    .ok()
                    .map(|r| r.spec().bits_per_sample)
            });
        }
        egui.request_repaint_after(Duration::from_secs(1));
    }
    pub fn peaks(&self, ctx: &egui::Context) -> Option<Arc<PeakData>> {
        let file = self.file.as_ref()?;
        let state = file.peaks_state();
        let state = if matches!(state, PeakState::Idle) {
            let ctx = ctx.clone();
            file.request_peaks(move || ctx.request_repaint())
        } else {
            state
        };
        match state {
            PeakState::Ready(p) => Some(p),
            _ => None,
        }
    }
    pub fn path(&self, a: &AudioClip) -> PathBuf {
        self.file
            .as_ref()
            .map_or_else(|| a.file.abs.clone(), |f| f.path().to_owned())
    }
    pub fn metadata(&self, a: &AudioClip, t: komponist_i18n::Lang) -> String {
        let rate = crate::project_io::fmt_bpm(f64::from(a.file.sample_rate) / 1000.0);
        let channels = if a.file.channels == 1 {
            t.audio_channel_mono()
        } else {
            t.audio_channel_stereo()
        };
        let duration =
            komponist_model::format::fmt_time(a.file.frames as f64 / f64::from(a.file.sample_rate));
        let mut meta = t.audio_file_metadata(rate, channels, duration);
        if let Some(bits) = self.bits {
            meta.push_str(" · ");
            meta.push_str(&t.audio_file_bit_depth(bits));
        }
        meta
    }
}
