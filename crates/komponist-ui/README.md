# komponist-ui

Implements the shared egui design system for the app, plugin frames and the component gallery.

## Structure

- [src/theme.rs](src/theme.rs) — Applies shared styles, colours and type sizes.
- [src/widgets/](src/widgets/) — Provides reusable controls and device widgets.
- [src/timeline/](src/timeline/) — Paints timeline elements and supplies accessibility helpers.
- [src/px.rs](src/px.rs) — Snaps geometry to physical pixels.
- [src/plugin_frame.rs](src/plugin_frame.rs) — Draws the shared plugin-frame header.
- [src/gallery_shell.rs](src/gallery_shell.rs) — Hosts the component gallery.
- [src/snapshot.rs](src/snapshot.rs) — Provides the optional visual test harness.

## How it works

The shared setup installs fonts, icons, styles and focus tracking in an egui context. Widgets turn
pointer and keyboard input into responses and expose accessible names and roles. Timeline helpers
draw elements from geometry supplied by the app. Run the component gallery with
`cargo run -p komponist-ui --bin komponist-gallery` .
