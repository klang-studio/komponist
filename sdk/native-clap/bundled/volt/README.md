# Volt CLAP bundle

Stable plugin ID `org.klang-studio.komponist.volt`. Existing project devices use
the builtin ID `knurr`. The same device parameter keys back the CLAP bindings,
and categorical state uses stable choice IDs.

The public v1 panel exposes every sound parameter, including each LFO's tempo
division. For an authenticated installed Volt bundle, komponist provides the
filter, oscillator, envelope and LFO views in its device editor. The envelope
and LFO selectors change which parameters the graph and controls show. Factory
presets from the device crate load as ordinary CLAP state and remain available
from the device menu after saving and reopening a project.
