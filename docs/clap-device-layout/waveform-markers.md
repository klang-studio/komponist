# CLAP waveform markers

The General Sampler bundle exposes a control-thread endpoint for reading and editing slice
markers on its `AudioSample` [asset slot](device-assets.md) `0`. The [C header](clap-waveform-markers.h)
defines the ABI for `org.klang-studio.komponist.waveform-markers`. This endpoint is separate
from the layout, display canvas, and [waveform preview](clap-waveform-preview.h) callbacks.

Query the exact extension ID after successful CLAP initialization. Use the original plugin
pointer and a supported asset slot ID on every call. Both callbacks run on the CLAP main thread
and may run while audio processing is active. The immutable table lives until destruction. The
caller retains its buffers through each synchronous call. The plugin publishes edits through its
existing realtime slice snapshot.

## Snapshot

`snapshot` writes a complete postcard payload of at most 4,096 bytes and returns its length.
It returns zero for an unsupported slot, invalid buffer or insufficient capacity. The payload
fields in order are `revision: u64`, `editable: bool`, `positions: Vec<f64>`,
`slice_options: Option<Vec<SliceOptions>>`, `default_options: Option<SliceOptions>` and
`preparation: Option<Preparation>`. `Preparation` has variants `Preparing` and
`Unavailable` in that order and describes a derived playback source, such as
tempo-stretched audio. A provider sends `None` when there is no preparation status.
The canonical codec is `komponist_ipc::waveform_markers`.

The positions are finite, positive and strictly increasing source seconds. The list holds up to
127 markers. It includes saved positions outside the current trim or beyond a shorter relinked
file. `editable` is true when the source is ready and slice playback mode is active. The revision
identifies this snapshot's positions within the current slot and instance. A successful edit,
state restore, or changed file binding invalidates previous revisions. Use a new snapshot after
recreating the instance.

Providers with active slice settings include one options entry for the implicit first slice
and one for each slice beginning at a listed marker. The options can be present while `editable`
is false. `SliceOptions` has these fields in order: `gate: bool`, `reverse: Option<bool>`,
`fade_in_ms: Option<f32>` and `fade_out_ms: Option<f32>`. Missing options for reverse and the
two fades inherit the corresponding sample-wide values from `default_options`. The defaults
accompany the options list and include all three resolved values. Fades are finite and range
from 0 to 10,000 ms. A provider can send the resolved defaults outside slice mode to show the
sample-wide fade overlays.

## Edit

`edit` accepts a complete postcard payload of at most 32 bytes. The fields in order are
`revision: u64` and `operation`. Operation variants and their fields in order are
`Insert(f64 seconds)`, `Move(u8 index, f64 seconds)`, `Remove(u8 index)`,
`SetSliceOptions(u8 slice, SliceOptions options)` and `DetectSlices`.
Indices start at zero and positions use source seconds. Both codecs reject trailing bytes and
invalid values.

| Status | Value | Result |
| --- | ---: | --- |
| `INVALID` | 0 | Malformed request, unknown slot, invalid index or position, or size limit reached |
| `APPLIED` | 1 | One edit has been made and is available through CLAP state `save` |
| `UNCHANGED` | 2 | The requested operation would leave the data unchanged |
| `STALE` | 3 | This revision has expired. Refresh the snapshot before deciding on another edit |
| `UNAVAILABLE` | 4 | The source or editing mode is unavailable |

An insert must be strictly inside the current playback trim and at least one source frame from
adjacent markers. It inherits the preceding slice's options, including those of the implicit
first slice. A move retains options and order. The last marker can reach the exact source end.
Existing positions outside current limits stay intact and can be moved back toward the valid range.
Remove deletes one marker and its options. `SetSliceOptions` replaces the settings for slice 0
(the implicit first slice) or slice `index + 1` (the slice starting at that marker index).
It leaves the marker positions intact. `DetectSlices` replaces interior entries with the device's
detected transients strictly inside the playback trim, preserves options at exact matches, and
keeps the implicit first slice. Rejected requests leave saved and live state intact.

Use `snapshot` after `APPLIED` to read the new revision and positions. The caller should capture
opaque CLAP state around an accepted edit to support project persistence and undo. Zoom and pan
are host view operations. Audition uses the [waveform preview](clap-waveform-preview.h)
extension's ephemeral press and release callback.
