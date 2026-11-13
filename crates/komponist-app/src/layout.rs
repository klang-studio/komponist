//! Layout 2a with dock (`HANDOFF.md` §4, `Layout.dc.html` `dockFrames` / `dockRules`).
//!
//! ```text
//! ┌──────────────────────── transport 44 ─────────────────────────────┐
//! ├ browser 260 ┬──────── arrangement ─────────────┬ inspector 280 ────┤
//! │ (200–420)   ├──── dock 320 (200 … 70 %), tabs 28 ┤ (240–360)        │
//! ├─────────────┴──────────── status bar 22 ─────────┴──────────────────┤
//! ```
//!
//! [`compute`] turns the window rect and [`PanelLayout`] into panel rects; it is pure, so the
//! sizes are tested without pixels. Panels sit 1 px apart on `surface.0`, which shows as the
//! lines between them. A collapsed browser or inspector becomes a 24 px rail; a collapsed dock
//! keeps its 28 px tab strip; a maximised dock fills the centre and hides the arrangement.
//!
//! The 1 px gaps are the resize edges ([`Edge`]): dragging changes the size within the token
//! range, a double-click resets it to the token default.

use egui::{Rect, pos2};
use komponist_tokens::size;

use crate::metrics::layout::{GAP, GRAB, RAIL_WIDTH};
use crate::ui_state::{Panel, PanelLayout};

/// A side panel: open with its rect, or collapsed to a rail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    /// Open: header and content.
    Open(Rect),
    /// Collapsed: the 24 px rail.
    Rail(Rect),
}

impl Side {
    /// The rect, open or rail.
    pub fn rect(self) -> Rect {
        match self {
            Side::Open(r) | Side::Rail(r) => r,
        }
    }

    /// Whether the panel is open.
    pub fn is_open(self) -> bool {
        matches!(self, Side::Open(_))
    }
}

/// A resize edge between two panels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    /// Which panel it resizes.
    pub panel: Panel,
    /// The grab zone around the 1 px gap.
    pub grab: Rect,
}

/// Where everything goes in one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Transport bar, `size.transport` high.
    pub transport: Rect,
    /// Browser (left).
    pub browser: Side,
    /// Inspector (right).
    pub inspector: Side,
    /// The arrangement, `None` while the dock is maximised.
    pub arrangement: Option<Rect>,
    /// The dock, tab strip included.
    pub dock: Rect,
    /// Status bar, `size.statusbar` high.
    pub status: Rect,
    /// Resize edges of the open, resizable panels.
    pub edges: Vec<Edge>,
}

/// The largest dock height in a window `window_height` high (`size.dock.max` = 70 % of it).
pub fn dock_max(window_height: f32) -> f32 {
    window_height * size::dock::MAX_VIEWPORT_HEIGHT_FRACTION
}

/// `height` clamped to the dock range for a window `window_height` high (never below the
/// minimum, even in a tiny window).
pub fn clamp_dock(height: f32, window_height: f32) -> f32 {
    height.min(dock_max(window_height)).max(size::dock::MIN)
}

/// `width` clamped to the range of `panel` (browser or inspector).
pub fn clamp_side(panel: Panel, width: f32) -> f32 {
    match panel {
        Panel::Browser => width.clamp(size::browser::MIN, size::browser::MAX),
        Panel::Inspector => width.clamp(size::inspector::MIN, size::inspector::MAX),
        Panel::Dock => width,
    }
}

/// The token default size of `panel`.
pub fn default_size(panel: Panel) -> f32 {
    match panel {
        Panel::Browser => size::browser::DEFAULT,
        Panel::Inspector => size::inspector::DEFAULT,
        Panel::Dock => size::dock::DEFAULT,
    }
}

