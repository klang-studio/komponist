# komponist-tokens

Turns the design system's JSON sources into Rust constants and provides the bundled type and icon
assets.

## Structure

- [build.rs](build.rs) — Runs token generation.
- [build/](build/) — Parses token sources and generates Rust modules.
- [src/track.rs](src/track.rs) — Derives track palettes from hues.
- [src/oklch.rs](src/oklch.rs) — Converts Oklch colours to sRGB.
- [src/font.rs](src/font.rs) — Embeds IBM Plex Sans and Mono.
- [src/icon.rs](src/icon.rs) — Exposes the bundled icons by name.
- [src/egui_support.rs](src/egui_support.rs) — Adds conversions behind the optional egui feature.

## How it works

Cargo reads the [base tokens](../../design/tokens.json) and
[device tokens](../../design/device-tokens.json) at build time and emits typed values for
consumers. Lengths are design points and colours use straight sRGB alpha. Common track colours are
derived for each hue, and consumers use the same assets and constants across views.
