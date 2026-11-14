//! Per-view caches: track palettes by hue and clip previews by clip.
//!
//! A clip preview ([`PreviewMesh`]) is built once per (clip `Arc` pointer, zoom, body height)
//! (ARCHITECTURE §9.2). The document is immutable, so an edited clip is a new `Arc` and misses the
//! cache by itself. The cache holds a clone of each clip's `Arc`, so a pointer cannot be reused by
//! another clip while its entry lives; entries not used in a frame are dropped at its end.

use std::collections::HashMap;
use std::sync::Arc;

use komponist_model::Clip;
use komponist_tokens::TrackPalette;
use komponist_ui::timeline::PreviewMesh;

/// Track palettes, computed once per hue.
#[derive(Debug, Default)]
pub struct Palettes {
    by_hue: HashMap<u16, TrackPalette>,
}

impl Palettes {
    /// The palette of `hue` (degrees).
    pub fn get(&mut self, hue: u16) -> &TrackPalette {
        self.by_hue
            .entry(hue)
            .or_insert_with(|| TrackPalette::new(hue))
    }
}

#[derive(Debug)]
struct Entry {
    clip: Arc<Clip>,
    mesh: PreviewMesh,
    used: u64,
}

/// Clip previews of the visible clips.
#[derive(Debug, Default)]
pub struct PreviewCache {
    entries: HashMap<usize, Entry>,
    frame: u64,
    built: usize,
}

impl PreviewCache {
    /// Starts a frame: counts builds from zero.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.built = 0;
    }

    /// The preview of `clip` at `px_per_tick` for a body of `body_height`, built if missing or
    /// made for another zoom or height.
    pub fn get(&mut self, clip: &Arc<Clip>, px_per_tick: f32, body_height: f32) -> &PreviewMesh {
        let key = Arc::as_ptr(clip) as usize;
        let frame = self.frame;
        let fresh = |built: &mut usize| {
            *built += 1;
            PreviewMesh::from_clip(clip, px_per_tick, body_height)
        };
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            clip: Arc::clone(clip),
            mesh: PreviewMesh::default(),
            used: 0,
        });
        if entry.used == 0
            || !Arc::ptr_eq(&entry.clip, clip)
            || !entry.mesh.fits(px_per_tick, body_height)
        {
            entry.clip = Arc::clone(clip);
            entry.mesh = fresh(&mut self.built);
        }
        entry.used = frame;
        &entry.mesh
    }

    /// Ends a frame: drops the previews of clips not shown in it.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, e| e.used == frame);
    }

    /// Number of cached previews.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Previews built in the current frame (cache misses).
    #[cfg(test)]
    pub fn built(&self) -> usize {
        self.built
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    #[test]
    fn previews_are_built_once_per_clip_and_zoom() {
        let p = demo_project();
        let clips: Vec<Arc<Clip>> = p.tracks[2].clips.clone();
        let mut cache = PreviewCache::default();
        cache.begin_frame();
        for c in &clips {
            cache.get(c, 72.0 / 3840.0, 36.0);
        }
        assert_eq!(cache.built(), 3);
        cache.end_frame();
        cache.begin_frame();
        for c in &clips {
            cache.get(c, 72.0 / 3840.0, 36.0);
        }
        assert_eq!(cache.built(), 0, "same clips, same zoom");
        // Another zoom rebuilds; an unused clip is dropped at the end of the frame.
        cache.get(&clips[0], 90.0 / 3840.0, 36.0);
        assert_eq!(cache.built(), 1);
        cache.end_frame();
        assert_eq!(cache.len(), 3);
        cache.begin_frame();
        cache.get(&clips[0], 90.0 / 3840.0, 36.0);
        cache.end_frame();
        assert_eq!(cache.len(), 1);
        // An edited clip is a new Arc: a miss.
        cache.begin_frame();
        let edited = Arc::new(Clip::clone(&clips[0]));
        cache.get(&edited, 90.0 / 3840.0, 36.0);
        assert_eq!(cache.built(), 1);
    }

    #[test]
    fn palettes_are_kept() {
        let mut p = Palettes::default();
        let base = p.get(150).base;
        assert_eq!(base, TrackPalette::new(150).base);
        assert_eq!(p.by_hue.len(), 1);
        p.get(150);
        assert_eq!(p.by_hue.len(), 1);
    }
}
