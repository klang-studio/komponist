# design

- `tokens.json`: all design tokens, the single source of truth. It is the handoff's `tokens.json`
  (`../klang-studio-handoff/`) plus the komponist additions listed below; `komponist-tokens`
  generates its constants from it and rejects keys it does not know.
- `device-tokens.json`: the device frame, grid and widget metrics from the device handoff.

## komponist additions to the handoff tokens

Colours, one shadow and five type styles the design data uses but the handoff's `tokens.json` lacks. Each value is copied from
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
| `color.canvas.strip-selected` | `#1a1f26` | `Screens.dc.html` `mixer()` `strips` (`background:sel?'#1a1f26':C.f1`); `ks-view.js` `mixView` `strip` (`sel?'#1a1f26':C.f1`) | Mixer strip of the selected track (else `surface.1`); its name row is `accent.surface` (WP-43) |
| `color.state.error-surface` | `#2c1c1e` | `Screens.dc.html` `tp()` `errS` (`background:'#2c1c1e'`) and the S07 markup (device notice `background:#2c1c1e`) | Surface of error notices: "Kein Audio-Gerät" in the transport bar, the S07 device notice (WP-52) |
| `color.overlay.scrim` | `#08090a9e` | `Screens.dc.html` markup `ovDim` (`background:rgba(8,9,10,0.62)`) | Scrim behind a modal dialog: 0.62 × 255 = 158.1 → 158 = `0x9e` (WP-52) |
| `type.body` | Sans 13 / 20, 400 | `Screens.dc.html` markup of S10 (empty-project text `font-size:13px; line-height:1.55`) and S06 (dialog text, same style); 13 × 1.55 = 20.15 → 20 | Body text of hints and dialogs: the S10 empty-project hint (WP-47a), the S06 text; `type.track`'s size at regular weight |
| `shadow.dialog` | `0 12px 32px rgba(0,0,0,0.5)` | `Screens.dc.html` markup of S06, S07, S08 (`box-shadow:0 0 0 1px #111214, 0 12px 32px rgba(0,0,0,0.5)`; the 1 px part is `shadow.edge`) | Shadow of dialogs and plugin windows (WP-52) |
| `type.device-label` | Sans 10 / 12, 400, `text.2` | `geraete_page_generator.py` `lbl()` (`font-size:10px;line-height:12px`, colour `text.2` per `device-tokens.json` `widget.label`) | Name above or below a device widget (WP-83) |
| `type.graph-title` | Sans 9 / 12, 600, caps, +0.08 em, `text.2` | `geraete_page_generator.py` `display()` (`font-size:9px;font-weight:600;letter-spacing:0.08em;text-transform:uppercase`); `widget.graph.title`; line 12 as `lbl()` | Title of a graph or display (WP-83, WP-84) |
| `type.mono-xs` | Mono 8 / 10, 400, `text.2` | `device-tokens.json` `widget.graph.axis_labels` / `handle.number_font` ("mono 8 text.2"); line 10 is not given (own choice, one line of the 8 px text) | Axis labels and handle numbers of the graph (WP-84) |
| `type.mono-xxs` | Mono 9 / 12, 400, `text.2` | `geraete_page_generator.py` `W_num` (`font-size:9px`, `text.2`), `W_pads` (`font-size:9px`); line 12 as `lbl()` | Unit in a number field, note in a pad, label of a meter bar (WP-83) |

In Rust: `TrackPalette::{key_playing, note_muted, clip_handle}` (formulas in
`komponist_tokens::track::formula`), `color::STATE_RECORD_INK`, `color::LINE_0` (with
`color::LINE`, the line scale as an array), `color::CANVAS_*`, `color::STATE_ERROR_SURFACE`,
`color::OVERLAY_SCRIM`, `shadow::DIALOG` and `typography::{BODY, DEVICE_LABEL, GRAPH_TITLE, MONO_XS, MONO_XXS}`. The three formulas have no
precomputed entries in `track.colors`; `komponist-tokens` checks `TrackPalette` against the output
of the prototype's `hex()` for all 14 hues instead (`added_formulas_match_the_prototype` in
`crates/komponist-tokens/src/track.rs`).

The handoff's type styles are unchanged; `type.body` and the four device styles are the added ones. IBM Plex Mono SemiBold (600), which the design data uses for the
track toggle letters (`tgl`: Mono 10, `fontWeight: 600`), is bundled as a font face, not as a
type token (`assets/fonts/README.md`).

### Design colours that are still no token

Found while checking the additions above; they belong to later work packages and become tokens
by a contract change when those need them:

- `#2a2c2f` / `#202124`: arrangement bar lines (`Screens.dc.html` `arrangement()` `lines`,
  `ks-view.js` `arrView`). G2 decided the arrangement grid stays `line.2` (design system);
  recheck in WP-62.
- `#34363b`, `#4d5056`, `#5a5d63`: fader thumb greys (WP-21 maps them to the nearest tokens).

## Knob and fader interaction (owner decision 2026-11-15)

The design system's `ix.knob` / `ix.fader` and the device handoff (§4) list Alt-click = default and
double-click = text entry. komponist deviates for knobs and faders (not for number fields): a
**double-click resets to the default** (Alt-click still does), text entry is **Enter** on the focused
control or the right-click menu „Wert eingeben“, and the **mouse wheel over the control** changes
the value (1 % of the range per notch, Shift fine, Ctrl coarse). Details: `docs/ARCHITECTURE.md`
§13 item 10.

## Track toggles M S R I (owner decision 2026-11-14, contract change request C3)

The design system's toggle table lets "monitoring on" use the accent blue. The owner decided
(question Q7) that monitoring on stays green. The table now reads:

| Toggle | Off | On: fill | On: ink |
|---|---|---|---|
| M (mute) | `surface.3` | `state.mute` | `surface.0` |
| S (solo) | `surface.3` | `state.solo` | `surface.0` |
| R (record arm) | `surface.3` | `state.record` | `text.1` |
| I (monitoring) | `surface.3` | `state.play` (green, not `accent`) | `surface.0` |

Green separates I from mute (yellow), solo and record (red); accent blue would equal Snap and Loop.
Symbols and spacing of R and I: `docs/ARCHITECTURE.md` §13 item 8.
