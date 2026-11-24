#ifndef KOMPONIST_CLAP_SIGNAL_TAPS_H
#define KOMPONIST_CLAP_SIGNAL_TAPS_H

#include <stdint.h>
#include <clap/plugin.h>

#define KOMPONIST_SIGNAL_TAPS_ID "org.klang-studio.komponist.signal-taps/1"
#define KOMPONIST_SIGNAL_TAPS_MAX 4
#define KOMPONIST_TAP_SPECTRUM_SOURCE 1
#define KOMPONIST_TAP_LEVEL_HISTORY 2
#define KOMPONIST_TAP_REDUCTION 3

typedef struct komponist_signal_tap {
    uint8_t name[32];
    uint32_t kind;
    uint32_t audio_frames_per_sample;
    uint32_t reference_rate;
    uint32_t capacity;
} komponist_signal_tap_t;

typedef struct komponist_plugin_signal_taps {
    uint32_t (CLAP_ABI *count)(const clap_plugin_t *plugin);
    uint32_t (CLAP_ABI *info)(const clap_plugin_t *plugin, uint32_t index,
                             komponist_signal_tap_t *out);
    void (CLAP_ABI *begin)(const clap_plugin_t *plugin, uint32_t mask);
    uint32_t (CLAP_ABI *read)(const clap_plugin_t *plugin, uint32_t index,
                             float *out, uint32_t capacity);
} komponist_plugin_signal_taps_t;

#endif