/// Lays out `screen` (the window in points) for `layout`.
pub fn compute(screen: Rect, layout: &PanelLayout) -> Layout {
    let (x0, x1) = (screen.left(), screen.right());
    let transport = Rect::from_min_max(screen.min, pos2(x1, screen.top() + size::TRANSPORT));
    let status = Rect::from_min_max(pos2(x0, screen.bottom() - size::STATUSBAR), screen.max);
    let middle = Rect::from_min_max(
        pos2(x0, transport.bottom() + GAP),
        pos2(x1, (status.top() - GAP).max(transport.bottom() + GAP)),
    );

    let browser_w = if layout.browser_open {
        clamp_side(Panel::Browser, layout.browser_width)
    } else {
        RAIL_WIDTH
    };
    let inspector_w = if layout.inspector_open {
        clamp_side(Panel::Inspector, layout.inspector_width)
    } else {
        RAIL_WIDTH
    };
    let browser_rect =
        Rect::from_min_max(middle.min, pos2((x0 + browser_w).min(x1), middle.bottom()));
    let inspector_rect = Rect::from_min_max(
        pos2((x1 - inspector_w).max(browser_rect.right()), middle.top()),
        middle.max,
    );
    let center = Rect::from_min_max(
        pos2(browser_rect.right() + GAP, middle.top()),
        pos2(
            (inspector_rect.left() - GAP).max(browser_rect.right() + GAP),
            middle.bottom(),
        ),
    );

    let maximized = layout.dock_open && layout.dock_maximized;
    let dock_h = if !layout.dock_open {
        size::dock::COLLAPSED
    } else if maximized {
        center.height()
    } else {
        clamp_dock(layout.dock_height, screen.height())
    };
    // The arrangement keeps what is left; in a tiny window the dock gives way.
    let dock_h = dock_h.min(center.height());
    let dock = Rect::from_min_max(pos2(center.left(), center.bottom() - dock_h), center.max);
    let arrangement = (!maximized).then(|| {
        Rect::from_min_max(
            center.min,
            pos2(center.right(), (dock.top() - GAP).max(center.top())),
        )
    });

    let mut edges = Vec::new();
    if layout.browser_open {
        let x = browser_rect.right() + GAP / 2.0;
        edges.push(Edge {
            panel: Panel::Browser,
            grab: Rect::from_x_y_ranges(x - GRAB..=x + GRAB, middle.y_range()),
        });
    }
    if layout.inspector_open {
        let x = inspector_rect.left() - GAP / 2.0;
        edges.push(Edge {
            panel: Panel::Inspector,
            grab: Rect::from_x_y_ranges(x - GRAB..=x + GRAB, middle.y_range()),
        });
    }
    if layout.dock_open && !maximized {
        let y = dock.top() - GAP / 2.0;
        edges.push(Edge {
            panel: Panel::Dock,
            grab: Rect::from_x_y_ranges(center.x_range(), y - GRAB..=y + GRAB),
        });
    }

    Layout {
        transport,
        browser: if layout.browser_open {
            Side::Open(browser_rect)
        } else {
            Side::Rail(browser_rect)
        },
        inspector: if layout.inspector_open {
            Side::Open(inspector_rect)
        } else {
            Side::Rail(inspector_rect)
        },
        arrangement,
        dock,
        status,
        edges,
    }
}

