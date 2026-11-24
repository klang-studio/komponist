#ifndef KOMPONIST_CLAP_DEVICE_LAYOUT_H
#define KOMPONIST_CLAP_DEVICE_LAYOUT_H

#include <stdint.h>
#include <clap/plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Plugin-side extension ID, passed to clap_plugin_t.get_extension. */
#define KOMPONIST_CLAP_EXT_DEVICE_LAYOUT "org.klang-studio.komponist.device-layout/1"
#define KOMPONIST_CLAP_EXT_DEVICE_LAYOUT_V2 "org.klang-studio.komponist.device-layout/2"

/* Maximum UTF-8 JSON payload size in bytes. The payload has no NUL terminator. */
#define KOMPONIST_CLAP_LAYOUT_MAX_BYTES UINT32_C(65536)

#define KOMPONIST_CLAP_LAYOUT_OK UINT32_C(0)
#define KOMPONIST_CLAP_LAYOUT_ABSENT UINT32_C(1)
#define KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL UINT32_C(2)
#define KOMPONIST_CLAP_LAYOUT_ERROR UINT32_C(3)

typedef struct komponist_clap_device_layout {
    /*
     * [main-thread] After successful plugin init and before destroy.
     *
     * plugin is the initialized CLAP instance. required_or_written must point
     * to a writable uint32_t outside the destination buffer. A null output
     * pointer returns ERROR.
     *
     * Query with destination == NULL and capacity == 0. BUFFER_TOO_SMALL
     * reports the required byte count when a layout is available. Then supply
     * a writable buffer of that capacity. OK copies the entire document,
     * reporting bytes written. The document and byte count are constant for
     * the lifetime of the initialized plugin instance.
     *
     * No destination bytes are written on BUFFER_TOO_SMALL, ABSENT or ERROR.
     * ABSENT and ERROR report zero bytes. Neither output pointer is retained.
     */
    uint32_t (CLAP_ABI *get_layout)(const clap_plugin_t *plugin,
                                    uint8_t *destination,
                                    uint32_t capacity,
                                    uint32_t *required_or_written);
} komponist_clap_device_layout_t;

#ifdef __cplusplus
}
#endif

#endif /* KOMPONIST_CLAP_DEVICE_LAYOUT_H */
