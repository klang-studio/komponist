# Surf CLAP bundle

Stable plugin ID `org.klang-studio.komponist.surf`. Existing project devices use
the builtin ID `surf`. The same device parameter keys back the CLAP bindings,
and categorical state uses stable choice IDs (the tables by their recipe names,
`basic` … `random`).

The public v1 panel exposes every sound parameter on four pages: both
oscillators and the filter, the three envelopes, both LFOs with their tempo
division, and the matrix. For an authenticated installed Surf bundle, komponist
shows its own device card instead: the 3D wavetable stack with the A/B switch,
the filter graph and volt's envelope and LFO views. Factory presets from the
device crate load as ordinary CLAP state.
