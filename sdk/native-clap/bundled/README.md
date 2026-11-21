# Bundled CLAP authoring

These packages wrap the existing device DSP with the public native CLAP author SDK. Layout
parameters use the existing author IDs, with typed CLAP enums for categorical choices. For
continuous controls, the adapter carries normalized CLAP values and uses each device's own
conversion to DSP units. Lookahead changes take effect on reactivation, alongside the updated
host latency.

| Device | Cargo package | Stable CLAP ID |
| --- | --- | --- |
| Hall | `komponist-bundled-hall` | `org.klang-studio.komponist.hall` |
| Delay | `komponist-bundled-delay` | `org.klang-studio.komponist.delay` |
| EQ | `komponist-bundled-eq` | `org.klang-studio.komponist.eq` |
| Presse | `komponist-bundled-presse` | `org.klang-studio.komponist.presse` |
| Limiter | `komponist-bundled-limiter` | `org.klang-studio.komponist.limiter` |
| Hexa | `komponist-bundled-hexa` | `org.klang-studio.komponist.hexa` |
| Volt | `komponist-bundled-volt` | `org.klang-studio.komponist.volt` |
| Klopfer | `komponist-bundled-sampler` | `org.klang-studio.komponist.sampler` |
| General Sampler | `komponist-bundled-general-sampler` | `org.klang-studio.komponist.general-sampler` |
| Arpeggiator | `komponist-bundled-arpeggiator` | `org.klang-studio.komponist.arpeggiator` |
| Scale (Tonleiter) | `komponist-bundled-scale` | `org.klang-studio.komponist.scale` |

The public v1 layout describes fixed controls. The Delay UI uses separate time and feedback
knobs in place of its XY control. EQ exposes all band controls but needs its response graph and
analyzer. Presse needs its transfer/history graphs and gain-reduction display, and Limiter
needs its gain-reduction display. Factory preset discovery and legacy project migration
remain rollout work. The bundled DSP accepts the device core's 8–384 kHz sample-rate range.

Presse initially exposes its stereo compressor configuration. Komponist selects its
standard CLAP sidechain configuration using the public
[sidechain fallback extension](../../../docs/clap-device-layout/sidechain-fallback.md).
With no source connected, the detector uses the main input. A connected source drives
the detector independently, including when the source is silent.

From the repository root, run
`cargo test --locked --workspace --manifest-path sdk/native-clap/Cargo.toml` to validate the
layout metadata against the real CLAP parameter inventory. The Bundled CLAP package
workflow tests the eleven-device archive through both host routes. For an unpacked test
package, start the app with `KOMPONIST_EXPERIMENTAL_DIRECT_CLAP=1` to use the verified
catalog. Add `KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE=1` to run the same verified devices in
the bridge. The historical project check is available with
`python3 scripts/measure-bundled-clap.py --historical-projects /path/to/unpacked/komponist`.
The experimental runtime attempts the migration when opening a project. If a device
cannot be verified or translated, the original project opens with its legacy devices.
A successful migration is one undoable edit. The test uses disposable project copies.

## Sampler instrument state

Klopfer (`komponist-bundled-sampler`, `org.klang-studio.komponist.sampler`) and Sampler
(`komponist-bundled-general-sampler`, `org.klang-studio.komponist.general-sampler`)
use the existing sampler DSP and the shared device file decoder. Their CLAP state stores
the file links and slice metadata under the nice-plug persistent field `sample_assets`.
The field contains the existing project's `samples` and `sample_edit` JSON objects. Relative
paths stay in the state. The generic CLAP state callback has no project directory, so it
opens the absolute fallback. File decoding and transfer to the DSP run on the loader thread.
Destroying an instance joins its loader before the CLAP library can be unloaded.

The native v1 control pages expose the pad and instrument parameters. A package suitable
as the default for existing sample projects still needs an editor or public host integration
for file picking, waveform and pad or slice editing, project-relative resolution, and waiting
for asset/tempo preparation before offline rendering.
