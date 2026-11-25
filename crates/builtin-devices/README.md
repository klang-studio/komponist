# Built-in devices

Each built-in instrument, audio effect and MIDI effect keeps its implementation, CLAP entry
and tests in one device directory. The `clap/` package produces the standalone bundle and
uses the shared adapter in the [Plugin SDK](../plugin-sdk/). See [Bundled CLAP](BUNDLED-CLAP.md)
for the release inventory and tests.

The [device core](core/) defines the unstable Rust API for catalog metadata, parameters, layouts,
state, presets and processor adapters. [Hexa](hexa/) and the [shared sampler engine](sampler-shared/core/)
have separate DSP cores; [Surf](surf/) runs on [Volt](volt/)'s engine with its own oscillators. The
[registry](registry/) supplies the catalog and processor factory.

## Add a device

1. Add a crate here and declare its path in the root `Cargo.toml` under `[workspace.dependencies]`.
   Keep the DSP, parameter table, layout, state and factory presets with the device.
2. Define its `Spec` and `CatalogSpec`. Choose a permanent catalog ID for project files and stable
   parameter IDs for state and automation. Implement the processor and add any state migrations
   next to the device's state handling.
3. Add the DE and EN catalog keys and wire the generated name accessor to `CatalogSpec::name_in`.
   Follow [I18N §8](../../docs/I18N.md#8-built-in-devices-and-default-names-4) for parameter and layout
   labels, preset names and aliases.
4. Add the crate as a dependency of the registry and register its spec and factory. Check the
   order, role and category in the browser.
5. Add a musically useful example to `showcase::showcase_project()` and test it with `--demo`.
   Follow [AGENTS.md rule 15](../../AGENTS.md#rules-for-all-agents) for the mix and showcase tests.
6. Test the processor, parameter and layout contracts, preset/state round trips and realtime
   allocation behavior. Run the catalog and app tests as well as `scripts/check.sh`. Build the
   CLAP and VST3 test fixtures with `scripts/build-test-plugins.sh` before the workspace check.
