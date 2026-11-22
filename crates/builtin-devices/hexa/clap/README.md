# Hexa CLAP bundle

Stable plugin ID `org.klang-studio.komponist.hexa`. Existing project devices use
the builtin ID `pisynth`. The parameter inventory includes all previous wire
slots and macro keys.

The versioned CLAP state field retains the entire Hexa patch, including
filters, routing, modulation and effects that are outside the visible
parameter bindings. `legacy_patch_to_clap_state` converts a project patch or
factory preset into a standard CLAP state stream accepted by the pinned
wrapper.

The public v1 layout provides fixed controls for every parameter. For an
authenticated installed Hexa bundle, komponist's device editor additionally
provides the interactive FM routing diagram and operator selector. It binds the
existing controls to the authoritative author-to-CLAP IDs. The Presets browser
and device menu load factory patches as ordinary CLAP state.
