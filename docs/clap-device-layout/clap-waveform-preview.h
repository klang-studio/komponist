#ifndef KOMPONIST_CLAP_WAVEFORM_PREVIEW_H
#define KOMPONIST_CLAP_WAVEFORM_PREVIEW_H

#include <clap/plugin.h>

#define KOMPONIST_CLAP_EXT_WAVEFORM_PREVIEW "org.klang-studio.komponist.waveform-preview/1"

typedef struct komponist_clap_waveform_preview {
    /* Main thread. Start at source seconds >= 0. Exactly -1 releases the preview. */
    void (CLAP_ABI *preview)(const clap_plugin_t *plugin, uint32_t asset_slot, double seconds);
} komponist_clap_waveform_preview_t;

#endif
