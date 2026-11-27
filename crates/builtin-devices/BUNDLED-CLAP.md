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
| Surf | `komponist-bundled-surf` | `org.klang-studio.komponist.surf` |
| Klopfer | `komponist-bundled-sampler` | `org.klang-studio.komponist.sampler` |
| General Sampler | `komponist-bundled-general-sampler` | `org.klang-studio.komponist.general-sampler` |
| Arpeggiator | `komponist-bundled-arpeggiator` | `org.klang-studio.komponist.arpeggiator` |
| Scale (Tonleiter) | `komponist-bundled-scale` | `org.klang-studio.komponist.scale` |

The bundles publish their panels through the public device layout extension. The bundled DSP
accepts the device core's 8–384 kHz sample-rate range. Release packages include all twelve bundles,
including Hexa. The app reads the CLAP inventory at runtime and creates each device instance
through its bundle.

Presse initially exposes its stereo compressor configuration. Komponist selects its
standard CLAP sidechain configuration using the public
[sidechain fallback extension](../../docs/clap-device-layout/sidechain-fallback.md).
With no source connected, the detector uses the main input. A connected source drives
the detector independently, including when the source is silent.

From the repository root, run
`cargo test --locked --workspace -p 'komponist-bundled-*'` to validate the
layout metadata against the real CLAP parameter inventory. The Bundled CLAP package
workflow tests the twelve-device archive in-process (D11: bundled devices are hosted
in-process; the bridge scans them and hosts third-party plugins). The current-format project
check for an unpacked package is available with
`python3 scripts/measure-bundled-clap.py --current-projects /path/to/unpacked/komponist`.

## Sampler instrument state

Klopfer (`komponist-bundled-sampler`, `org.klang-studio.komponist.sampler`) and Sampler
(`komponist-bundled-general-sampler`, `org.klang-studio.komponist.general-sampler`)
use the existing sampler DSP and the shared device file decoder. Their audio files are
host-managed assets (see [device-assets.md](../../docs/clap-device-layout/device-assets.md)):
komponist stores the links in the device's `asset_links`, resolves relative links against the
project folder, binds them to the device's audio-sample slots and rebases them on Save As and
Collect and Save. The device offers file picking, waveform, pad and slice editing through its
native panel ([sampler-ui.md](../../docs/clap-device-layout/sampler-ui.md)), and an offline
render waits for asset and tempo preparation.

The CLAP state stores the parameter values and the slice metadata (`sample_edit`) under the
nice-plug persistent field `sample_assets`. A sample link embedded in that field is used by
hosts without the asset extension, which open the saved absolute fallbacks; komponist drops
embedded links of pads it manages itself. File decoding and transfer to the DSP run on the
loader thread. Destroying an instance signals its loader and waits briefly. If file I/O is
still blocked, the loader's native module remains loaded until the host exits so cleanup can
finish as soon as the filesystem responds.
