#ifndef KOMPONIST_CLAP_SIDECHAIN_FALLBACK_H
#define KOMPONIST_CLAP_SIDECHAIN_FALLBACK_H

#include <stdint.h>

/* Plugin-side extension ID, passed to clap_plugin_t.get_extension after init. */
#define KOMPONIST_CLAP_EXT_SIDECHAIN_FALLBACK "org.klang-studio.komponist.sidechain-fallback/1"
#define KOMPONIST_CLAP_SIDECHAIN_MAIN_INPUT UINT32_C(1)
#define KOMPONIST_CLAP_SIDECHAIN_CURRENT_CONFIGURATION UINT32_MAX

/* Immutable plugin-owned table, valid until the instance is destroyed. */
typedef struct komponist_clap_sidechain_fallback {
    uint32_t mode; /* KOMPONIST_CLAP_SIDECHAIN_MAIN_INPUT */
    uint32_t auxiliary_config_id; /* Standard CLAP audio ports config ID, or CURRENT_CONFIGURATION. */
} komponist_clap_sidechain_fallback_t;

#endif /* KOMPONIST_CLAP_SIDECHAIN_FALLBACK_H */
