//! Size negotiation between a plugin GUI and its frame window (ARCHITECTURE §8.4, spike WP-15
//! §1.4), as a state machine without X11 or egui so it can be tested on its own.
//!
//! All sizes are physical pixels: the plugin reports and takes physical pixels on X11, and the
//! frame converts to points with its `pixels_per_point`.
//!
//! - **The plugin decides** ([`Sizing::opened`], [`Sizing::requested`]): the frame follows. The
//!   X11 child gets the plugin's size, the frame gets the plugin size plus the header, at least
//!   as wide as the header needs. A fixed-size plugin locks the frame (min = max inner size, not
//!   resizable), so the user cannot drag it and nothing snaps back. The frame never answers a
//!   plugin's request with `set_gui_size`: CLAP does not require it, VST3's `resizeView` has
//!   already resized the view in the bridge, and Dexed complains about it (spike pitfall 2). A
//!   `GuiSizeRequest` that answers our own `set_gui_size` (Surge XT CLAP adds 1 px) is followed
//!   the same way, so no ping-pong can start.
//! - **The user drags the frame** ([`Sizing::observed`]): egui reports the new inner size late,
//!   so sizes are ignored until the size the frame asked for shows up (or [`STALE`] passed).
//!   For a resizable plugin the child follows at once (no flash of a stale area), and the plugin
//!   hears the new size once the drag pauses for [`DEBOUNCE`] ([`Sizing::tick`]); the size it
//!   adopts comes back as a request. A fixed-size plugin's child is never resized (baseview
//!   answers a parent resize with its own request, spike pitfall 3).

use std::time::{Duration, Instant};

use komponist_ipc::GuiSize;

/// How long a reported frame size may still be the old one after the frame asked for a new one.
pub const STALE: Duration = Duration::from_millis(500);

/// How long a user resize must rest before the plugin hears the new size.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// A size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Px {
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl Px {
    /// `w` × `h`.
    pub const fn new(w: u32, h: u32) -> Px {
        Px { w, h }
    }

    /// Equal up to 1 px in each direction (rounding between points and pixels).
    pub fn close_to(self, other: Px) -> bool {
        self.w.abs_diff(other.w) <= 1 && self.h.abs_diff(other.h) <= 1
    }
}

impl From<GuiSize> for Px {
    fn from(s: GuiSize) -> Px {
        Px::new(s.width, s.height)
    }
}

impl From<Px> for GuiSize {
    fn from(p: Px) -> GuiSize {
        GuiSize {
            width: p.w,
            height: p.h,
        }
    }
}

/// The frame's own measures in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chrome {
    /// Header height.
    pub header: u32,
    /// Width the header needs.
    pub min_width: u32,
}

/// What the frame has to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Resize the frame window to `inner` (header included); `locked` = min = max = `inner`,
    /// not resizable.
    Frame {
        /// Inner size.
        inner: Px,
        /// Fixed size.
        locked: bool,
    },
    /// Resize the X11 child (the plugin area) to this size.
    Child(Px),
    /// Tell the plugin its new size (`DeviceHost::set_gui_size`).
    Tell(GuiSize),
}

/// The negotiation state of one frame.
#[derive(Debug, Clone, Default)]
pub struct Sizing {
    /// The plugin's size as last known.
    plugin: Option<Px>,
    /// Whether the plugin may be resized.
    resizable: bool,
    /// The frame size we asked for and since when; reported sizes before it are stale.
    expect: Option<(Instant, Px)>,
    /// The last frame size acted on.
    seen: Option<Px>,
    /// A user resize waiting for [`DEBOUNCE`]: since when, and the new plugin area.
    pending: Option<(Instant, Px)>,
}

impl Sizing {
    /// A frame whose plugin has not reported a size yet.
    pub fn new() -> Sizing {
        Sizing::default()
    }

    /// The plugin's size as last known.
    pub fn plugin(&self) -> Option<Px> {
        self.plugin
    }

    /// Whether the plugin may be resized.
    pub fn resizable(&self) -> bool {
        self.resizable
    }

    /// The frame size for a plugin of `plugin` px: the header on top, at least the header's
    /// width.
    pub fn frame_for(plugin: Px, chrome: Chrome) -> Px {
        Px::new(
            plugin.w.max(chrome.min_width),
            plugin.h.saturating_add(chrome.header),
        )
    }

