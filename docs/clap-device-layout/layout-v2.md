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

## Sample waveforms

A waveform graph can declare an optional `asset_slot` with the numeric ID of an
`AudioSample` slot in the [device asset inventory](device-assets.md). The host
shows the waveform of the current project-linked file, using its source duration
for the horizontal axis. Use a zoom gesture over the waveform to zoom at the
pointer, scroll to pan, and double-click an empty part of the plot to fit the
source. These gestures change the view. Start and end keep their normal CLAP
parameter gestures. The optional `trim_unit` defaults to `milliseconds`. With
`source_fraction`, list `source-fraction-trim` in `required_capabilities`.
The asset slot is required and any start or end binding must be a continuous
parameter with plain values from 0 to 1. The host uses the file duration to show
those values on the time axis. The trim handles stay locked until the file
duration is available.
If the slot or link is unavailable or the host cannot read its audio, the graph
has an empty waveform. Select a file using the device menu or drop a single
file on the waveform to link it to that slot.
A waveform can bind the `playhead` role to a named stream of normalized playback
positions in the linked file. The playhead appears when the slot reports it is
ready.

Allow at least six columns and set `waveform_toolbar` to show the linked file's
name and status, a file picker, and buttons for zooming, panning and fitting the
source. For graphs that offer
audition, the toolbar also includes a click-preview toggle. The toolbar occupies
the top of the graph cell.

Set `waveform_slice_controls` to the three numeric IDs of the sample-wide
direction and fade controls to hide while editing slices. This uses a ten-column
waveform in the first two rows, with `start`, `end`, `audition_note`,
`waveform_toolbar` and `waveform_markers`. In the third row it reserves the
bottom half and the top half after the first two columns. Ordinary controls
listed in `waveform_slice_controls` may occupy those cells outside slice mode.
The host draws the slice's trigger, direction and fades, the transient and
marker tools, and a note assignment menu in the reserved cells.
Double-click an empty part of the plot in slice mode to add a marker. Use the
toolbar's Fit button to reset zoom while in that mode.

Set `waveform_markers` to `true` to display the [marker snapshot](waveform-markers.md)
from the graph's `asset_slot`. The host labels the visible marker positions in its
current language. When the device allows edits, drag a marker to move it, press
Delete on the focused marker to remove it, or Shift-double-click an empty part
of the waveform to add one. Double-click without Shift fits the waveform.

An audio asset waveform can bind `audition_note` to the numeric ID of a stepped
0–127 parameter. The host uses that value for a note held while the waveform is
pressed. When the marker snapshot is editable, the host instead calls the optional
[waveform preview extension](clap-waveform-preview.h) with the source position in
seconds and releases that preview on pointer-up or loss of focus. Preview is
ephemeral and does not enter project state or undo.

## Pad-selected pages

List `pad-bank` in `required_capabilities` and declare a top-level `pad_bank`
object to replace the page switch with a pad grid. Declare one to sixteen
`pads`, in the same order as `pages`. Each entry contains its distinct MIDI
`note` (0–127) and distinct `asset_slot` (an audio sample slot ID).

Place one full-cell `pad_grid` at the same location and with the same span on
each page. It lays out the pads in row order, filling the declared number of
rows. Each page's localized label identifies its pad until a sample is linked.
Clicking a pad selects its page and auditions its note. Users can drop a file
on a pad or choose, locate or remove its file from the pad's menu. A waveform
that references a pad's slot shows the pad label and asset name.

The bank can name an `activity_tap` of kind
[pad activity](signal-taps.md). This supplies the playing pads and their
waveform playheads. An optional numeric `follow_param` identifies a
`toggle_button` present on each page. When that toggle is on, the host uses
the activity tap's most recent pad hit to follow incoming notes.

## Live data layers

An optional per-graph `signal_taps` object binds named streams to host-rendered
layers. See the [signal tap extension](signal-taps.md) for the supported roles
and the visibility lease.

A `meter_gr` cell shows the host's vertical In, GR and Out meter. It occupies
one full column and all three rows. Set `reduction_tap` to the key of a signal
tap that reports gain reduction in positive dB. For example

```json
{"widget":"meter_gr","col":3,"row":0,"cols":1,"rows":3,"reduction_tap":"reduction"}
```

The host supplies the accessible name and reports the maximum reduction over
the last 1.5 seconds. Click the meter to reset the held value. The host leases
the tap while the meter is visible and active. In and Out use the host's effect
level readings where available. If the stream is absent or has a different
kind, the host uses its existing effect readings when available and otherwise
shows an empty meter. A meter has no parameter, label or choice bindings.

## Device catalog labels

An optional root `metadata` object contains `names` and optional `descriptions`
maps, keyed by the document's locales. Each nonempty map includes the default
locale. The host obtains these labels at scan time. It falls back to the CLAP
descriptor name when the plugin supplies no valid localized names. Vendor,
plugin ID, role and features still come from the descriptor.

The same field is accepted as optional decoration in v1. Invalid decoration
does not affect a v1 panel, and older v1 hosts ignore the field.