/// The size `panel` gets when its edge is dragged to the pointer position `pointer` (x for the
/// side panels, y for the dock), clamped to its range.
pub fn size_at(panel: Panel, pointer: egui::Pos2, screen: Rect) -> f32 {
    match panel {
        Panel::Browser => clamp_side(panel, pointer.x - screen.left()),
        Panel::Inspector => clamp_side(panel, screen.right() - pointer.x),
        Panel::Dock => {
            let bottom = screen.bottom() - size::STATUSBAR - GAP;
            clamp_dock(bottom - pointer.y, screen.height())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
    }

    #[test]
    fn default_layout_matches_layout_2a() {
        let l = compute(screen(), &PanelLayout::default());
        assert_eq!(
            l.transport,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 44.0))
        );
        assert_eq!(
            l.status,
            Rect::from_min_max(pos2(0.0, 1058.0), pos2(1920.0, 1080.0))
        );
        let b = l.browser.rect();
        assert!(l.browser.is_open());
        assert_eq!(
            (b.left(), b.width(), b.top(), b.bottom()),
            (0.0, 260.0, 45.0, 1057.0)
        );
        let i = l.inspector.rect();
        assert_eq!((i.left(), i.width()), (1640.0, 280.0));
        let a = l.arrangement.unwrap();
        assert_eq!((a.left(), a.right()), (261.0, 1639.0));
        assert_eq!(l.dock.height(), 320.0);
        assert_eq!(l.dock.bottom(), 1057.0);
        assert_eq!(a.bottom() + 1.0, l.dock.top());
        // 1012 px between transport and status bar, as in the prototype's `geo`.
        assert_eq!(b.height(), 1012.0);
        assert_eq!(l.edges.len(), 3);
    }

    #[test]
    fn collapsed_panels_become_rails_and_a_tab_strip() {
        let layout = PanelLayout {
            browser_open: false,
            inspector_open: false,
            dock_open: false,
            ..PanelLayout::default()
        };
        let l = compute(screen(), &layout);
        assert_eq!(
            l.browser,
            Side::Rail(Rect::from_min_max(pos2(0.0, 45.0), pos2(24.0, 1057.0)))
        );
        assert_eq!(l.inspector.rect().width(), 24.0);
        assert_eq!(l.dock.height(), size::dock::COLLAPSED);
        assert!(l.edges.is_empty(), "collapsed panels have no resize edge");
        let a = l.arrangement.unwrap();
        assert_eq!(a.width(), 1920.0 - 2.0 * 24.0 - 2.0);
    }

    #[test]
    fn a_maximised_dock_hides_the_arrangement() {
        let layout = PanelLayout {
            dock_maximized: true,
            ..PanelLayout::default()
        };
        let l = compute(screen(), &layout);
        assert_eq!(l.arrangement, None);
        assert_eq!(l.dock.height(), 1012.0);
        assert!(l.edges.iter().all(|e| e.panel != Panel::Dock));
        // Collapsing wins over maximising.
        let l = compute(
            screen(),
            &PanelLayout {
                dock_open: false,
                ..layout
            },
        );
        assert!(l.arrangement.is_some());
        assert_eq!(l.dock.height(), size::dock::COLLAPSED);
    }

    #[test]
    fn sizes_are_clamped_to_the_token_ranges() {
        let layout = PanelLayout {
            browser_width: 999.0,
            inspector_width: 1.0,
            dock_height: 5000.0,
            ..PanelLayout::default()
        };
        let l = compute(screen(), &layout);
        assert_eq!(l.browser.rect().width(), size::browser::MAX);
        assert_eq!(l.inspector.rect().width(), size::inspector::MIN);
        assert_eq!(l.dock.height(), 1080.0 * 0.7);
        assert_eq!(clamp_dock(10.0, 1080.0), size::dock::MIN);
    }

    #[test]
    fn dragging_an_edge_follows_the_pointer_within_range() {
        let s = screen();
        assert_eq!(size_at(Panel::Browser, pos2(300.0, 500.0), s), 300.0);
        assert_eq!(
            size_at(Panel::Browser, pos2(50.0, 500.0), s),
            size::browser::MIN
        );
        assert_eq!(size_at(Panel::Inspector, pos2(1600.0, 500.0), s), 320.0);
        assert_eq!(
            size_at(Panel::Inspector, pos2(1000.0, 500.0), s),
            size::inspector::MAX
        );
        // Dock: its top edge at the pointer; bottom is above the status bar.
        assert_eq!(size_at(Panel::Dock, pos2(800.0, 657.0), s), 400.0);
        assert_eq!(size_at(Panel::Dock, pos2(800.0, 10.0), s), 1080.0 * 0.7);
        assert_eq!(default_size(Panel::Dock), size::dock::DEFAULT);
    }

    #[test]
    fn a_small_window_never_gives_negative_sizes() {
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
        let l = compute(tiny, &PanelLayout::default());
        let a = l.arrangement.unwrap();
        assert!(a.width() >= 0.0 && a.height() >= 0.0);
        assert!(l.dock.height() >= 0.0);
        assert!(l.dock.top() >= l.transport.bottom());
    }
}
