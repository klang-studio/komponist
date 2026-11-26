# Device display

A plugin may put a display-only `canvas` cell in its [device layout](layout.md).
A canvas occupies full grid cells. It has a unique key, a localized accessible
name and a localized description. The plugin supplies drawing commands in a
display list. The host applies its theme and clips each list to its canvas.

Return the callback table in [the public header](clap-device-display.h) for
`org.klang-studio.komponist.device-display`. The host calls it on the CLAP main
thread, at most once every 80 ms per device. A plugin may read a snapshot of live
audio data here. Publishing it from the audio thread must use the plugin's
existing nonblocking data transfer. The host supplies a buffer of at most
65,536 bytes. Return zero when no frame is available.

The payload is a UTF-8 JSON object with a `frames` array. Each frame contains
a distinct `key` from the layout and a `primitives` array in painter order.
The host discards frames for keys outside the visible layout. It rejects invalid
payloads before rendering, including on the receiving end of the bridge.
There are at most 16 frames and 512 primitives per payload, and at most 1,024
points in a path. The host scales positions from normalized `[x, y]` to the
canvas, with the origin at the top left. All coordinates and the radius are
finite numbers in the inclusive range 0–1. Circle radii use the shorter side.

The optional `fm_topologies` array carries the current routing for host-rendered
FM diagrams. These entries share the frame limit and key namespace with
`frames`. Each entry includes `key` and one to eight `modulates` masks, one per
operator. Bit `n` in a mask describes an edge to operator index `n`. `outputs`
is a mask of operators that contribute audio, including operators that also
modulate. A self-edge uses the optional zero-based `feedback` index. `algorithm`
is a number from 1 to 255 for a fixed algorithm and can be absent for free
routing. The host rejects bits and indices outside the operator inventory.
Declare `fm-topology` in the layout's `required_capabilities` and set
`fm_topology.selector` on the corresponding canvas. The selector must be on
the same page with one option per operator, in diagram order. A diagram node
changes that selection. Optional `fm_topology.carriers` entries map the grid
`col` and `row` of a mini fader to a zero-based `operator`. They derive their
fill color from the same routing as the diagram. The canvas `name` labels free
routing. Fixed algorithms use the host's localized algorithm number. The canvas
`description` also describes the interactive diagram.

| `kind` | Fields in addition to `tone` |
| --- | --- |
| `line` | `from`, `to` |
| `polyline` | `points`, at least two |
| `rectangle` | `from`, `to`, `filled` |
| `circle` | `center`, `radius`, `filled` |
| `area` | `points` in nondecreasing x order and a horizontal `baseline` y |
| `text` | `at`, `text` (at most 128 UTF-8 bytes without control characters), `alignment` (`left`, `center`, or `right`) |

`tone` is one of `text`, `muted`, `accent`, and `grid`. The host owns fonts,
stroke widths, clipping, and accessibility. Device controls use the declarative
layout and keep their normal parameter gestures.
Set `horizontal_viewport` on the layout's `canvas` object to opt in to host
horizontal zoom and pan. Double click fits the display. These gestures change
only the view.

Rust authors can use `komponist_native_clap_sdk::display::DisplaySource` and
the recording `Painter` through the SDK's nice-plug adapter.
