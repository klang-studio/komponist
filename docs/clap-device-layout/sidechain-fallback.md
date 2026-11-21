# CLAP sidechain fallback v1

A CLAP plugin such as a compressor can use its main signal as a detector source when its
sidechain is disconnected. The [C header](clap-sidechain-fallback.h) defines a plugin-side
extension that tells a host to provide that fallback. It uses the standard CLAP audio ports
and audio ports configuration interfaces for the actual buffers and configuration.

After a successful plugin initialization, while the instance is still inactive and on the CLAP
main thread, the host queries `clap_plugin_t.get_extension` using
`org.klang-studio.komponist.sidechain-fallback/1`. The plugin returns a pointer to an immutable
`komponist_clap_sidechain_fallback_t` table valid until destruction, or null if unavailable.

For mode `KOMPONIST_CLAP_SIDECHAIN_MAIN_INPUT` (1), the table's `auxiliary_config_id` is a
standard CLAP audio ports configuration ID. The host enumerates `clap.audio-ports-config`
and selects the advertised configuration while the instance is still inactive. The selected
configuration must describe a main and an auxiliary input. The sentinel
`KOMPONIST_CLAP_SIDECHAIN_CURRENT_CONFIGURATION` uses the current audio port configuration
without selecting a different one. The host queries the input ports before activation.
[CLAP 1.2.8 audio ports configuration](https://github.com/free-audio/clap/blob/ca1b43ee2b4f381e94e4170e622d60b92108b128/include/clap/ext/audio-ports-config.h)

For each process block, the host writes the audio from a connected source to the auxiliary
input, including when that audio is silent. When no source is connected, the host writes the
unprocessed main signal there. This requires no change to the selected port configuration when
a source connects or disconnects. In Komponist, an absent extension, an unsupported mode or an
unavailable configuration uses the usual silent input for disconnected auxiliary ports.
The extension does not participate in authorization to load a bundle in-process.

Plugins can declare ordinary stereo processing without an auxiliary port as their initial
configuration, then expose another configuration for sidechain-capable hosts. Presse uses
this pattern. It compresses from its main input in hosts that use the default configuration.
