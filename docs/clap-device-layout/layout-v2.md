# Device layout version 2

Use `org.klang-studio.komponist.device-layout/2` with the callback table and
transfer rules in [the public header](clap-device-layout.h). The JSON root uses
`schema_version: 2`. The host tries version 2 first and falls back to version 1
if version 2 is absent, incompatible, or fails validation. A plugin can supply
both. The version 1 schema and extension retain their existing behavior.

Version 2 extends the [version 1 grid](README.md) to fourteen columns. Number
cells may occupy one or two columns. Set a page's optional `dividers` to the
zero-based indices of the columns followed by a visual separator. A page may also
declare localized `headings` with a zero-based `col` and `row` and a span in
`cols`. Each heading occupies an empty top half of its row and is exposed as a
read-only accessibility label. A graph occupies
whole cells, spans up to three rows, and cannot overlap another cell. Its
localized `name` is its title and accessible name. Its localized `description`
is announced through accessibility APIs. Both include the default locale.
Set `hide_title` to `true` to omit the painted title while retaining the accessible name.
A `mini_fader` is a parameter control one column wide that spans one to three
full rows.

It also adds [canvas cells](device-display.md) for imperative drawing. Each
canvas declares `key`, `name`, and `description` in its `canvas` object. Set
`show_title` to draw the localized `name` inside the canvas.

Graph slots bind directly to numeric CLAP parameter IDs. Omit a slot to draw it
with the host configuration's default value and without editing. The host
checks each bound ID against the live parameter inventory, applying the same
availability and range rules as for controls.

| Kind | Slot names |
| --- | --- |
| `envelope` | `attack`, `decay`, `sustain`, `release`, `curve` |
| `eq` | `b1_on`, `b1_freq`, `b1_gain`, `b1_q`, `b1_type`, and the same slots for every band. Set `bands` to 1–8. |
| `filter` | `cutoff`, `resonance`, `type` |
| `transfer` or `compressor` | `threshold`, `ratio`, `knee` |
| `waveform` | `start` and `end` in milliseconds |
| `xy` | `x` and `y`, both required |
| `lfo` | `x1`, `y1` and the same pair for every point. Set `points` to 2–16. |
| `gr_history` | No parameter slots |

The [schema](schema-v2.json) describes the structure and the
[example](example-v2.json) uses the layout from an independent C fixture.

## Device catalog labels

An optional root `metadata` object contains `names` and optional `descriptions`
maps, keyed by the document's locales. Each nonempty map includes the default
locale. The host obtains these labels at scan time. It falls back to the CLAP
descriptor name when the plugin supplies no valid localized names. Vendor,
plugin ID, role and features still come from the descriptor.

The same field is accepted as optional decoration in v1. Invalid decoration
does not affect a v1 panel, and older v1 hosts ignore the field.