    /// The GUI opened at `size`.
    pub fn opened(
        &mut self,
        size: GuiSize,
        resizable: bool,
        chrome: Chrome,
        now: Instant,
    ) -> Vec<Effect> {
        self.resizable = resizable;
        self.follow(size.into(), chrome, now)
    }

    /// The plugin asked for `size` (on its own, or as the answer to [`Effect::Tell`]).
    pub fn requested(&mut self, size: GuiSize, chrome: Chrome, now: Instant) -> Vec<Effect> {
        self.follow(size.into(), chrome, now)
    }

    /// The frame shows the plugin again (view switch, new header width): the frame follows the
    /// known plugin size.
    pub fn refit(&mut self, chrome: Chrome, now: Instant) -> Vec<Effect> {
        match self.plugin {
            Some(plugin) => self.follow(plugin, chrome, now),
            None => Vec::new(),
        }
    }

    fn follow(&mut self, plugin: Px, chrome: Chrome, now: Instant) -> Vec<Effect> {
        self.plugin = Some(plugin);
        self.pending = None;
        let inner = Sizing::frame_for(plugin, chrome);
        self.expect = Some((now, inner));
        vec![
            Effect::Child(plugin),
            Effect::Frame {
                inner,
                locked: !self.resizable,
            },
        ]
    }

    /// The frame window reports inner size `frame` (each frame of the window).
    pub fn observed(&mut self, frame: Px, chrome: Chrome, now: Instant) -> Vec<Effect> {
        if let Some((since, want)) = self.expect {
            if frame.close_to(want) {
                self.expect = None;
                self.seen = Some(frame);
                return Vec::new();
            }
            if now.duration_since(since) < STALE {
                return Vec::new();
            }
            // The window manager did not give us the size (tiling, screen edge): take what is.
            self.expect = None;
        }
        if self.seen.is_some_and(|s| s.close_to(frame)) {
            return Vec::new();
        }
        self.seen = Some(frame);
        if !self.resizable || self.plugin.is_none() {
            // A fixed-size plugin keeps its size; the frame is locked anyway.
            return Vec::new();
        }
        let area = Px::new(frame.w, frame.h.saturating_sub(chrome.header).max(1));
        self.pending = Some((now, area));
        vec![Effect::Child(area)]
    }

