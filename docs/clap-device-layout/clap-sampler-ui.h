#ifndef KOMPONIST_CLAP_SAMPLER_UI_H
#define KOMPONIST_CLAP_SAMPLER_UI_H

#include <stdint.h>
#include <clap/plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KOMPONIST_CLAP_EXT_SAMPLER_UI "org.klang-studio.komponist.sampler-ui/2"
#define KOMPONIST_CLAP_SAMPLER_UI_MAX_BYTES UINT32_C(32768)

typedef struct komponist_clap_sampler_ui {
    /* Writes one complete bounded editor snapshot and returns the number of bytes used.
     * Zero indicates no snapshot, an invalid buffer or insufficient capacity.
     * A positive finite export_tempo requests preparation at that tempo in BPM.
     * Pass 0.0 for ordinary polling, including during audio processing. Other
     * non-positive or non-finite values also leave the requested tempo unchanged.
     * A tempo request must not overlap audio processing. */
    uint32_t (CLAP_ABI *snapshot)(const clap_plugin_t *plugin,
                                  uint8_t *destination,
                                  uint32_t capacity,
                                  double export_tempo);

    /* Start a held audition at time_seconds in source time. A negative finite value
     * releases the audition. Ignore non-finite input. */
    void (CLAP_ABI *preview)(const clap_plugin_t *plugin, double time_seconds);
} komponist_clap_sampler_ui_t;

#ifdef __cplusplus
}
#endif

#endif /* KOMPONIST_CLAP_SAMPLER_UI_H */
