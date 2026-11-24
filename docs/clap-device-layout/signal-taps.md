# CLAP signal taps

A plugin can expose mono samples for a spectrum, detector levels in dBFS,
or gain reduction in positive dB. The host draws
these layers as described by [layout v2](layout-v2.md). The [C header](clap-signal-taps.h) defines
the optional `org.klang-studio.komponist.signal-taps/1` extension.

The host calls `count` and `info` on the main thread after initialization.
The ordered inventory contains one to four streams and is fixed for the
instance. Each key starts with a lowercase ASCII letter and can contain lowercase
letters, digits, hyphens and underscores. It is at most 32 bytes, followed by
zero padding when shorter. `info` writes a descriptor and returns 1.
`audio_frames_per_sample` specifies a fixed cadence from 1 to 2048 audio frames. A nonzero `reference_rate` scales the interval
by the nearest whole ratio of the actual audio rate to that reference, with
a minimum multiplier of one.
`capacity` requests a host history of 2 to 65,536 samples. The host rounds it up
to a power of two.

The host calls `begin` before processing a block while a view has an active
lease. Each set bit selects the corresponding index from the inventory.
After processing, `read` writes the selected stream's finite samples in time
order and returns their count. A return of zero indicates no new samples.
The supplied writable capacity is at most 2048 samples per stream. Both calls
run on the audio thread, with storage allocated before processing. Plugins
should collect data only for the requested interval. With no leased stream,
the host skips both callbacks and does not copy any samples.

A graph's optional `signal_taps` object maps layer roles to inventory keys.
An EQ can bind `spectrum` to a spectrum source. A compressor can bind `level` to
a level history. A gain-reduction history can bind `level` and `reduction`.
A vertical gain-reduction meter uses `reduction_tap` to name a reduction stream.
Missing streams leave the corresponding data layer empty. The existing
[audio meter extension](audio-meter.md) remains available for level meters.
