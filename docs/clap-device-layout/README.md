# Native CLAP device layout

A CLAP plugin can publish a native device panel for komponist. This document, the
[C header](clap-device-layout.h), the [JSON schema](schema.json), and the
[layout reference](layout.md) define the contract. The [example](example.json) shows graphs
and a canvas. The [C control fixture](../../test-plugins/native-layout/c-fixture/controls.json)
binds a continuous Gain at CLAP ID `42` and a stepped Mode enum at CLAP ID `7`.

Plugins can also publish [device assets](device-assets.md) for linked files and offline
preparation, and [sidechain fallback](sidechain-fallback.md) for disconnected inputs.

## Discovery and lifetime

After a successful `clap_plugin_t.init`, the host calls `clap_plugin_t.get_extension` on the CLAP
main thread with the exact ID `org.klang-studio.komponist.device-layout`. The plugin returns
either a pointer to a const `komponist_clap_device_layout_t` table or null. Null indicates that
the extension is unavailable. The bundle uses the standard CLAP entry and factory for
discovery and initialization. The reference fixture builds against these pinned CLAP headers.
[CLAP 1.2.8 plugin header](https://github.com/free-audio/clap/blob/ca1b43ee2b4f381e94e4170e622d60b92108b128/include/clap/plugin.h)

The plugin owns the table and keeps it valid until the instance is destroyed. All callback calls
for this extension occur on the CLAP main thread between successful initialization and destruction.
The host supplies the same initialized plugin pointer that returned the extension. The layout and
its byte count are immutable for that instance. Each call borrows the two output pointers only for
its duration. The host owns the destination buffer and any copy of the descriptor.

## Buffer protocol

The caller passes a writable `uint32_t` as `required_or_written`. The output length and the
destination buffer occupy disjoint storage. During a size probe the caller
passes `destination == NULL` and `capacity == 0`. When the probe reports a valid size, the host
supplies a writable buffer of that size in a second call. The plugin copies the complete UTF-8
document with no trailing NUL. The size is measured in bytes, including JSON whitespace, with a
range of 1 through 65,536.

| Status | Value | Output length | Destination |
| --- | ---: | --- | --- |
| `KOMPONIST_CLAP_LAYOUT_OK` | 0 | Bytes copied, in the valid range and at most `capacity` | Exactly the complete document |
| `KOMPONIST_CLAP_LAYOUT_ABSENT` | 1 | Zero | Unchanged. This instance has no layout |
| `KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL` | 2 | Required bytes, in the valid range and greater than `capacity` | Unchanged. This is the normal response to a probe for an available layout |
| `KOMPONIST_CLAP_LAYOUT_ERROR` | 3 | Zero, when an output pointer is available | Unchanged |

A null `required_or_written` returns `ERROR` without writing either output. A null destination
with nonzero capacity also returns `ERROR`. With a non-null destination and adequate capacity,
an available document is copied in full. The plugin must stay within the supplied capacity.

The host checks the probe status and byte count before allocating. It accepts a copy only on
`OK` when the reported byte count equals the valid probe count. Unknown statuses, inconsistent
statuses and lengths, a null callback or invalid UTF-8 invalidate the descriptor. The reported
length delimits the bytes for validation.

## Document and geometry

The payload is a single UTF-8 JSON object with unique member names. Integer fields use JSON
number tokens without a fraction or exponent. It declares 1–8 locales, a default locale and
1–4 pages, or one page per pad for a pad-selected panel. Page keys are unique within the
document, stable across updates and match `[a-z][a-z0-9_-]{0,31}`. Pages and cells appear in
array order. Each page has 1–14 columns, exactly 3 available rows and 1–84 cells.

Cell coordinates are zero-based. `col` and `row` name the upper-left grid position, while `cols`
and `rows` give a positive span. Every cell fits inside the page
rectangle. A full cell omits `half` and occupies both halves of all columns it spans. A half cell
sets `half` to `top` or `bottom` and occupies that half of each column it spans. Two cells may
share a grid position when they occupy opposite halves. Any shared occupied half is an overlap.

| Widget | Column span | Row part | CLAP parameter |
| --- | ---: | --- | --- |
| `knob` | 1 | Full | Numeric |
| `knob_large` | 2 | Full | Numeric |
| `number` | 1–2 | Top or bottom | Numeric |
| `toggle_button` | 1–14 | Top or bottom | Stepped with plain range `[0, 1]` |
| `segment` | 1–14 | Top or bottom | Stepped enum with 2–16 choices |
| `dropdown` | 2 | Top or bottom | Stepped enum with 2–16 choices |

The [layout reference](layout.md) describes graphs, canvases and additional controls. A
binding supplies its numeric CLAP ID in `param_id`. The valid ID range is 0–4,294,967,294.
`CLAP_INVALID_ID` is reserved. A parameter can appear in multiple cells or pages. The host maps
the ID with `ParamId::plugin`. The same ID identifies the control in UI edits, automation, undo
and restoration.

Each binding must resolve to a visible, writable parameter with a finite and increasing plain
range. Parameters flagged `CLAP_PARAM_IS_HIDDEN`, `CLAP_PARAM_IS_READONLY` or
`CLAP_PARAM_IS_BYPASS` are excluded. Numeric widgets use the CLAP range and step semantics.
Toggles require `CLAP_PARAM_IS_STEPPED`. Choices require both `CLAP_PARAM_IS_STEPPED` and
`CLAP_PARAM_IS_ENUM` with integer bounds representable as signed 32-bit values. `choices` is
required for `segment` and `dropdown` and is omitted for other widgets. Each choice supplies
`value` and localized `labels`. Values must enumerate every integer from the advertised minimum
through the maximum exactly once, in ascending order.

The CLAP parameter inventory remains authoritative for IDs, ranges, defaults, steps, flags and
automation. The plugin supplies numeric value formatting through its CLAP parameter extension.
Choice and cell labels provide display text. The only stored plugin state is its standard CLAP
state. An accepted descriptor is presentation metadata for the current instance.
[CLAP 1.2.8 parameter header](https://github.com/free-audio/clap/blob/ca1b43ee2b4f381e94e4170e622d60b92108b128/include/clap/ext/params.h)

## Localized labels

Locale tags use an ASCII subset of BCP 47 with a 2–3-letter language subtag and up to two
hyphen-separated subtags of 2–8 ASCII letters or digits. Tags compare case-insensitively and
must be distinct within `locales` and each `labels` object. `default_locale` and every label
key must be declared in `locales`.

Page and choice label maps include a label for the default locale. A cell label map is optional
and may omit the default locale. Each label contains 1–128 UTF-8 bytes, includes at least one
non-whitespace character and excludes C0 and C1 controls. Labels are displayed as plain text.

For a localized label, look up the requested locale, then its primary language subtag, then
`default_locale`. A cell with no matching label uses the CLAP parameter name.

## Layout validation

The optional `author_ids` map contains one to 256 stable author keys mapped to numeric
CLAP IDs. It must be a bijection with the live parameter inventory. Keys are nonblank
UTF-8 strings of at most 128 bytes without control characters.

The host ignores unknown optional fields and rejects unknown required capabilities. A
plugin declares required capabilities as up to eight unique strings matching
`[a-z][a-z0-9._-]{0,31}`. The [layout reference](layout.md) describes the supported
capabilities. The host validates geometry, labels and bindings against the live CLAP
inventory before drawing the panel and revalidates it after inventory changes.

Before 1.0, komponist supports the current contract under one extension ID. A missing,
unsupported or malformed layout leaves the standard CLAP parameter view and plugin
editor available.

## Interoperability prototype

The [C fixture and nice-plug spike](../../test-plugins/native-layout/README.md) provide executable
examples of discovery, buffer transfer, validation and stable CLAP parameter IDs.

## Rust author SDK

The [author guide](../../crates/plugin-sdk/README.md) covers writing a Rust device with the SDK,
building and validating its CLAP bundle, and installing it in komponist.

See [CLAP preset interoperability](presets.md) for factory discovery and localized preset names.
