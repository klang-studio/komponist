# CLAP preset interoperability

A CLAP bundle can advertise presets with the standard preset-discovery factory. The provider
declares embedded locations and files or directories to index. For each preset it supplies a
display name, compatible plug-in IDs and an opaque source consisting of the location and load
key. Use the same source with the standard `clap.preset-load/2` main-thread callback. A host can
capture and restore an opaque user preset with `clap.state-context/2` and the PRESET context.
`clap.state` is the fallback for plug-ins without state contexts.

- [CLAP preset-discovery factory](https://github.com/free-audio/clap/blob/main/include/clap/factory/preset-discovery.h)
- [CLAP preset loading](https://github.com/free-audio/clap/blob/main/include/clap/ext/preset-load.h)
- [CLAP state](https://github.com/free-audio/clap/blob/main/include/clap/ext/state.h)
- [CLAP state contexts](https://github.com/free-audio/clap/blob/main/include/clap/ext/state-context.h)

## Optional aliases and localized display names

CLAP discovery provides the `add_extra_info` callback. A provider can use the following
optional keys to describe the current preset.

| Key | Value |
| --- | --- |
| `org.klang-studio.komponist.preset-alias` | An earlier exact preset name. Repeat for each alias. |
| `org.klang-studio.komponist.preset-name.<language-tag>` | A display label for the given language tag, such as `de`. |

The default display name comes from `begin_preset`. Keep the load key stable when a display
name changes. Consumers can resolve an old name against the default name and aliases for
the original plug-in ID, then use the resolved source to load. A provider can offer the same
preset to ordinary CLAP hosts that ignore this optional metadata. komponist ignores unknown
keys, malformed entries and duplicate aliases. If a translation is unavailable, it shows
the provider's default display name.

Hexa, Volt, Surf, the five audio effects, Arpeggiator and Scale publish factory presets
from their bundles. The Sampler and Klopfer bundles have no factory presets. Existing states
carry their complete parameter values, and the host recognizes old on-disk preset names. For
example the EQ accepts both the current `Warm Bass` load key and the earlier `Bass Wärme` key.

The browser searches the provider's display names and aliases. The device card and floating
plugin frame expose the same discovered presets for a running device. Applying one records the
resulting project state as a single undo step. Adding a device from the browser prepares its
preset state in an isolated process before the new device first activates.

The device card's menu can save the current settings as a `.kppreset` file or load a file into
a running instance of the same CLAP ID. Files hold opaque preset-context state. The project
receives ordinary project state after loading the file. CLAP itself does not standardize
a portable preset file container.

## Inspecting a bundle

Run `komponist --bridge --scan-presets /path/to/plugin.clap` to list the declared CLAP presets
as JSON from a disposable scanner process. A bundle without a discovery factory produces an
empty list. Unreadable paths and unsupported plug-in IDs are skipped. Discovery skips a
provider that fails initialization, and discards rows from a location whose metadata receiver
reports an error. Locations, directory traversal, and metadata have size limits. Directory
traversal uses declared preset file extensions, follows a symlink declared as a location,
and skips symlinks within that location. The load key in JSON is a byte array so it can
round-trip without interpreting the provider's key.

The normal catalog scan caches the rows after a successful CLAP descriptor scan and associates
them with the exact bundle path and plug-in ID. The optional preset scan runs in a separate
process with a three-second limit. Malformed output, a crash, or a timeout leaves the plug-in
available in the catalog and keeps any previously cached preset list. The next catalog scan
retries incomplete preset discovery even when the bundle is unchanged. A bundle change also
refreshes its preset list.

The hosting API reports unsupported preset loading and refused state operations as errors.
A failed contextual state operation is surfaced to the caller. A failure can mean the
plug-in attempted a load without completing it, so callers should only commit the new state
on success.

Packaged builds start the bridge from the app executable. Quit the app before replacing or
rolling back the installation. Custom launchers that use a separate bridge executable need the
bridge from the same app version because startup checks the protocol version.
