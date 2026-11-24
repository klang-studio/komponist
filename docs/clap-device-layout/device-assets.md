# CLAP device assets v1

A CLAP plugin can publish file slots for host-managed project links and request time to
prepare its assets for an offline render. The [C header](clap-device-assets.h) defines the
ABI for `org.klang-studio.komponist.device-assets/1`.

The host queries the extension on the CLAP main thread after successful initialization.
The returned table is immutable and valid until instance destruction. The host uses the
original plugin pointer for every call. File slots use the `snapshot` and `set_bindings`
callbacks. Offline preparation uses the independent `prepare_offline` callback. Null
callbacks indicate an unavailable operation.

## File slots

`snapshot` returns one complete inventory in display order. A slot has a numeric ID, a role
such as `sample` or `impulse-response`, a short display label, a file kind and current status.
Use stable IDs across compatible plugin versions and sessions. Keep slot IDs, roles and kinds
fixed for the life of an initialized instance. The host uses the ID to persist that slot's
project link, and uses the file kind when offering a file picker or accepting a drop.
When the file-slot callbacks are present, the host retries a temporarily unavailable snapshot.

The host supplies resolved absolute UTF-8 paths in `set_bindings`. An update can set or clear
multiple existing slots at once. Slots omitted from the update keep their bindings. Reject
malformed updates and unknown IDs without applying any part of the update. Accepting an update
means the bindings have changed. Loading files can continue asynchronously. A binding with
`reload: true` asks the device to load that path again even if it is already bound. A clear
binding and project restoration use `reload: false`.

The host owns the project's relocatable links, including relative path resolution, missing
file reporting, rebasing and copying during project save. The plugin owns the runtime files
and loader state. The host may reapply its bindings after restoring standard CLAP state.
After binding a path that does not exist, report the slot as `Missing`.
Collect and Save copies non-audio assets that the user selected for the current document session.
A link read from the project file can still resolve and play without granting permission to copy it.

The host may read snapshots and set bindings while audio processing is active. Both callbacks
run on the main thread. Use a worker for expensive file access and decoding, and keep the
audio path independent of callbacks and worker completion. Both buffers are borrowed for
the call only.

## Payloads

The canonical codec is `komponist_ipc::device_assets`. Both payloads use postcard with one
length-prefixed sequence of 1–32 records and no trailing bytes. Struct fields and enum
variants appear in the order shown below. IDs must be distinct in each payload.

| Payload | Record fields in wire order |
| --- | --- |
| Snapshot | `id: u32`, `role: string`, `label: string`, `kind: AssetKind`, `status: AssetStatus` |
| Bindings | `id: u32`, `path: Option<string>`, `reload: bool` |

`AssetKind` variants are `AudioSample` and `File`. `AssetStatus` variants are `Empty`,
`Loading`, `Ready`, `Missing`, and `Failed(string)`. `None` in a binding clears that slot.
Roles match `[a-z][a-z0-9._-]{0,31}`. Labels and failure reasons are nonblank UTF-8 without
control characters. Their maximum lengths are 128 and 256 bytes, respectively. A path
contains 1–4,096 UTF-8 bytes and excludes NUL. The snapshot and bindings payload limits
are 16,384 and 135,168 bytes. Unknown variants, invalid lengths and malformed payloads
are rejected.

## Offline preparation

The host assigns a nonzero request ID to each render and passes its positive finite sample
rate in Hz and tempo in BPM. The first call for an ID starts preparation. Repeating the
same ID with the same settings polls that request without restarting work. The plugin
returns `Preparing` while work is pending, `Ready` when offline processing can start, or
`Failed` if it cannot prepare. The host treats unknown results as failure.

The host starts and polls with audio processing detached, after applying the file bindings
needed for the render. Once ready, the plugin holds the prepared result for that render.
An all-zero call retires or cancels the request and returns `Ready`. The host clears the
request while processing is detached. A later render uses a new ID even if the sample rate
and tempo are unchanged. The control callback should return promptly while decoding or
time-stretching proceeds on a worker.
