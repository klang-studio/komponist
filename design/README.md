# design

- `tokens.json`: all design tokens, the single source of truth. It is the handoff's `tokens.json`
  (`../klang-studio-handoff/`) plus the komponist additions listed below; `komponist-tokens`
  generates its constants from it and rejects keys it does not know.
- `reference/*.png`: the handoff's `.dc.html` files rendered by `scripts/render-reference.sh`.

## komponist additions to the handoff tokens

Colours the design data uses but the handoff's `tokens.json` lacks. Each value is copied from
the design data, not chosen; existing tokens are unchanged. Paths are relative to
`../klang-studio-handoff/reference/`; the name after the file is the script variable or function
in its `<script>` block.

| Token | Value | Source | Use |
|---|---|---|---|
| `track.$formula.key-playing` | `oklch(0.82 0.10 h)` | `ks-engine.js` `col.key` (`hex(.82,.1,h)`); `Screens.dc.html` `pianoRoll()` `keys` (`pl?hex(.82,.1,hue)`); `ks-view.js` `prView` `keys` (`col.key(hue)`) | Piano-roll keyboard key of a pitch that plays right now |
| `track.$formula.note-muted` | `oklch(0.55 0.08 h)` | `Designsystem.dc.html` `notes`, sample "stumm" (`inset 0 0 0 1px hex(.55,.08,HUE)`) | Contour of a muted note |
| `track.$formula.clip-handle` | `oklch(0.40 0.07 h)` | `Designsystem.dc.html` `clipO()`, extras of `st==='hover'` (`hex(.40,.07,h)`, 5 px wide) | Trim handles at both ends of a hovered clip |
| `color.state.record-ink` | `#f2a6a8` | `Designsystem.dc.html` `clipO()` `inkC` (`st==='rec'`); `Screens.dc.html` `clipO()` `ik`; `ks-view.js` `clipView` `ik` | Notes and steps inside a recording clip |
| `color.line.0` | `#232427` | `Screens.dc.html` `pianoRoll()` `lines` (`i%16===0?C.l2:i%4===0?C.l1:'#232427'`); `ks-view.js` `prView` `lines` | Grid lines between the beats of the piano roll |
| `color.canvas.lane-selected` | `#1a1c1f` | `Screens.dc.html` `arrangement()` `lane` (`t.sel?'#1a1c1f':C.f1`); `ks-view.js` `arrView` `lane` | Arrangement lane of a selected track (else `surface.1`) |
| `color.canvas.black-key-row` | `#1a1b1d` | `Screens.dc.html` `pianoRoll()` `rows` (`b?'#1a1b1d':C.f2`); `ks-view.js` `prView` `rows` | Piano-roll row of a black key (white keys `surface.2`) |
| `color.canvas.outside-clip` | `#11121499` | `Screens.dc.html` `pianoRoll()` `outside` (`rgba(17,18,20,0.6)`); `ks-view.js` `prView` `outside` | Overlay on the piano-roll grid after the clip end: `surface.0` at 60 % (0.6 × 255 = 153 = `0x99`) |
| `color.canvas.box-select` | `#4ea7e61a` | `ks-view.js` `prView` `box` (`background:'rgba(78,167,230,0.10)'`, with `boxShadow:'inset 0 0 0 1px '+C.acc`) | Fill of the box selection in the piano roll (WP-41) and the arrangement (WP-40): `accent` at 10 % (0.10 × 255 = 25.5 → 26 = `0x1a`); its 1 px edge is `accent` |
| `color.canvas.clip-edge-selected` | `#e8e9eb29` | `ks-view.js` `clipView` `edge` (`background: sel ? 'rgba(232,233,235,0.16)' : 'transparent'`, `width:6`, `right:0`) | Right edge of a selected arrangement clip, where the length is dragged: `text.1` at 16 % (0.16 × 255 = 40.8 → 41 = `0x29`) |
| `color.canvas.step-group-alt` | `#222326` | `Screens.dc.html` `stepSeq()` `cells` (`grp?'#222326':C.f2`); `ks-view.js` `stepView` `off` | Step-sequencer cell that is off, in every other group of four steps (steps 5–8, 13–16, …; the others `surface.2`) (WP-42) |
| `color.state.error-surface` | `#2c1c1e` | `Screens.dc.html` `tp()` `errS` (`background:'#2c1c1e'`) and the S07 markup (device notice `background:#2c1c1e`) | Surface of error notices: "Kein Audio-Gerät" in the transport bar, the S07 device notice (WP-52) |
| `color.overlay.scrim` | `#08090a9e` | `Screens.dc.html` markup `ovDim` (`background:rgba(8,9,10,0.62)`) | Scrim behind a modal dialog: 0.62 × 255 = 158.1 → 158 = `0x9e` (WP-52) |
| `shadow.dialog` | `0 12px 32px rgba(0,0,0,0.5)` | `Screens.dc.html` markup of S06, S07, S08 (`box-shadow:0 0 0 1px #111214, 0 12px 32px rgba(0,0,0,0.5)`; the 1 px part is `shadow.edge`) | Shadow of dialogs and plugin windows (WP-52) |

In Rust: `TrackPalette::{key_playing, note_muted, clip_handle}` (formulas in
`komponist_tokens::track::formula`), `color::STATE_RECORD_INK`, `color::LINE_0` (with
`color::LINE`, the line scale as an array), `color::CANVAS_*`, `color::STATE_ERROR_SURFACE`,
`color::OVERLAY_SCRIM` and `shadow::DIALOG`. The three formulas have no
precomputed entries in `track.colors`; `komponist-tokens` checks `TrackPalette` against the output
of the prototype's `hex()` for all 14 hues instead (`added_formulas_match_the_prototype` in
`crates/komponist-tokens/src/track.rs`).

The type styles are unchanged. IBM Plex Mono SemiBold (600), which the design data uses for the
track toggle letters (`tgl`: Mono 10, `fontWeight: 600`), is bundled as a font face, not as a
type token (`assets/fonts/README.md`).

### Design colours that are still no token

Found while checking the additions above; they belong to later work packages and become tokens
by a contract change when those need them:

- `#2a2c2f` / `#202124`: arrangement bar lines (`Screens.dc.html` `arrangement()` `lines`,
  `ks-view.js` `arrView`). G2 decided the arrangement grid stays `line.2` (design system);
  recheck in WP-62.
- `#1a1f26`: mixer strip of the selected track (`Screens.dc.html` `mixer()`, `ks-view.js`
  `mixView`).
- `#34363b`, `#4d5056`, `#5a5d63`: fader thumb greys (WP-21 maps them to the nearest tokens).
