# Volt CLAP bundle

Stable plugin ID `org.klang-studio.komponist.volt`. Existing project devices use
the builtin ID `knurr`. The same device parameter keys back the CLAP bindings,
and categorical state uses stable choice IDs.

The public v1 panel exposes every sound parameter. The two LFOs and three
envelopes have separate fixed controls, including each LFO's tempo division.
Volt's filter response, oscillator signal, envelope and LFO graphs still need a
public layout capability or an editor before changing the default device
presentation. Existing factory preset values remain in the builtin device crate
for the project migration work.
