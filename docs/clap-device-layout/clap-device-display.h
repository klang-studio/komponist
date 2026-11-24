#ifndef KOMPONIST_CLAP_DEVICE_DISPLAY_H
#define KOMPONIST_CLAP_DEVICE_DISPLAY_H

#include <stdint.h>
#include <clap/plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KOMPONIST_CLAP_EXT_DEVICE_DISPLAY "org.klang-studio.komponist.device-display/1"
#define KOMPONIST_CLAP_DISPLAY_MAX_BYTES UINT32_C(65536)

typedef struct komponist_clap_device_display {
    /*
     * [main-thread] After successful init and before destroy, including while
     * processing. Copy the current UTF-8 JSON display list to destination.
     * Return bytes written, or zero if no complete display list is available.
     * No NUL terminator. The buffer is valid for this call only.
     */
    uint32_t (CLAP_ABI *draw)(const clap_plugin_t *plugin,
                              uint8_t *destination,
                              uint32_t capacity);
} komponist_clap_device_display_t;

#ifdef __cplusplus
}
#endif

#endif /* KOMPONIST_CLAP_DEVICE_DISPLAY_H */
