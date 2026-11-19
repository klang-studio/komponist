# Icons

A subset of [Lucide](https://lucide.dev), unmodified, from the npm package
[`lucide-static@1.72.0`](https://www.npmjs.com/package/lucide-static/v/1.72.0)
(`package/icons/*.svg`; tarball integrity
`sha512-QhdZ60O7Zcs/e8gw40uMfEn055xZTHBZoNi0xb+/1R2dTd0v4FXS46VL6rKSwC+V92tLBiqsAbCcijy7Wy4j4w==`).
Licence: ISC, with MIT for the icons derived from Feather; see `LICENSE` (copied from the package).

The handoff proposes Lucide but says the set is not final (HANDOFF §5, Designsystem §07). The
`komponist_tokens::Icon` enum hides the file names, so the set can be replaced later.

## What the crate does with them

`crates/komponist-tokens/build.rs` reads every `*.svg` here and writes a prepared copy to
`OUT_DIR`; `Icon::svg_bytes()` returns that copy. The preparation only touches attributes of the
root `<svg>` element:

| Attribute | Lucide | Prepared | Why |
|---|---|---|---|
| `width`, `height` | `24` | `16` | `size.icon` from `design/tokens.json`, so the intrinsic size is the design size |
| `stroke-width` | `2` | `2.25` | 1.5 px stroke on the 16 px grid (HANDOFF §5) = 1.5 × 24 / 16 user units |
| `stroke` | `currentColor` | `#ffffff` | resvg renders `currentColor` black; white lets egui's image tint set the colour |
| `fill` (only `star-filled`) | `none` | `#ffffff` | filled favourite star ★ next to the outline ☆ |

`star-filled` is the only derived icon; it has no file of its own.

## Icon list and where the screens use them

`AudioWaveform` (`audio-waveform`) is an original six-stroke geometric waveform under ISC,
used for audio files and inputs (audio proposal §10), prepared like the Lucide subset.
The automation proposal uses an adaptation of Lucide’s spline with round control points
(automation proposal §9, internal notes).

The screens draw most icons as text glyphs (`⋯ ▾ ▸ ‹ › × ✓ ★ ⚠`) or as plain text buttons. The
table maps each one to a Lucide icon. Play, Stop and Record stay geometric shapes (no icon).

| `Icon` | Lucide | Glyph / control in the design | Where |
|---|---|---|---|
| `Search` | `search` | browser search field "Suchen · Strg F" | S01–S08, S10 browser; S05 search "hall"; S09 "Filtern" |
| `Plus` | `plus` | `+` | dock tab "+" (S01–S08, S10); zoom + (S01–S03, S05); insert slot "+" (S04); "+ Effekt" (S01, S03–S05); "+ Gerät" (S05); "+ Ordner" (S07); icon button and panel-header action (Designsystem §04, §06) |
| `Minus` | `minus` | `−` | zoom − in arrangement and piano roll (S01–S03, S05) |
| `Ellipsis` | `ellipsis` | `⋯` | transport menu, top right (S01–S08, S10); panel-header action (Designsystem §06) |
| `ChevronDown` | `chevron-down` | `▾` / `▼` | expanded panel header (S01–S08, S10; Designsystem §06); dropdowns (S01, S03–S05, S07, S08; Designsystem §04); "Maximieren ▾" (S01–S03, S05–S08, S10) |
| `ChevronRight` | `chevron-right` | `▸` / `▶`, `›` | collapsed panel header (Designsystem §06); browser folders "EQ und Filter", "Takes" … (S01–S08, S10); submenu "Farbe ▸" (Designsystem §06); next preset `›` (S05 plugin window) |
| `ChevronLeft` | `chevron-left` | `‹` | previous preset (S05 plugin window) |
| `X` | `x` | `×` | close plugin window (S05), "Einstellungen" (S07), "WAV exportieren" (S08); remove device (prototype) |
| `Check` | `check` | `✓` | current value in an open dropdown (Designsystem §04); "Loop ✓" in the context menu (Designsystem §06) |
| `Star` | `star` | `☆` | browser item that is not a favourite (prototype `ks-view.js`) |
| `StarFilled` | `star` + fill | `★` | browser favourites (S01–S08, S10; S05 search results) |
| `TriangleExclamationPoint` | `triangle-exclamation-point` (formerly `alert-triangle`) | `⚠` | track header "⚠ Plugin fehlt: Vital" (Designsystem §05) |
| `Settings` | `settings` | "Einstellungen Strg ," | settings dialog (S07); system lines on the start screen (S09) |
| `Folder` | `folder` | "Plugin-Ordner", "Ordner" | plugin folders (S07); export folder (S08) |
| `FolderOpen` | `folder-open` | "Projekt öffnen …", "Im Ordner zeigen" | start screen (S09), empty project (S10), export done (S08) |
| `Maximize2` | `maximize-2` | "Maximieren" | dock (S01–S03, S05–S08, S10) |
| `Minimize2` | `minimize-2` | "Maximiert" | maximised dock, restore (S04) |
| `Power` | `power` | bypass switch (square in the card head) | device cards (S05), inspector "Geräte" (S01, S03–S05), plugin window "Bypass" (S05) |
| `SlidersHorizontal` | `sliders-horizontal` | "Parameter" | device card and plugin window (S05) |
| `Piano` | `piano` | editor tab "Piano Roll" | S01, S02, S04–S05; Designsystem §06 tabs |
| `Grid3x3` | `grid-3x3` | editor tab "Step Sequencer" | S03; Designsystem §06 tabs |
| `MousePointer` | `mouse-pointer` | tool "Zeiger" | piano roll toolbar (S01, S02; Designsystem §06) |
| `Pencil` | `pencil` | tool "Stift" | piano roll toolbar (S01, S02, S10; Designsystem §06) |
| `Eraser` | `eraser` | tool "Radierer" | piano roll toolbar (S01, S02; Designsystem §06) |
| `Magnet` | `magnet` | "Snap" | arrangement panel header and piano roll toolbar (S01–S03, S05; owner decision 2026-11-15: icon button instead of the text) |
| `Metronome` | `metronome` | "Metronom" | transport bar (S01–S08, S10) |
| `Repeat` | `repeat` | "Loop" | transport bar (S01–S08, S10); looped clip (Designsystem §05) |
| `Timer` | `timer` | "Vorzähler 1" | transport bar (S01–S08, S10); icon with the bar count beside it (owner decision 2026-11-15, no Lucide icon says count-in; `timer` = time before the start) |
| `Volume2` | `volume-2` | track toggle "Monitoring" (I) | arrangement track header (owner decision 2026-11-14: icon instead of the letter, ARCHITECTURE §13 item 8) |
| `Spline` | `spline` (adapted) | automation toggle and clip marker | arrangement, piano roll and step sequencer |

## Updating

```bash
curl -sSLfO https://registry.npmjs.org/lucide-static/-/lucide-static-<version>.tgz
tar -xzf lucide-static-<version>.tgz
cp package/icons/<name>.svg assets/icons/       # one per Icon variant
cp package/LICENSE assets/icons/LICENSE
```

Then add or rename the variant in `crates/komponist-tokens/src/icon.rs`; the tests check that every
file here has a variant and every variant has a file.
