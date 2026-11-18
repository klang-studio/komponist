//! One song/source projection for painting, seeking, readouts and viewport bounds.
//! Native-frame edits retain the model's exact phase-aware integer mapping.
use komponist_model::{AudioClip, Clip, PPQ, Tempo, Tick, WarpMap};

#[derive(Clone, Copy)]
pub struct Mapping<'a> {
    audio: &'a AudioClip,
    start: Tick,
    tempo: Tempo,
    warp: Option<WarpMap>,
}
impl<'a> Mapping<'a> {
    pub fn new(clip: &Clip, audio: &'a AudioClip, tempo: Tempo) -> Self {
        Self {
            audio,
            start: clip.start,
            tempo,
            warp: audio.warp_map(audio.active_take).ok(),
        }
    }
    fn frames_per_tick(self) -> f64 {
        f64::from(self.audio.file.sample_rate) * self.audio.speed(self.tempo) * 60.0
            / (self.tempo.bpm * PPQ as f64)
    }
    pub fn song_at_frame(self, frame: f64) -> f64 {
        self.start.0 as f64
            + self.warp.map_or_else(
                || (frame - self.audio.offset as f64) / self.frames_per_tick(),
                |m| m.tick_at_source_position(frame),
            )
    }
    pub fn frame_at_song(self, tick: f64) -> f64 {
        let local = tick - self.start.0 as f64;
        self.warp.map_or_else(
            || self.audio.offset as f64 + local * self.frames_per_tick(),
            |m| m.source_position(local),
        )
    }
    /// Whether a requested song position has a real native-file coordinate. Display callers
    /// must not pair a clamped source value with a song tick outside this domain.
    pub fn contains_song(self, tick: Tick) -> bool {
        if let Some(m) = self.warp {
            m.source_frame(tick - self.start)
                .is_ok_and(|f| f <= self.audio.file.frames)
        } else {
            let frame = self.frame_at_song(tick.0 as f64).round();
            frame >= 0.0 && frame <= self.audio.file.frames as f64
        }
    }
    pub fn source_frame(self, tick: Tick) -> u64 {
        let local = tick - self.start;
        self.warp
            .map_or_else(
                || self.frame_at_song(tick.0 as f64).round().max(0.0) as u64,
                |m| m.source_frame(local).unwrap_or(0),
            )
            .min(self.audio.file.frames)
    }
    pub fn source_seconds(self, tick: Tick) -> f64 {
        self.source_frame(tick) as f64 / f64::from(self.audio.file.sample_rate)
    }
    pub fn bounds(self) -> std::ops::Range<f64> {
        self.song_at_frame(0.0)..self.song_at_frame(self.audio.file.frames as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{AudioFile, ClipContent, Project, edits};
    use std::path::Path;
    #[test]
    fn phase_projection_and_inverse_follow_native_mapping_across_rates_and_tempos() {
        for rate in [8000, 44100, 48000, 384000] {
            for original in [40.0, 90.0, 180.0, 240.0] {
                for project in [40.0, 120.0, 240.0] {
                    let mut p = Project::new("projection");
                    p.tempo = Tempo::new(project);
                    let (p, t) =
                        edits::add_audio_track(&p, edits::NewAudioTrack::default()).unwrap();
                    let (p, c) = edits::add_clip(
                        &p,
                        t,
                        edits::NewClip {
                            start: Tick(3840),
                            length: Tick(7680),
                            name: "test".into(),
                            content: edits::NewClipContent::Audio {
                                file: AudioFile::new(
                                    Path::new("/missing.wav"),
                                    None,
                                    u64::from(rate) * 8,
                                    rate,
                                    2,
                                ),
                                offset: u64::from(rate) / 3,
                            },
                        },
                    )
                    .unwrap();
                    let p = edits::set_audio_clip_prop(
                        &p,
                        c,
                        edits::AudioClipProp::OriginalTempo(Tempo::new(original)),
                    )
                    .unwrap();
                    let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
                    let cut = Tick(3977);
                    let q = edits::trim_audio_clip_start(&p, c, cut).unwrap();
                    let clip = p.find_clip(c).unwrap().1;
                    let ClipContent::Audio(a) = &clip.content else {
                        unreachable!()
                    };
                    let trimmed = q.find_clip(c).unwrap().1;
                    let ClipContent::Audio(b) = &trimmed.content else {
                        unreachable!()
                    };
                    let old = Mapping::new(clip, a, p.tempo);
                    let new = Mapping::new(trimmed, b, p.tempo);
                    for delta in [0, 17, 137, 960] {
                        let song = cut + Tick(delta);
                        assert_eq!(old.source_frame(song), new.source_frame(song));
                        assert!(
                            (old.frame_at_song(song.0 as f64) - new.frame_at_song(song.0 as f64))
                                .abs()
                                < 1e-6
                        );
                        let native = new.source_frame(song);
                        assert!(
                            (new.frame_at_song(song.0 as f64) - native as f64).abs() <= 0.500001
                        );
                        let position = song.0 as f64 + 0.123;
                        assert!(
                            (new.song_at_frame(new.frame_at_song(position)) - position).abs()
                                < 1e-8
                        );
                    }
                    assert!((new.frame_at_song(new.bounds().start)).abs() < 1e-6);
                    assert!(
                        (new.frame_at_song(new.bounds().end) - b.file.frames as f64).abs() < 1e-6
                    );
                }
            }
        }
    }
}