    /// Time passed: tells the plugin a user resize once it rests.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        match self.pending {
            Some((since, area)) if now.duration_since(since) >= DEBOUNCE => {
                self.pending = None;
                vec![Effect::Tell(area.into())]
            }
            _ => Vec::new(),
        }
    }

    /// Whether [`Sizing::tick`] has work later (the caller keeps repainting).
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.expect.is_some()
    }

    /// Forgets the frame size (the frame showed something else meanwhile); the plugin's size and
    /// resizability stay.
    pub fn reset_frame(&mut self) {
        self.expect = None;
        self.seen = None;
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: Chrome = Chrome {
        header: 35,
        min_width: 700,
    };

    fn gs(w: u32, h: u32) -> GuiSize {
        GuiSize {
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_fixed_size_plugin_locks_the_frame_and_never_hears_a_size() {
        let t = Instant::now();
        let mut s = Sizing::new();
        let fx = s.opened(gs(1100, 600), false, CHROME, t);
        assert_eq!(
            fx,
            [
                Effect::Child(Px::new(1100, 600)),
                Effect::Frame {
                    inner: Px::new(1100, 635),
                    locked: true
                }
            ]
        );
        // The old size is still reported for a moment, then the new one.
        assert!(s.observed(Px::new(820, 552), CHROME, t).is_empty());
        assert!(s.observed(Px::new(1100, 635), CHROME, t).is_empty());
        // A window manager forcing another size changes nothing: no snap-back, no set_size.
        let later = t + Duration::from_secs(2);
        assert!(s.observed(Px::new(1000, 500), CHROME, later).is_empty());
        assert!(s.tick(later + DEBOUNCE).is_empty());
    }

    #[test]
    fn a_narrow_plugin_gets_a_frame_as_wide_as_the_header() {
        let mut s = Sizing::new();
        let fx = s.opened(gs(240, 80), false, CHROME, Instant::now());
        assert_eq!(
            fx[1],
            Effect::Frame {
                inner: Px::new(700, 115),
                locked: true
            }
        );
        assert_eq!(fx[0], Effect::Child(Px::new(240, 80)));
    }

    #[test]
    fn stale_sizes_are_ignored_until_the_asked_size_shows_up() {
        let t = Instant::now();
        let mut s = Sizing::new();
        s.opened(gs(1300, 872), true, CHROME, t);
        // Old size reported right after our resize: not a user resize.
        assert!(
            s.observed(Px::new(820, 559), CHROME, t + Duration::from_millis(10))
                .is_empty()
        );
        assert!(s.tick(t + Duration::from_millis(200)).is_empty());
        // Off by one from rounding: still ours.
        assert!(s.observed(Px::new(1301, 906), CHROME, t).is_empty());
        assert!(!s.busy());
    }

    #[test]
    fn a_user_resize_moves_the_child_at_once_and_tells_the_plugin_after_the_drag_rests() {
        let t = Instant::now();
        let mut s = Sizing::new();
        s.opened(gs(1300, 872), true, CHROME, t);
        s.observed(Px::new(1300, 907), CHROME, t);
        let t1 = t + Duration::from_secs(1);
        assert_eq!(
            s.observed(Px::new(1500, 1035), CHROME, t1),
            [Effect::Child(Px::new(1500, 1000))]
        );
        // Still dragging.
        let t2 = t1 + Duration::from_millis(50);
        assert_eq!(
            s.observed(Px::new(1520, 1035), CHROME, t2),
            [Effect::Child(Px::new(1520, 1000))]
        );
        assert!(s.tick(t2 + Duration::from_millis(60)).is_empty());
        assert_eq!(s.tick(t2 + DEBOUNCE), [Effect::Tell(gs(1520, 1000))]);
        assert!(s.tick(t2 + DEBOUNCE * 2).is_empty());
        // Surge keeps its aspect ratio: the adopted size comes back as a request, the frame
        // follows and never tells again.
        let t3 = t2 + Duration::from_millis(300);
        assert_eq!(
            s.requested(gs(1487, 1000), CHROME, t3),
            [
                Effect::Child(Px::new(1487, 1000)),
                Effect::Frame {
                    inner: Px::new(1487, 1035),
                    locked: false
                }
            ]
        );
        assert!(s.observed(Px::new(1487, 1035), CHROME, t3).is_empty());
        assert!(s.tick(t3 + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_one_pixel_adjustment_after_set_size_is_followed_without_a_loop() {
        // Surge XT CLAP: set_size(1685) → it takes 1686 → GuiSizeRequest(1686).
        let t = Instant::now();
        let mut s = Sizing::new();
        s.opened(gs(1685, 1130), true, CHROME, t);
        s.observed(Px::new(1685, 1165), CHROME, t);
        let fx = s.requested(gs(1686, 1131), CHROME, t + Duration::from_millis(5));
        assert!(fx.iter().all(|e| !matches!(e, Effect::Tell(_))));
        assert_eq!(s.plugin(), Some(Px::new(1686, 1131)));
        assert!(
            s.observed(Px::new(1686, 1166), CHROME, t + Duration::from_millis(30))
                .is_empty()
        );
        assert!(s.tick(t + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_plugin_request_cancels_a_pending_user_resize() {
        let t = Instant::now();
        let mut s = Sizing::new();
        s.opened(gs(800, 600), true, CHROME, t);
        s.observed(Px::new(800, 635), CHROME, t);
        let t1 = t + Duration::from_secs(1);
        s.observed(Px::new(900, 700), CHROME, t1);
        s.requested(gs(1000, 700), CHROME, t1 + Duration::from_millis(10));
        assert!(s.tick(t1 + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn a_refused_frame_size_counts_as_a_user_resize_after_the_stale_time() {
        let t = Instant::now();
        let mut s = Sizing::new();
        s.opened(gs(2000, 1400), true, CHROME, t);
        // A tiling window manager keeps the frame at 1200 × 800.
        assert!(s.observed(Px::new(1200, 800), CHROME, t).is_empty());
        let late = t + STALE;
        assert_eq!(
            s.observed(Px::new(1200, 800), CHROME, late),
            [Effect::Child(Px::new(1200, 765))]
        );
        assert_eq!(s.tick(late + DEBOUNCE), [Effect::Tell(gs(1200, 765))]);
    }

    #[test]
    fn refit_follows_the_known_plugin_size() {
        let t = Instant::now();
        let mut s = Sizing::new();
        assert!(s.refit(CHROME, t).is_empty());
        s.opened(gs(866, 674), false, CHROME, t);
        s.reset_frame();
        assert_eq!(
            s.refit(CHROME, t)[1],
            Effect::Frame {
                inner: Px::new(866, 709),
                locked: true
            }
        );
    }
}
