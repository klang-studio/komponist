# CLAP effect meter extension

`org.klang-studio.komponist.audio-meter` supplies actual DSP gain reduction and optional
detector data. Hosts without the extension continue using standard CLAP and the device layout.

```c
#include <stdint.h>
#define KP_AUDIO_METER_DETECTOR_FRAMES 64

typedef struct kp_audio_meter_block {
    float reduction_db;
    uint32_t detector_frames;
    float detector[KP_AUDIO_METER_DETECTOR_FRAMES][2];
} kp_audio_meter_block_t;

typedef struct kp_clap_audio_meter {
    void (*begin)(const void *plugin, uint32_t capture_detector);
    uint32_t (*read)(const void *plugin, kp_audio_meter_block_t *out);
} kp_clap_audio_meter_t;
```

The host queries the static callback table using `get_extension` on the initialized instance.
Both callbacks receive that live instance's `clap_plugin_t` pointer. During active processing,
the host may call `begin` immediately before `process` and `read` immediately after it on the
same symbolic audio thread. Calls cannot overlap either each other or another process call.
Both callbacks must be realtime-safe. The plugin does no detector capture for hosts that
never request it.

A successful `read` returns 1 and writes a finite, nonnegative gain reduction in dB and a count
of complete detector pairs from the preceding block. Return 0 when no reading is available.
Each pair is the maximum detector level in dBFS, then the maximum gain reduction in positive
dB, over its interval. The compressor uses intervals of 32 frames up to 48 kHz and multiples
of 32 frames at higher rates, maintaining roughly 1,500 pairs per second. If the block produces
more pairs than fit, the plugin selects its latest 64 pairs. Earlier entries are oldest in time.
If capture is off or the effect has no detector, the count is zero. The host provides storage
for one whole block and ignores entries beyond the count. It validates the count and levels.
