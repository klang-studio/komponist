# nice-plug CLAP layout prototype

Spike for [komponist #180](https://github.com/klang-studio/komponist/issues/180). The prototype is a headless stereo gain effect. A regular CLAP host can enumerate its parameters. A host that supports `org.klang-studio.komponist.device-layout` can also read a two-knob layout through the C ABI.

## Run the check

Use a local Git checkout or Git cache of [klang-studio/nice-plug](https://github.com/klang-studio/nice-plug) containing commit `39e9ad41cf84f98762d86b83abdb9c422fcb58a5`. On Linux with Cargo, Python 3 and a linker:

```sh
./verify.sh /path/to/nice-plug-git-repository /tmp/komponist-layout-check
```

Provide a new or empty scratch directory outside the source trees. Omit the second argument to use `mktemp`. The script copies the prototype, archives the exact revision, checks and applies the patch, then runs low-priority Cargo builds with at most three jobs. It rebuilds the prototype after removing the prototype's compiled artifacts in scratch and reuses the compiled dependencies. All writes are inside scratch.

The script uses the locked dependencies offline by default. Set `CARGO_NET_OFFLINE=false` to allow Cargo to fetch missing dependencies. The script prints the path to the staged artifacts and reports. All build products stay in scratch.

Each variant has a `nice-layout-prototype.clap` binary and an `ids.json` report. To inspect it with the independent C probe from #180, pass the binary path and plugin ID `org.klang-studio.komponist.nice-layout-prototype`.

| Build | CLAP parameter enumeration | Layout result |
| --- | --- | --- |
| `baseline` | Gain = 3165055, Mix = 108124 | Layout references Gain then Mix |
| `rebuild` | Gain = 3165055, Mix = 108124 | Same descriptor |
| `reordered` | Mix = 108124, Gain = 3165055 | Same descriptor |
| `collision` | Mix = 2112 at both indexes | `ABSENT`, zero bytes |

The Rust host loads each binary with `libloading` and the published CLAP types from `clap-sys`. It drives the exported entry/factory/instance lifecycle, calls `clap.params` and then calls the layout extension on the same instance. It checks IDs against the parameter names reported by CLAP, and checks the size query, short buffer, exact copy, output guard byte and invalid pointer combinations defined by the layout contract. The rebuild comparison includes the actual CLAP parameter metadata and the parsed layout document.

## Wrapper patch

The patch adds two opt-in APIs at the pinned nice-plug revision.

- `ClapPlugin::clap_custom_extension` returns an optional static, immutable C function table. The wrapper reaches it only after existing CLAP extension dispatch. The hook has a null default and can be called from the threads CLAP permits for extension discovery. The device-layout callback is called on the main thread.
- `Wrapper<P>::custom_extension_param_id` resolves a stable author string ID using the live wrapper's `param_id_to_hash`. It checks both the forward and reverse map lengths against the number of enumerated parameters. That detects duplicate author IDs and distinct author IDs that collapse to the same numeric ID. The helper returns `None` when the inventory is ambiguous. The layout callback reports `ABSENT`, allowing the host to use its ordinary CLAP path.

The pinned wrapper's CLAP parameter inventory enumerates `param_hashes` and resolves metadata by the same hash. The prototype's layout obtains its wire IDs from these existing maps. It avoids a second implementation of the hash, whose behavior is shared with nice-plug's VST3 wrapper. The colliding strings `Aa` and `BB` both reach 2112. Existing nice-plug lookup behavior surfaces the last metadata entry under both indexes in that case. The prototype rejects the entire native descriptor before it can bind any controls.

For a production SDK, prefer a safe author-facing adapter that builds from the wrapper's canonical mapping during initialization, validates the complete author ID set, and stores the immutable bytes for the layout callback. The low-level hook can support that adapter without taking a komponist dependency in nice-plug. Define and verify the supported path for existing layouts, including official nice-plug support or a maintained fork. Further nice-plug revisions should run the same CLAP checks and compare against a released plugin's recorded IDs. Changing an author ID or its nested parameter prefix changes the identity used by automation. Source order and localized labels can change without changing the author IDs.

The pinned CLAP wrapper sets `CLAP_PARAM_IS_STEPPED` for a stepped `EnumParam` but does not publish `CLAP_PARAM_IS_ENUM`. The `segment` and `dropdown` controls require a typed CLAP enumeration, so [#182](https://github.com/klang-studio/komponist/issues/182) must verify or add that flag in the SDK's supported wrapper before enabling those controls. CLAP remains authoritative for numeric values and flags. A step count or a set of labels cannot establish enum semantics.

This spike exercises binary discovery on Linux x86-64. It leaves cross-platform bundling, SDK distribution, app-side validation and bridge integration to their respective work items.
