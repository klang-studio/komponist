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

The public v1 layout describes fixed controls. The Delay UI uses separate time and feedback
knobs in place of its XY control. EQ exposes all band controls but needs its response graph and
analyzer. Presse needs its transfer/history graphs and gain-reduction display, and Limiter
needs its gain-reduction display. Factory preset discovery, legacy project migration and
Presse's host selection of the sidechain-enabled configuration remain rollout work. The
bundled DSP accepts the device core's 8–384 kHz sample-rate range.

From the repository root, run
`cargo test --locked --workspace --manifest-path sdk/native-clap/Cargo.toml` to validate the
layout metadata against the real CLAP parameter inventory of each effect.
