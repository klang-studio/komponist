//! Shared file handles and waveform meshes for visible audio clips.
use komponist_audiofile::{FilePool, PeakData, PeakState, PoolFile};
use komponist_model::{AudioFile, Clip, ClipContent, Tempo};
use komponist_ui::timeline::{AudioFileState, WaveMesh, WaveSpec};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug)]
struct FileEntry {
    checked: Instant,
    file: Option<Arc<PoolFile>>,
}
#[derive(Debug)]
struct WaveEntry {
    clip: Arc<Clip>,
    peaks: Option<Arc<PeakData>>,
    spec: WaveSpec,
    mesh: WaveMesh,
    used: u64,
}
/// Files are rechecked once a second, allowing missing links to come back online.
#[derive(Debug, Default)]
pub struct AudioCache {
    files: HashMap<(AudioFile, Option<PathBuf>), FileEntry>,
    waves: HashMap<usize, WaveEntry>,
    frame: u64,
}
impl AudioCache {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }
    pub fn end_frame(&mut self) {
        self.waves.retain(|_, e| e.used == self.frame);
        // Bound file handles to visible clips; the global pool shares active engine handles.
        self.files.retain(|(file, _), _| {
            self.waves
                .values()
                .any(|e| matches!(&e.clip.content,ClipContent::Audio(a) if &a.file==file))
        });
    }
    pub fn get(
        &mut self,
        clip: &Arc<Clip>,
        tempo: Tempo,
        dir: Option<&Path>,
        ppt: f32,
        height: f32,
        ctx: &egui::Context,
    ) -> Option<(AudioFileState, &WaveMesh)> {
        let ClipContent::Audio(a) = &clip.content else {
            return None;
        };
        let key = (a.file.clone(), dir.map(Path::to_path_buf));
        let file = self.files.entry(key).or_insert_with(|| FileEntry {
            checked: Instant::now() - Duration::from_secs(2),
            file: None,
        });
        if file.checked.elapsed() >= Duration::from_secs(1) {
            file.checked = Instant::now();
            file.file = a
                .file
                .resolve(dir)
                .and_then(|p| FilePool::global().open(&p).ok());
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        let (state, peaks) = match &file.file {
            None => (AudioFileState::Missing, None),
            Some(file) => {
                let state = file.peaks_state();
                // Register only once, not on every pending frame (callbacks live until completion).
                let state = if matches!(state, PeakState::Idle) {
                    let ctx = ctx.clone();
                    file.request_peaks(move || ctx.request_repaint())
                } else {
                    state
                };
                match state {
                    PeakState::Ready(p) => (AudioFileState::Ready, Some(p)),
                    PeakState::Failed(_) => (AudioFileState::Missing, None),
                    _ => (AudioFileState::Pending, None),
                }
            }
        };
        let spec = WaveSpec {
            width: (clip.length.0 as f32 * ppt
                - 2.0 * komponist_ui::widgets::metrics::timeline::CLIP_INSET_X)
                .max(komponist_ui::widgets::metrics::timeline::CLIP_MIN_WIDTH),
            px_per_tick: ppt,
            body_height: height,
            offset: a.offset,
            frames: a.source_frames(clip.length, tempo),
            gain: 10.0f32.powf(a.gain_db / 20.0),
            fade_in: a.fade_in.0 as f32 * ppt,
            fade_out: a.fade_out.0 as f32 * ppt,
        };
        let entry = self
            .waves
            .entry(Arc::as_ptr(clip) as usize)
            .or_insert_with(|| WaveEntry {
                clip: clip.clone(),
                peaks: None,
                spec,
                mesh: WaveMesh::flat(spec.width, ppt, height),
                used: 0,
            });
        let changed = match (&entry.peaks, &peaks) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if entry.used == 0 || entry.spec != spec || changed {
            entry.mesh = peaks.as_ref().map_or_else(
                || WaveMesh::flat(spec.width, ppt, height),
                |p| WaveMesh::from_peaks(p, &spec),
            );
            entry.peaks = peaks;
            entry.spec = spec;
        }
        entry.used = self.frame;
        Some((state, &entry.mesh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::edits::{self, NewAudioTrack, NewClip, NewClipContent};
    use komponist_model::{Project, Tick};
    fn clip(path: &Path) -> Arc<Clip> {
        let (p, t) =
            edits::add_audio_track(&Project::new("cache"), NewAudioTrack::default()).unwrap();
        let (p, c) = edits::add_clip(
            &p,
            t,
            NewClip {
                start: Tick::ZERO,
                length: Tick(3840),
                name: "wave".into(),
                content: NewClipContent::Audio {
                    file: AudioFile::new(path, None, 96000, 48000, 1),
                    offset: 0,
                },
            },
        )
        .unwrap();
        p.find_clip(c).unwrap().1.clone()
    }
    #[test]
    fn fixed_tempo_changes_rebuild_the_same_clip_window() {
        let c = clip(Path::new("/missing/cache-test.wav"));
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        cache
            .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
            .unwrap();
        assert_eq!(cache.waves.values().next().unwrap().spec.frames, 96000);
        cache
            .get(&c, Tempo::new(240.0), None, 0.02, 36.0, &ctx)
            .unwrap();
        assert_eq!(cache.waves.values().next().unwrap().spec.frames, 48000);
        cache.end_frame();
        assert_eq!(cache.waves.len(), 1);
        cache.begin_frame();
        cache.end_frame();
        assert!(cache.waves.is_empty());
        assert!(cache.files.is_empty());
    }
    #[test]
    fn restored_files_and_ready_peaks_replace_the_flat_missing_mesh() {
        let path = std::env::temp_dir().join(format!(
            "komponist-wave-cache-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _remove = Remove(path.clone());
        let c = clip(&path);
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        assert_eq!(
            cache
                .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
                .unwrap()
                .0,
            AudioFileState::Missing
        );
        let mut wav = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..96000 {
            wav.write_sample(if i % 2 == 0 { 16000i16 } else { -16000i16 })
                .unwrap();
        }
        wav.finalize().unwrap();
        for f in cache.files.values_mut() {
            f.checked = Instant::now() - Duration::from_secs(2);
        }
        cache
            .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (state, mesh) = cache
                .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
                .unwrap();
            if state == AudioFileState::Ready {
                assert!(!mesh.is_empty());
                break;
            }
            assert!(Instant::now() < deadline, "peak worker never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
        let before = cache.waves.values().next().unwrap().peaks.clone().unwrap();
        // A different-size replacement at the same path receives a different FileId and peaks.
        let mut wav = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..48000 {
            wav.write_sample(0i16).unwrap();
        }
        wav.finalize().unwrap();
        for f in cache.files.values_mut() {
            f.checked = Instant::now() - Duration::from_secs(2);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (state, _) = cache
                .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
                .unwrap();
            if state == AudioFileState::Ready {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!Arc::ptr_eq(
            &before,
            cache.waves.values().next().unwrap().peaks.as_ref().unwrap()
        ));
    }
}
