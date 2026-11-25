# Volt CLAP bundle

Stable plugin ID `org.klang-studio.komponist.volt`. Existing project devices use
the builtin ID `knurr`. The same device parameter keys back the CLAP bindings,
and categorical state uses stable choice IDs.

The layout v2 panel publishes Volt's Sound, Envelopes and Matrix pages,
including the oscillator mix, filter response, envelope and LFO graphs.
The envelope and LFO selectors change which parameters the graphs and controls
show. The layout v1 fallback exposes every sound parameter, including each
LFO's tempo division. Factory presets from the device crate load as ordinary
CLAP state and remain available from the device menu after saving and reopening
a project.
