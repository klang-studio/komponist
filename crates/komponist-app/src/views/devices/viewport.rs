use egui::{Rect, Ui};

/// Visible horizontal range of a plot, in the plot's source coordinates.
#[derive(Clone, Default)]
pub(super) struct Viewport {
    pub start: f64,
    pub end: f64,
}

impl Viewport {
    pub fn fit(length: f64) -> Self {
        Self {
            start: 0.0,
            end: length,
        }
    }

    pub fn width(&self) -> f64 {
        self.end - self.start
    }

    pub fn centre(&self) -> f64 {
        (self.start + self.end) * 0.5
    }

    pub fn set(&mut self, centre: f64, width: f64, length: f64, min_width: f64) {
        self.set_anchored(centre, 0.5, width, length, min_width);
    }

    fn set_anchored(
        &mut self,
        anchor: f64,
        fraction: f64,
        width: f64,
        length: f64,
        min_width: f64,
    ) {
        let width = width.clamp(min_width.min(length), length);
        self.start = (anchor - width * fraction).clamp(0.0, length - width);
        self.end = self.start + width;
    }

    /// Zoom around the pointer or pan horizontally with either wheel axis.
    pub fn wheel(&self, ui: &Ui, plot: Rect, length: f64, min_width: f64) -> Option<Self> {
        let (delta, zoom, pointer) =
            ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        let width = self.width();
        let mut next = self.clone();
        if zoom != 1.0 {
            let x = pointer.map_or(plot.center().x, |p| p.x);
            let fraction = f64::from(((x - plot.left()) / plot.width()).clamp(0.0, 1.0));
            let anchor = self.start + fraction * width;
            next.set_anchored(anchor, fraction, width / f64::from(zoom), length, min_width);
            return Some(next);
        }
        let pixels = delta.x + delta.y;
        if (pixels > 0.0 && self.start > 0.0) || (pixels < 0.0 && self.end < length) {
            let shift = f64::from(pixels / plot.width()) * width;
            next.set(self.centre() - shift, width, length, min_width);
            return (next.start != self.start).then_some(next);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_WIDTH: f64 = 0.005;

    #[test]
    fn viewport_stays_in_bounds_and_pans_to_both_ends() {
        let mut view = Viewport::fit(10.0);
        view.set(5.0, 2.0, 10.0, MIN_WIDTH);
        assert_eq!((view.start, view.end), (4.0, 6.0));
        view.set(0.0, view.width(), 10.0, MIN_WIDTH);
        assert_eq!((view.start, view.end), (0.0, 2.0));
        view.set(10.0, view.width(), 10.0, MIN_WIDTH);
        assert_eq!((view.start, view.end), (8.0, 10.0));
        view.set(10.0, 0.0, 10.0, MIN_WIDTH);
        assert!(view.width() > 0.0 && view.end == 10.0);
    }
}
