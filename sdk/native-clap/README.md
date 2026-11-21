# Author a native CLAP device for komponist

This workspace contains a source-pinned [Rust SDK](author/) and a [stereo gain example](example/).
The example is an ordinary CLAP audio effect. It reports Gain and Mix through the standard CLAP
parameter extension and supplies a localized native panel through the [device-layout v1
extension](../../docs/clap-device-layout/README.md). A CLAP host can load the bundle as usual.
Komponist runs DSP in its plugin bridge and draws the validated panel in the app.

## Build and verify the example

Run these commands from the repository root, on the operating system and architecture where you
will use the bundle. Use the repository's Rust toolchain and the platform linker (GCC or Clang
on Linux, Xcode Command Line Tools on macOS, MSVC Build Tools on Windows). The standalone SDK
workspace builds the CLAP example independently of the app. It uses its own lockfile.

```sh
cd sdk/native-clap
cargo test --locked --workspace
cargo run --locked -p native-clap-xtask -- bundle --release
cargo run --locked -p native-clap-xtask -- validate target/bundled/native_clap_example.clap
```

With the default Cargo target directory, the bundle is at
`sdk/native-clap/target/bundled/native_clap_example.clap`. The app and the bundle must target
the same architecture. The bundle command accepts `--target TRIPLE` and `--out-dir DIRECTORY`
when you need to specify the build target or stage the output elsewhere.

| Build on | Target used by release checks | Bundle |
| --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | A `.clap` shared library |
| macOS with Apple Silicon | `aarch64-apple-darwin` | A `.clap` directory containing `Contents/MacOS/` and `Contents/Info.plist` |
| Windows | `x86_64-pc-windows-msvc` | A `.clap` DLL |

`validate` loads the generated bundle through a standalone CLAP host. Use it to check discovery,
the defaults and IDs reported by `clap.params`, their match to the layout, and English and German
labels. Build the plain CLAP variant
into a separate directory to exercise the standard parameter view:

```sh
cargo run --locked -p native-clap-xtask -- bundle --release --no-default-features --out-dir target/bundled/plain
cargo run --locked -p native-clap-xtask -- validate target/bundled/plain/native_clap_example.clap --plain
```

### Install and rescan

Copy `native_clap_example.clap` into a per-user CLAP folder. Copy the entire bundle directory on
macOS. The following destinations are scanned by komponist:

| Platform | Default per-user folder |
| --- | --- |
| Linux | `~/.clap/` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP/` |
| Windows | `%LOCALAPPDATA%\Programs\Common\CLAP` |

You can choose a separate developer directory using **Settings → Plugins → + Folder**. Select
**Rescan** in the same tab and add **Native Gain Example** from the browser to a track.
Change Gain or Mix, save the project and reopen it to check that the value is restored. With the
native build, change the language between English and German in **Settings → Appearance → Language**
to see the localized panel.
Both builds have the CLAP ID `org.klang-studio.komponist.native-clap-example`. Install one build
at a time when testing manually. Quit komponist, replace the installed bundle, then relaunch and
rescan. Reopen the project or insert a fresh instance to see the plain build's generic controls.
[App and plugin installation](../../docs/INSTALL.md).

The automated app check loads both staged bundles directly. From the repository root, point
`KOMPONIST_SDK_EXAMPLE` at the native bundle and `KOMPONIST_SDK_EXAMPLE_PLAIN` at the plain bundle,
using absolute paths, and run:

```sh
cargo test --locked -p komponist-app --test it sdk_example_uses_native_layout_and_falls_back
```

On Linux or macOS, set the variables with `export KOMPONIST_SDK_EXAMPLE=/path/to/native_clap_example.clap`
and `export KOMPONIST_SDK_EXAMPLE_PLAIN=/path/to/plain/native_clap_example.clap`. In PowerShell, use
`$env:KOMPONIST_SDK_EXAMPLE = 'C:\path\to\native_clap_example.clap'` and
`$env:KOMPONIST_SDK_EXAMPLE_PLAIN = 'C:\path\to\plain\native_clap_example.clap'`.

## Add a panel to your device

Use the example as a starting point. The SDK's Rust API follows the komponist device core.
The published v1 CLAP extension is the compatibility contract for the compiled bundle.
Keep the device's `Cargo.lock` and pin the komponist source revision that supplies its SDK and
device core. For example, with a pinned checkout in a sibling directory named
`komponist-source`, the device can use these dependencies:

```toml
[dependencies]
komponist-device-core = { path = "../komponist-source/crates/builtin-devices/core" }
komponist-native-clap-sdk = { path = "../komponist-source/sdk/native-clap/author" }
nice-plug = { path = "../komponist-source/sdk/native-clap/vendor/nice-plug/crates/nice-plug", default-features = false }
```

The SDK and the device use the same patched nice-plug wrapper. See the pinned revision and the
reproduction steps in the [wrapper provenance notes](vendor/nice-plug/VENDORED.md).

Define your device's audio processing, parameter declarations and defaults in nice-plug.
Implement `NativeClapLayout::panel()` to return a `Panel` with the same contents each time.
The `grid` uses the device core's `DeviceLayout` with fixed parameter bindings. Supply one
`PageLabels` entry per grid page in page order and use stable page keys. Declare `en` and `de`
for the English and German labels, and set the fallback in `default_locale`. Bind each cell
to its nice-plug author ID, such as `layout::knob(0, 0, "gain")`.

In `ClapPlugin::clap_custom_extension`, return `extension::<Self>(id)` and export the CLAP
entry with `nice_export_clap!`. The SDK resolves the author strings through the live
wrapper's parameter mapping and validates the resulting v1 metadata against the standard
CLAP parameter inventory.
[Example plugin](example/src/lib.rs), [bilingual panel](example/src/native.rs).

Keep the CLAP plugin ID and every shipped parameter's author ID stable. This includes nested
ID prefixes. Reordering declarations or translating labels keeps the IDs when the author
strings stay the same. Renaming an ID or changing the wrapper's ID mapping can break saved
automation. Keep page keys stable for layout compatibility. When you update the nice-plug pin,
compare the CLAP IDs reported by an independent host to those from your previous release. The
workspace's `validate` command is specific to Native Gain Example. Its scan code shows how to
check a bundle against your own expected plugin descriptor, parameter IDs and layout. Resolve ID
collisions before a first release and plan an explicit migration before changing an existing ID.

### Choice controls

CLAP supplies values, ranges, defaults, steps and automation. The layout supplies placement
and labels. For a segment or dropdown, declare a nice-plug `EnumParam`. The patched wrapper
reports `CLAP_PARAM_IS_ENUM` and `CLAP_PARAM_IS_STEPPED` for typed enums. A plain stepped
integer reports only the stepped flag, so use a numeric control for it.

Bind the cell to the enum's author ID and add a matching entry in `Panel::choice_labels`.
V1 allows 2–16 choices. Supply one label map per advertised integer value, in ascending
order from the CLAP minimum to its maximum. Each choice needs a label for `default_locale`. For example, for a `mode`
enum with CLAP values 0 and 1 and English and German labels:

```rust
use komponist_native_clap_sdk::{Cell, Half, ParamId, WidgetKind, layout};

