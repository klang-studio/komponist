#ifndef KOMPONIST_CLAP_WAVEFORM_MARKERS_H
#define KOMPONIST_CLAP_WAVEFORM_MARKERS_H

#include <stdint.h>
#include <clap/plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KOMPONIST_CLAP_EXT_WAVEFORM_MARKERS "org.klang-studio.komponist.waveform-markers/1"
#define KOMPONIST_CLAP_WAVEFORM_MAX_MARKERS UINT32_C(127)
#define KOMPONIST_CLAP_WAVEFORM_MAX_SNAPSHOT_BYTES UINT32_C(4096)
#define KOMPONIST_CLAP_WAVEFORM_MAX_EDIT_BYTES UINT32_C(32)
#define KOMPONIST_CLAP_WAVEFORM_MAX_FADE_MS UINT32_C(10000)
#define KOMPONIST_CLAP_WAVEFORM_EDIT_INVALID UINT32_C(0)
#define KOMPONIST_CLAP_WAVEFORM_EDIT_APPLIED UINT32_C(1)
#define KOMPONIST_CLAP_WAVEFORM_EDIT_UNCHANGED UINT32_C(2)
#define KOMPONIST_CLAP_WAVEFORM_EDIT_STALE UINT32_C(3)
#define KOMPONIST_CLAP_WAVEFORM_EDIT_UNAVAILABLE UINT32_C(4)

typedef struct komponist_clap_waveform_markers {
    /* Writes one complete postcard snapshot. Returns bytes written, or 0 on failure. */
    uint32_t (CLAP_ABI *snapshot)(const clap_plugin_t *plugin,
                                  uint32_t slot,
                                  uint8_t *destination,
                                  uint32_t capacity);

    /* Applies one bounded postcard edit. Returns a KOMPONIST_CLAP_WAVEFORM_EDIT_* status. */
    uint32_t (CLAP_ABI *edit)(const clap_plugin_t *plugin,
                              uint32_t slot,
                              const uint8_t *source,
                              uint32_t length);
} komponist_clap_waveform_markers_t;

#ifdef __cplusplus
}
#endif

#endif /* KOMPONIST_CLAP_WAVEFORM_MARKERS_H */
