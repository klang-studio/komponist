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

## Cell selections and synth graphs

A panel can list `cell-selections` in `required_capabilities` to add
page-local segmented selectors. A `selector` cell declares its selection
key, localized `labels` and two to four localized `options`. It occupies
a half cell and changes only the editor selection. The first option is
selected initially.

Another cell on the same page can set `follows` to the selector's key.
For a parameter control, `variants` replaces the fixed CLAP parameter
binding. List one object per option, each with `param_id` and any
localized `labels` or enum `choices`. An envelope, LFO shape or wavetable
graph instead declares `graph.variants`, which supplies the same slot
names with potentially different numeric IDs and an optional `name` per
option. The regular
`graph.slots` apply to every option and must be distinct from the variant
slots.

Add both `panel-selections` and `cell-selections` to use up to eight
options. Set `selector.shared` to `true` on each page that shares the
selection. Selectors with the same shared key must have the same number
and order of options. Each following cell still declares its selector on
its own page. Shared and page-local selections use independent state.

The same capabilities allow a `column_selector` overlay. Give it one option
per column, omit `half`, and list it first on the page. It can span one to
three rows behind the normal controls. Clicking a column, including the
area covered by one of its controls, selects that option. Each option is
also an accessible button.

A graph can set `half` to `top` to occupy only the top half of its last
row. Each preceding row in its span remains full. A `filter_va` graph
shows a synth's VA filter response, with `cutoff`, `resonance` and
`type` slots. An `lfo_shape` graph displays one period using `shape`.
A `wavetable` graph draws the table stack and edits its `position`
using `table` and `position` slots. Its two selection variants address
oscillators A and B in that order. The filter and wavetable can also bind
`mod1_src` through `mod4_amt`, `lfo1_uni` and `lfo2_uni` to show
their modulation range. A filter can also bind `at_cut` for aftertouch.

A `signal` graph displays the oscillator mix over two periods of oscillator A.
It uses `wave_a` and `wave_b` (sine, triangle, saw, square at indices 0–3),
`pw_a` and `pw_b` (pulse widths), `b_oct` and `b_fine` (oscillator B pitch
offsets in octaves and cents), and `mix` and `sub` (balance and sub level).
The graph is read-only.

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

## EQ band controls

An EQ panel may list `eq-band-controls` in `required_capabilities`. Set
`select_band` to `true` on one EQ graph per page to make its selected band
available to controls on that page. A selectable EQ graph binds
`bN_on`, `bN_type`, `bN_freq`, `bN_gain` and `bN_q` for every band.
The selection starts at band 1 and follows the graph handles.

A band-dependent knob, toggle or segment declares `band_controls` in place of
`param_id`, `labels` and `choices`. Include one object per band in ascending
order. Each object supplies a numeric `param_id` and optional localized
`labels`. Each ID must match the same graph slot role for its band.
Knobs bind frequency, gain or Q. Toggles bind enabled state. Segments bind
filter type and supply the five type choices in CLAP order. A choice can
provide `full_labels` for longer text in the parameter inspector while the
segment uses `labels`.

An EQ graph may also bind a Boolean `spectrum_enabled` slot while declaring
this capability. The host leases the named `spectrum` stream only while the
graph is visible and active and this parameter is on.

## Physical parameter display

A panel can present a continuous CLAP parameter with a wire range of 0–1 in
physical units. Add `parameter-display` to `required_capabilities` and place
the mappings in the top-level `parameter_display` object, keyed by decimal
CLAP parameter ID. Mapped IDs must be bound by a control or a graph in the
panel. Hosts that do not support the capability can fall back to layout v1.

For example, parameter 42 can display delay time as 1–2000 ms with a
375 ms default.

```json
"parameter_display": {
  "42": {
    "min": 1, "max": 2000, "default": 375,
    "scale": "logarithmic", "unit": "milliseconds"
  }
}
```

The host maps normalized position `n` to `min + n × (max − min)` for
`linear`, or `min × (max / min)^n` for `logarithmic`. The inverse maps a
physical edit back to the existing CLAP wire value. Automation, plugin state,
and the CLAP parameter inventory keep their existing wire values. Bounds must
be finite, increasing, and have a finite span. A logarithmic range must start
above zero and have a finite ratio. The physical default must lie within the
range and map to the CLAP default, allowing for single-precision rounding.
Units are `none`, `hertz`, `milliseconds`, `percent`, `decibels`,
`decibel_width`, and `ratio`. A percentage uses fractional values, so 0.42
displays as 42%. Use `decibel_width` for whole dB widths without a leading
plus sign, and `ratio` for values such as 4.0:1.

## Conditional knobs

A `knob` or `knob_large` can set `disabled_by` to the CLAP parameter ID of
a separate Boolean toggle on the same page. Declare `conditional-controls`
in `required_capabilities`. The knob remains visible and accessible, and
the host disables its edits while the toggle is on. The condition changes
the UI only. The plugin keeps both parameters and is responsible for how
their values affect processing.

## Device catalog labels

An optional root `metadata` object contains `names` and optional `descriptions`
maps, keyed by the document's locales. Each nonempty map includes the default
locale. The host obtains these labels at scan time. It falls back to the CLAP
descriptor name when the plugin supplies no valid localized names, and to
the descriptor description when the layout supplies no descriptions. Vendor,
plugin ID, role and features come from the descriptor. The browser groups
audio effects by their declared features.

The same field is accepted as optional decoration in v1. Invalid decoration
does not affect a v1 panel, and older v1 hosts ignore the field.
