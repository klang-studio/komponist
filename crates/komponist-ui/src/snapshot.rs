//! Snapshot tests with `egui_kittest` (feature `snapshot`, on for this crate's tests).
//!
//! Every snapshot is rendered with the wgpu renderer at **1× and 2×** (`pixels_per_point`), with
//! the komponist setup ([`crate::install`]) and without the mouse cursor, into
//! `crates/komponist-ui/tests/snapshots/<name>-1x.png` and `<name>-2x.png`:
//!
//! ```ignore
//! use komponist_ui::{State, snapshot};
//!
//! #[test]
//! fn button_states() {
//!     let mut shots = snapshot::Snapshots::new();
//!     for (_, state) in State::CONTROL_STATES {
//!         // tests/snapshots/button-<state>-1x.png and -2x.png
//!         shots.add_state("button", state, None, |ui| {
//!             button(ui, "Quantisieren");
//!         });
//!     }
//!     shots.finish();
//! }
//! ```
//!
//! [`Snapshots::add_state`] renders a widget in a forced [`State`] ([`crate::state::force`]), so no
//! input has to be simulated. A new or changed snapshot fails the test and leaves
//! `<name>.new.png` (and `<name>.diff.png`) next to it; `UPDATE_SNAPSHOTS=1 cargo test` accepts
//! them. Look at the PNGs before committing them (rule 7).

use std::path::PathBuf;

use egui::{ColorImage, Frame, Margin, Ui, Vec2};
use egui_kittest::{Harness, HarnessBuilder, SnapshotOptions, SnapshotResults};
use komponist_tokens::space;

use crate::state::{self, State};

pub use egui_kittest;

/// The scales every snapshot is rendered at.
pub const SCALES: [f32; 2] = [1.0, 2.0];

/// `crates/komponist-ui/tests/snapshots`.
pub fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Snapshot options: the komponist snapshot directory, kittest's default thresholds.
pub fn options() -> SnapshotOptions {
    SnapshotOptions::new().output_path(snapshot_dir())
}

/// The file stem of a snapshot at a scale: `button-hovered` at 2× → `button-hovered-2x`.
pub fn file_stem(name: &str, pixels_per_point: f32) -> String {
    format!("{name}-{pixels_per_point}x")
}

/// A harness builder with the komponist setup at `pixels_per_point`: komponist installed, wgpu
/// renderer, no cursor in the image, snapshots in [`snapshot_dir`].
pub fn builder<S>(pixels_per_point: f32) -> HarnessBuilder<S> {
    HarnessBuilder::default()
        .with_pixels_per_point(pixels_per_point)
        .with_context_setup(crate::install)
        .with_render_cursor(false)
        .with_options(options())
}

/// A harness running `app` at `pixels_per_point`; `size` in points, or `None` to fit the
/// contents. The contents get `space.2` of room on every side (on `surface.1`, the panel
/// colour), so focus rings and selection rings outside a widget's rect are in the picture.
pub fn harness<'a>(
    size: Option<Vec2>,
    pixels_per_point: f32,
    mut app: impl FnMut(&mut Ui) + 'a,
) -> Harness<'a> {
    let b = builder(pixels_per_point);
    let b = match size {
        Some(size) => b.with_size(size),
        None => b.with_fit_contents(),
    };
    let mut h = b.build_ui(move |ui| {
        padded(ui, &mut app);
    });
    h.run();
    h
}

/// Runs `add` with `space.2` of room on every side, as [`harness`] does; for harnesses built
/// with [`builder`] directly.
pub fn padded<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .inner_margin(Margin::same(space::SPACE_2 as i8))
        .show(ui, add)
        .inner
}

/// Collects the results of several snapshots, so that one test reports (and with
/// `UPDATE_SNAPSHOTS=1` writes) all of them before it fails.
pub struct Snapshots {
    results: SnapshotResults,
}

impl Default for Snapshots {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshots {
    /// No snapshots yet.
    pub fn new() -> Self {
        Self {
            results: SnapshotResults::new(),
        }
    }

    /// Renders `app` at 1× and 2× and compares with `tests/snapshots/<name>-1x.png` and
    /// `-2x.png`; `size` in points, or `None` to fit the contents.
    pub fn add(&mut self, name: &str, size: Option<Vec2>, mut app: impl FnMut(&mut Ui)) {
        for ppp in SCALES {
            let mut h = harness(size, ppp, &mut app);
            h.snapshot(file_stem(name, ppp));
            self.results.extend_harness(&mut h);
        }
    }

    /// Renders a widget in a forced `state` ("render widget in state X") at 1× and 2× into
    /// `tests/snapshots/<name>-<state slug>-1x.png` / `-2x.png` ([`State::slug`]).
    pub fn add_state(
        &mut self,
        name: &str,
        state: State,
        size: Option<Vec2>,
        mut app: impl FnMut(&mut Ui),
    ) {
        let name = format!("{name}-{}", state.slug());
        self.add(&name, size, |ui| state::force(ui, state, &mut app));
    }

    /// Panics if any snapshot is missing or differs.
    #[track_caller]
    pub fn finish(self) {
        self.results.unwrap();
    }
}

/// One snapshot at 1× and 2× ([`Snapshots::add`]); panics if it is missing or differs.
#[track_caller]
pub fn snapshot(name: &str, size: Option<Vec2>, app: impl FnMut(&mut Ui)) {
    let mut s = Snapshots::new();
    s.add(name, size, app);
    s.finish();
}

/// One widget in a forced state at 1× and 2× ([`Snapshots::add_state`]).
#[track_caller]
pub fn snapshot_state(name: &str, state: State, size: Option<Vec2>, app: impl FnMut(&mut Ui)) {
    let mut s = Snapshots::new();
    s.add_state(name, state, size, app);
    s.finish();
}

/// Renders `app` once at `pixels_per_point` and returns the image (physical pixels), for
/// assertions on pixels.
pub fn render(size: Vec2, pixels_per_point: f32, app: impl FnMut(&mut Ui)) -> ColorImage {
    let mut h = harness(Some(size), pixels_per_point, app);
    let image = h.render().expect("wgpu render");
    let (w, hgt) = (image.width() as usize, image.height() as usize);
    ColorImage::from_rgba_unmultiplied([w, hgt], image.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(file_stem("button-hovered", 2.0), "button-hovered-2x");
        assert_eq!(file_stem("x", 1.0), "x-1x");
        assert!(snapshot_dir().ends_with("komponist-ui/tests/snapshots"));
    }
}