let mode_cell = Cell::half(0, 1, 2, Half::Top, WidgetKind::Dropdown, layout::param("mode"));
panel.grid.pages[0].cells.push(mode_cell);
panel.choice_labels.insert(
    ParamId::new("mode"),
    vec![
        [("en".into(), "Clean".into()), ("de".into(), "Klar".into())].into(),
        [("en".into(), "Warm".into()), ("de".into(), "Warm".into())].into(),
    ],
);
```

The SDK checks the full choice sequence against the live CLAP flags and range. Keep the
meaning of each released enum value stable for existing automation and state.

### Optional editor

Your plugin may also define a normal nice-plug editor. Enable nice-plug's `editor` feature and
implement the editor as usual. Its `clap.gui` extension remains independent of the layout.
Komponist uses the native panel for its device controls and can open your editor via the plugin
frame when the editor supports the platform's embedded API (X11, Cocoa or Win32). If opening
the editor fails, the app can show its parameter view. That view uses a valid layout when available
and standard CLAP parameters for the generic fallback. The example is headless, so it is useful for checking
the generated and generic views without an embedded editor.

## Prepare release metadata

Validate the bundle you intend to ship and keep its CLAP structure in the archive. From this workspace, pass the bundle and the packaged bytes to the metadata
command. For example, for a Linux x86-64 release:

```sh
target=x86_64-unknown-linux-gnu
archive="target/bundled/native-clap-example-$target.tar.gz"
cp ../../LICENSE target/bundled/LICENSE
cp vendor/nice-plug/LICENSE target/bundled/LICENSE-nice-plug
tar -czf "$archive" -C target/bundled native_clap_example.clap LICENSE LICENSE-nice-plug
cargo run --locked -p native-clap-xtask -- metadata \
  target/bundled/native_clap_example.clap "$archive" "$target" \
  "$(git rev-parse HEAD)" "target/bundled/native-clap-example-$target.json"
```

Run metadata generation on a native host for each target and supply the reviewed source
revision as a full Git commit ID. The JSON records the discovered CLAP plugin ID and version,
the platform target, the layout extension and schema version, the source revision and license,
and the archive name, size in bytes and SHA-256.
Store it alongside the corresponding archive. The size and digest describe that exact archive.

[#59](https://github.com/klang-studio/komponist/issues/59) owns the community repository and
distribution. Its platform CI will build from reviewed source, publish artifact URLs and
digests in the signed index, and supply the metadata used by the app to verify the index with a
pinned key and check the downloaded archive hash before a managed install.
[#60](https://github.com/klang-studio/komponist/issues/60) owns the community workspace and
its `devices/<name>/` layout, SDK/core pin, maintainers, review and required platform checks.
Its contribution guide covers licensing, DE/EN, tests, demo presets, descriptions and screenshots. The SDK example can be
built and installed manually with the workflow above.
