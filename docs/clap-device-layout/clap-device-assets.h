#ifndef KOMPONIST_CLAP_DEVICE_ASSETS_H
#define KOMPONIST_CLAP_DEVICE_ASSETS_H

#include <stdint.h>
#include <clap/plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KOMPONIST_CLAP_EXT_DEVICE_ASSETS "org.klang-studio.komponist.device-assets/1"
#define KOMPONIST_CLAP_ASSET_MAX_SLOTS UINT32_C(32)
#define KOMPONIST_CLAP_ASSET_MAX_SNAPSHOT_BYTES UINT32_C(16384)
#define KOMPONIST_CLAP_ASSET_MAX_BINDINGS_BYTES UINT32_C(135168)
#define KOMPONIST_CLAP_ASSET_OFFLINE_READY UINT32_C(1)
#define KOMPONIST_CLAP_ASSET_OFFLINE_PREPARING UINT32_C(2)
#define KOMPONIST_CLAP_ASSET_OFFLINE_FAILED UINT32_C(3)

typedef struct komponist_clap_device_assets {
    /* Writes one complete postcard snapshot. Returns bytes written, or 0 on failure.
     * May run during audio processing. */
    uint32_t (CLAP_ABI *snapshot)(const clap_plugin_t *plugin,
                                  uint8_t *destination,
                                  uint32_t capacity);

    /* Applies the complete postcard update atomically. Returns 1 on acceptance and 0
     * on rejection. Actual file loading may continue asynchronously. */
    uint32_t (CLAP_ABI *set_bindings)(const clap_plugin_t *plugin,
                                      const uint8_t *source,
                                      uint32_t length);

    /* Starts or polls request_id at sample_rate Hz and tempo BPM.
     * The host passes all zero arguments to clear the request and resumes polling
     * only for a new request ID. Call only with audio processing detached. */
    uint32_t (CLAP_ABI *prepare_offline)(const clap_plugin_t *plugin,
                                         uint32_t request_id,
                                         double sample_rate,
                                         double tempo);
} komponist_clap_device_assets_t;

#ifdef __cplusplus
}
#endif

#endif /* KOMPONIST_CLAP_DEVICE_ASSETS_H */
