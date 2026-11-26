/* Standalone CLAP layout fixture using only the public C headers. */
#include <clap-device-layout.h>
#include <clap-device-display.h>
#include <clap/entry.h>
#include <clap/ext/audio-ports.h>
#include <clap/ext/params.h>
#include <clap/ext/preset-load.h>
#include <clap/ext/gui.h>
#include <clap/ext/state.h>
#include <clap/ext/state-context.h>
#include <clap/factory/plugin-factory.h>
#include <clap/factory/preset-discovery.h>

#include <math.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fixture-layouts.h"

#define FIXTURE_ID "org.klang-studio.komponist.layout-fixture."

enum variant {
    VALID,
    PRESENTATION,
    MISSING,
    ABSENT,
    INVALID_LENGTH,
    WRONG_COPY_LENGTH,
    OVERSIZE,
    UNKNOWN_PARAM,
    OPTIONAL_FIELD,
    REQUIRED_CAPABILITY,
    GRAPHS,
    INVALID_GRAPHS,
    PROGRAM,
    VARIANT_COUNT
};

/* PROGRAM is only exposed by the program variant. */
enum parameter_index { GAIN, MODE, PROGRAM_PARAM, PARAM_COUNT };

static const char *const features[] = {CLAP_PLUGIN_FEATURE_AUDIO_EFFECT,
                                       CLAP_PLUGIN_FEATURE_STEREO, NULL};

#define DESCRIPTOR(suffix)                                                                         \
    {.clap_version = CLAP_VERSION_INIT,                                                             \
     .id = FIXTURE_ID suffix,                                                                      \
     .name = "Layout Fixture (" suffix ")",                                                        \
     .vendor = "klang-studio",                                                                     \
     .description = "Standalone stereo CLAP fixture.",                                             \
     .version = "1.0.0",                                                                           \
     .features = features}

static const clap_plugin_descriptor_t descriptors[VARIANT_COUNT] = {
    [VALID] = DESCRIPTOR("valid"),
    [PRESENTATION] = DESCRIPTOR("presentation"),
    [MISSING] = DESCRIPTOR("missing"),
    [ABSENT] = DESCRIPTOR("absent"),
    [INVALID_LENGTH] = DESCRIPTOR("invalid-length"),
    [WRONG_COPY_LENGTH] = DESCRIPTOR("wrong-copy-length"),
    [OVERSIZE] = DESCRIPTOR("oversize"),
    [UNKNOWN_PARAM] = DESCRIPTOR("unknown-param"),
    [OPTIONAL_FIELD] = DESCRIPTOR("optional-field"),
    [REQUIRED_CAPABILITY] = DESCRIPTOR("required-capability"),
    [GRAPHS] = DESCRIPTOR("graphs"),
    [INVALID_GRAPHS] = DESCRIPTOR("invalid-graphs"),
    [PROGRAM] = DESCRIPTOR("program"),
};

static const clap_param_info_t parameter_info[PARAM_COUNT] = {
    [GAIN] = {.id = 42,
              .flags = CLAP_PARAM_IS_AUTOMATABLE,
              .name = "Gain",
              .min_value = 0,
              .max_value = 1,
              .default_value = 0.5},
    [MODE] = {.id = 7,
              .flags = CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM,
              .name = "Mode",
              .min_value = 0,
              .max_value = 2,
              .default_value = 0},
    [PROGRAM_PARAM] = {.id = 9,
                       .flags = CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM,
                       .name = "Program",
                       .min_value = 0,
                       .max_value = 3,
                       .default_value = 0},
};

static const char *const mode_labels[] = {"Clean", "Warm", "Drive"};

/* The program variant loads these presets itself when its Program parameter changes and reports
 * them through the host side of CLAP preset-load. The last key has no preset: it reports an error. */
static const char *const program_labels[] = {"None", "Boost", "Calm", "Missing"};
static const char *const program_keys[] = {NULL, "fixture-boost", "fixture-calm", "fixture-missing"};

struct instance {
    clap_plugin_t plugin;
    enum variant variant;
    _Atomic double values[PARAM_COUNT];
    _Atomic float rendered_gain;
    bool hide_mode;
    atomic_int pending_program;
    const clap_host_t *host;
};

static struct instance *instance(const clap_plugin_t *plugin) {
    return plugin->plugin_data;
}

static uint32_t CLAP_ABI param_count(const clap_plugin_t *plugin) {
    return instance(plugin)->variant == PROGRAM ? PARAM_COUNT : PROGRAM_PARAM;
}

static int parameter_index(const clap_plugin_t *plugin, clap_id id) {
    for (uint32_t i = 0; i < param_count(plugin); ++i) {
        if (parameter_info[i].id == id)
            return (int)i;
    }
    return -1;
}

static bool CLAP_ABI param_info(const clap_plugin_t *plugin, uint32_t index, clap_param_info_t *out) {
    if (index >= param_count(plugin) || !out)
        return false;
    *out = parameter_info[index];
    if (index == MODE && instance(plugin)->hide_mode)
        out->flags |= CLAP_PARAM_IS_HIDDEN;
    return true;
}

static bool CLAP_ABI param_value(const clap_plugin_t *plugin, clap_id id, double *out) {
    const int index = parameter_index(plugin, id);
    if (index < 0 || !out)
        return false;
    *out = instance(plugin)->values[index];
    return true;
}

static bool CLAP_ABI param_to_text(const clap_plugin_t *plugin, clap_id id, double value,
                                   char *out, uint32_t capacity) {
    const int index = parameter_index(plugin, id);
    if (index < 0 || !out || capacity == 0 || !isfinite(value) ||
        value < parameter_info[index].min_value || value > parameter_info[index].max_value)
        return false;
    const char *label = NULL;
    if (index == MODE)
        label = mode_labels[(int)value];
    else if (index == PROGRAM_PARAM)
        label = program_labels[(int)value];
    else if (instance(plugin)->variant == PRESENTATION && value == 0.625)
        label = "Focus";
    const int size = label ? snprintf(out, capacity, "%s", label)
                           : snprintf(out, capacity, "%.3f", value);
    return size >= 0 && (uint32_t)size < capacity;
}

static bool CLAP_ABI text_to_param(const clap_plugin_t *plugin, clap_id id, const char *text,
                                   double *out) {
    const int index = parameter_index(plugin, id);
    if (index < 0 || !text || !out)
        return false;
    if (index == MODE || index == PROGRAM_PARAM) {
        const char *const *labels = index == MODE ? mode_labels : program_labels;
        const size_t count = index == MODE ? sizeof(mode_labels) / sizeof(*mode_labels)
                                           : sizeof(program_labels) / sizeof(*program_labels);
        for (size_t i = 0; i < count; ++i) {
            if (strcmp(text, labels[i]) == 0) {
                *out = (double)i;
                return true;
            }
        }
        return false;
    }
    if (instance(plugin)->variant == PRESENTATION && strcmp(text, "Focus") == 0) {
        *out = 0.625;
        return true;
    }
    char *end;
    const double value = strtod(text, &end);
    if (text == end || *end != '\0' || !isfinite(value) ||
        value < parameter_info[index].min_value || value > parameter_info[index].max_value)
        return false;
    *out = value;
    return true;
}

static void CLAP_ABI flush_params(const clap_plugin_t *plugin, const clap_input_events_t *input,
                                  const clap_output_events_t *output) {
    (void)output;
    if (!input)
        return;
    for (uint32_t i = 0, count = input->size(input); i < count; ++i) {
        const clap_event_header_t *header = input->get(input, i);
        if (!header || header->space_id != CLAP_CORE_EVENT_SPACE_ID ||
            header->type != CLAP_EVENT_PARAM_VALUE || header->size < sizeof(clap_event_param_value_t))
            continue;
        const clap_event_param_value_t *change = (const clap_event_param_value_t *)header;
        const int index = parameter_index(plugin, change->param_id);
        if (index < 0 || !isfinite(change->value) ||
            change->value < parameter_info[index].min_value ||
            change->value > parameter_info[index].max_value)
            continue;
        struct instance *self = instance(plugin);
        if (index == PROGRAM_PARAM) {
            /* Any thread: the preset is loaded on the main thread. */
            self->values[index] = (int)change->value;
            if ((int)change->value != 0) {
                atomic_store(&self->pending_program, (int)change->value);
                self->host->request_callback(self->host);
            }
            continue;
        }
        self->values[index] = index == MODE ? (int)change->value : change->value;
    }
}

static const clap_plugin_params_t params = {
    .count = param_count,
    .get_info = param_info,
    .get_value = param_value,
    .value_to_text = param_to_text,
    .text_to_value = text_to_param,
    .flush = flush_params,
};

static uint32_t CLAP_ABI port_count(const clap_plugin_t *plugin, bool is_input) {
    (void)plugin;
    (void)is_input;
    return 1;
}

static bool CLAP_ABI port_info(const clap_plugin_t *plugin, uint32_t index, bool is_input,
                               clap_audio_port_info_t *out) {
    (void)plugin;
    if (index != 0 || !out)
        return false;
    *out = (clap_audio_port_info_t){.id = is_input ? 0 : 1,
                                   .flags = CLAP_AUDIO_PORT_IS_MAIN,
                                   .channel_count = 2,
                                   .port_type = CLAP_PORT_STEREO,
                                   .in_place_pair = is_input ? 1 : 0};
    snprintf(out->name, sizeof(out->name), "%s", is_input ? "Stereo In" : "Stereo Out");
    return true;
}

static const clap_plugin_audio_ports_t audio_ports = {.count = port_count, .get = port_info};

static uint32_t CLAP_ABI get_layout(const clap_plugin_t *plugin, uint8_t *buffer,
                                    uint32_t capacity, uint32_t *written) {
    if (!written)
        return KOMPONIST_CLAP_LAYOUT_ERROR;
    *written = 0;
    const uint8_t *payload = control_layout;
    uint32_t size = sizeof(control_layout);
    switch (instance(plugin)->variant) {
    case PRESENTATION:
        payload = presentation_layout;
        size = sizeof(presentation_layout);
        break;
    case ABSENT:
        return KOMPONIST_CLAP_LAYOUT_ABSENT;
    case INVALID_LENGTH:
        return KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL;
    case OVERSIZE:
        *written = KOMPONIST_CLAP_LAYOUT_MAX_BYTES + 1;
        return KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL;
    case UNKNOWN_PARAM:
        payload = unknown_param_layout;
        size = sizeof(unknown_param_layout);
        break;
    case OPTIONAL_FIELD:
        payload = optional_field_layout;
        size = sizeof(optional_field_layout);
        break;
    case GRAPHS:
        payload = graph_layout;
        size = sizeof(graph_layout);
        break;
    case INVALID_GRAPHS:
        payload = invalid_graph_layout;
        size = sizeof(invalid_graph_layout);
        break;
    case REQUIRED_CAPABILITY:
        payload = required_capability_layout;
        size = sizeof(required_capability_layout);
        break;
    default:
        break;
    }
    if (!buffer && capacity != 0)
        return KOMPONIST_CLAP_LAYOUT_ERROR;
    *written = size;
    if (capacity < size)
        return KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL;
    memcpy(buffer, payload, size);
    if (instance(plugin)->variant == WRONG_COPY_LENGTH)
        --*written;
    return KOMPONIST_CLAP_LAYOUT_OK;
}

static const komponist_clap_device_layout_t layout_extension = {.get_layout = get_layout};

static uint32_t CLAP_ABI draw(const clap_plugin_t *plugin, uint8_t *buffer,
                             uint32_t capacity) {
    if (!buffer || !capacity)
        return 0;
    const unsigned gain = (unsigned)(atomic_load(&instance(plugin)->rendered_gain) * 1000.0f);
    const int written = snprintf((char *)buffer, capacity,
        "{\"frames\":[{\"key\":\"scope\",\"primitives\":["
        "{\"kind\":\"line\",\"from\":[0,0.75],\"to\":[1,%u.%03u],\"tone\":\"accent\"}]}]}",
        gain / 1000, gain % 1000);
    return written > 0 && (uint32_t)written < capacity ? (uint32_t)written : 0;
}

static const komponist_clap_device_display_t display_extension = {.draw = draw};

static bool CLAP_ABI plugin_init(const clap_plugin_t *plugin) {
    (void)plugin;
    return true;
}

static void CLAP_ABI plugin_destroy(const clap_plugin_t *plugin) {
    free(instance(plugin));
}

static bool CLAP_ABI plugin_activate(const clap_plugin_t *plugin, double rate,
                                    uint32_t min_frames, uint32_t max_frames) {
    (void)plugin;
    return isfinite(rate) && rate > 0 && min_frames > 0 && min_frames <= max_frames;
}

static void CLAP_ABI no_op(const clap_plugin_t *plugin) {
    (void)plugin;
}

static clap_process_status CLAP_ABI process(const clap_plugin_t *plugin, const clap_process_t *p) {
    flush_params(plugin, p->in_events, p->out_events);
    const float gain = (float)instance(plugin)->values[GAIN];
    atomic_store(&instance(plugin)->rendered_gain, gain);
    if (p->audio_inputs_count != 1 || p->audio_outputs_count != 1 ||
        p->audio_inputs[0].channel_count != 2 || p->audio_outputs[0].channel_count != 2 ||
        !p->audio_inputs[0].data32 || !p->audio_outputs[0].data32)
        return CLAP_PROCESS_ERROR;
    for (uint32_t channel = 0; channel < 2; ++channel) {
        const float *input = p->audio_inputs[0].data32[channel];
        float *output = p->audio_outputs[0].data32[channel];
        for (uint32_t frame = 0; frame < p->frames_count; ++frame)
            output[frame] = input[frame] * gain;
    }
    p->audio_outputs[0].constant_mask = p->audio_inputs[0].constant_mask;
    return CLAP_PROCESS_CONTINUE;
}

/* The persisted inventory flag lets host tests exercise a rescan on state restore. */
static bool CLAP_ABI save(const clap_plugin_t *plugin, const clap_ostream_t *stream) {
    const struct instance *self = instance(plugin);
    const double values[] = {self->values[GAIN], self->values[MODE], self->hide_mode ? 1 : 0};
    const uint8_t *bytes = (const uint8_t *)values;
    for (size_t offset = 0; offset < sizeof(values);) {
        const int64_t count = stream->write(stream, bytes + offset, sizeof(values) - offset);
        if (count <= 0 || (uint64_t)count > sizeof(values) - offset)
            return false;
        offset += (size_t)count;
    }
    return true;
}

static bool CLAP_ABI restore(const clap_plugin_t *plugin, const clap_istream_t *stream) {
    /* Gain, mode and the inventory flag: the program is not part of the state. */
    enum { HIDE = PROGRAM_PARAM };
    double values[HIDE + 1];
    uint8_t *bytes = (uint8_t *)values;
    for (size_t offset = 0; offset < sizeof(values);) {
        const int64_t count = stream->read(stream, bytes + offset, sizeof(values) - offset);
        if (count <= 0 || (uint64_t)count > sizeof(values) - offset)
            return false;
        offset += (size_t)count;
    }
    if (!isfinite(values[GAIN]) || values[GAIN] < 0 || values[GAIN] > 1 ||
        !isfinite(values[MODE]) || values[MODE] < 0 || values[MODE] > 2 ||
        (values[HIDE] != 0 && values[HIDE] != 1))
        return false;
    struct instance *self = instance(plugin);
    self->values[GAIN] = values[GAIN];
    self->values[MODE] = (int)values[MODE];
    const bool hide = values[HIDE] != 0;
    if (self->hide_mode != hide) {
        self->hide_mode = hide;
        const clap_host_params_t *host_params =
            self->host->get_extension(self->host, CLAP_EXT_PARAMS);
        if (host_params)
            host_params->rescan(self->host, CLAP_PARAM_RESCAN_INFO);
    }
    return true;
}

static const clap_plugin_state_t state_extension = {.save = save, .load = restore};

static bool CLAP_ABI save_context(const clap_plugin_t *plugin, const clap_ostream_t *out,
                                  uint32_t context) {
    if (context != CLAP_STATE_CONTEXT_FOR_PRESET)
        return false;
    return save(plugin, out);
}

static bool CLAP_ABI load_context(const clap_plugin_t *plugin, const clap_istream_t *in,
                                  uint32_t context) {
    if (context != CLAP_STATE_CONTEXT_FOR_PRESET)
        return false;
    return restore(plugin, in);
}

static const clap_plugin_state_context_t state_context = {.save = save_context,
                                                         .load = load_context};

/* Applies an embedded preset; false for an unknown key. */
static bool apply_preset(struct instance *self, const char *key) {
    if (strcmp(key, "fixture-boost") == 0) {
        self->values[GAIN] = 0.75;
        self->values[MODE] = 2.0;
    } else if (self->variant == PROGRAM && strcmp(key, "fixture-calm") == 0) {
        self->values[GAIN] = 0.25;
        self->values[MODE] = 1.0;
    } else {
        return false;
    }
    const clap_host_params_t *params = self->host->get_extension(self->host, CLAP_EXT_PARAMS);
    if (params)
        params->rescan(self->host, CLAP_PARAM_RESCAN_VALUES);
    return true;
}

static bool CLAP_ABI load_factory_preset(const clap_plugin_t *plugin, uint32_t kind,
                                       const char *location, const char *key) {
    if (kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || location || !key)
        return false;
    return apply_preset(instance(plugin), key);
}

/* A Program change loads the preset the plug-in way: on the main thread, reported to the host. */
static void CLAP_ABI plugin_on_main_thread(const clap_plugin_t *plugin) {
    struct instance *self = instance(plugin);
    const int program = atomic_exchange(&self->pending_program, 0);
    if (program <= 0 || program >= (int)(sizeof(program_keys) / sizeof(*program_keys)))
        return;
    const char *key = program_keys[program];
    const clap_host_preset_load_t *host_presets =
        self->host->get_extension(self->host, CLAP_EXT_PRESET_LOAD);
    if (!apply_preset(self, key)) {
        if (host_presets && host_presets->on_error)
            host_presets->on_error(self->host, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, NULL, key, 0,
                                   "unknown fixture preset");
        return;
    }
    if (host_presets && host_presets->loaded)
        host_presets->loaded(self->host, CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, NULL, key);
    const clap_host_state_t *host_state = self->host->get_extension(self->host, CLAP_EXT_STATE);
    if (host_state)
        host_state->mark_dirty(self->host);
}

static const clap_plugin_preset_load_t preset_load = {.from_location = load_factory_preset};

/* Simulate an advertised embedded editor that fails creation. */
static bool CLAP_ABI gui_supported(const clap_plugin_t *plugin, const char *api, bool floating) {
    (void)plugin;
    return !floating && (strcmp(api, CLAP_WINDOW_API_X11) == 0 ||
                         strcmp(api, CLAP_WINDOW_API_COCOA) == 0 ||
                         strcmp(api, CLAP_WINDOW_API_WIN32) == 0);
}

static bool CLAP_ABI gui_create(const clap_plugin_t *plugin, const char *api, bool floating) {
    (void)plugin;
    (void)api;
    (void)floating;
    return false;
}

static const clap_plugin_gui_t failing_gui = {.is_api_supported = gui_supported, .create = gui_create};

static const void *CLAP_ABI get_extension(const clap_plugin_t *plugin, const char *id) {
    if (strcmp(id, CLAP_EXT_PARAMS) == 0)
        return &params;
    if (strcmp(id, CLAP_EXT_AUDIO_PORTS) == 0)
        return &audio_ports;
    if (strcmp(id, CLAP_EXT_STATE) == 0)
        return &state_extension;
    const bool presets = instance(plugin)->variant == VALID || instance(plugin)->variant == PROGRAM;
    if (presets && strcmp(id, CLAP_EXT_STATE_CONTEXT) == 0)
        return &state_context;
    if (presets &&
        (strcmp(id, CLAP_EXT_PRESET_LOAD) == 0 || strcmp(id, CLAP_EXT_PRESET_LOAD_COMPAT) == 0))
        return &preset_load;
    if (instance(plugin)->variant == OPTIONAL_FIELD && strcmp(id, CLAP_EXT_GUI) == 0)
        return &failing_gui;
    const enum variant variant = instance(plugin)->variant;
    if (variant == MISSING)
        return NULL;
    if (variant == GRAPHS && strcmp(id, KOMPONIST_CLAP_EXT_DEVICE_DISPLAY) == 0)
        return &display_extension;
    return strcmp(id, KOMPONIST_CLAP_EXT_DEVICE_LAYOUT) == 0 ? &layout_extension : NULL;
}

static uint32_t CLAP_ABI plugin_count(const clap_plugin_factory_t *factory) {
    (void)factory;
    return VARIANT_COUNT;
}

static const clap_plugin_descriptor_t *CLAP_ABI get_descriptor(const clap_plugin_factory_t *factory,
                                                              uint32_t index) {
    (void)factory;
    return index < VARIANT_COUNT ? &descriptors[index] : NULL;
}

static const clap_plugin_t *CLAP_ABI create_plugin(const clap_plugin_factory_t *factory,
                                                 const clap_host_t *host, const char *id) {
    (void)factory;
    if (!host || !clap_version_is_compatible(host->clap_version))
        return NULL;
    for (int i = 0; i < VARIANT_COUNT; ++i) {
        if (strcmp(id, descriptors[i].id) != 0)
            continue;
        struct instance *state = calloc(1, sizeof(*state));
        if (!state)
            return NULL;
        state->variant = (enum variant)i;
        state->host = host;
        for (int j = 0; j < PARAM_COUNT; ++j)
            atomic_init(&state->values[j], parameter_info[j].default_value);
        atomic_init(&state->rendered_gain, (float)parameter_info[GAIN].default_value);
        atomic_init(&state->pending_program, 0);
        state->plugin = (clap_plugin_t){.desc = &descriptors[i],
                                       .plugin_data = state,
                                       .init = plugin_init,
                                       .destroy = plugin_destroy,
                                       .activate = plugin_activate,
                                       .deactivate = no_op,
                                       .start_processing = plugin_init,
                                       .stop_processing = no_op,
                                       .reset = no_op,
                                       .process = process,
                                       .get_extension = get_extension,
                                       .on_main_thread = plugin_on_main_thread};
        return &state->plugin;
    }
    return NULL;
}

static const clap_plugin_factory_t plugin_factory = {.get_plugin_count = plugin_count,
                                                     .get_plugin_descriptor = get_descriptor,
                                                     .create_plugin = create_plugin};

static const clap_preset_discovery_provider_descriptor_t preset_descriptor = {
    .clap_version = CLAP_VERSION_INIT, .id = FIXTURE_ID "presets",
    .name = "Fixture presets", .vendor = "klang-studio",
};

static bool CLAP_ABI preset_provider_init(const clap_preset_discovery_provider_t *provider) {
    const clap_preset_discovery_indexer_t *indexer = provider->provider_data;
    const clap_preset_discovery_location_t embedded = {
        .flags = CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
        .name = "Factory", .kind = CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN, .location = NULL,
    };
    return indexer->declare_location(indexer, &embedded);
}

static void CLAP_ABI preset_provider_destroy(const clap_preset_discovery_provider_t *provider) {
    free((void *)provider);
}

static bool CLAP_ABI preset_provider_metadata(const clap_preset_discovery_provider_t *provider,
                                              uint32_t kind, const char *location,
                                              const clap_preset_discovery_metadata_receiver_t *receiver) {
    (void)provider;
    if (kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || location ||
        !receiver->begin_preset(receiver, "Fixture Boost", "fixture-boost"))
        return false;
    const clap_universal_plugin_id_t valid = {.abi = "clap", .id = FIXTURE_ID "valid"};
    const clap_universal_plugin_id_t program = {.abi = "clap", .id = FIXTURE_ID "program"};
    receiver->add_plugin_id(receiver, &valid);
    receiver->add_plugin_id(receiver, &program);
    receiver->add_extra_info(receiver, "org.klang-studio.komponist.preset-name.de", "Testanhebung");
    if (!receiver->begin_preset(receiver, "Fixture Calm", "fixture-calm"))
        return false;
    receiver->add_plugin_id(receiver, &program);
    receiver->add_extra_info(receiver, "org.klang-studio.komponist.preset-name.de", "Testruhe");
    return true;
}

static uint32_t CLAP_ABI preset_count(const clap_preset_discovery_factory_t *factory) {
    (void)factory;
    return 1;
}

static const clap_preset_discovery_provider_descriptor_t *CLAP_ABI
preset_provider_descriptor(const clap_preset_discovery_factory_t *factory, uint32_t index) {
    (void)factory;
    return index == 0 ? &preset_descriptor : NULL;
}

static const clap_preset_discovery_provider_t *CLAP_ABI
create_preset_provider(const clap_preset_discovery_factory_t *factory,
                       const clap_preset_discovery_indexer_t *indexer, const char *id) {
    (void)factory;
    if (strcmp(id, preset_descriptor.id) != 0)
        return NULL;
    clap_preset_discovery_provider_t *provider = malloc(sizeof(*provider));
    if (!provider)
        return NULL;
    *provider = (clap_preset_discovery_provider_t){
        .desc = &preset_descriptor, .provider_data = (void *)indexer,
        .init = preset_provider_init, .destroy = preset_provider_destroy,
        .get_metadata = preset_provider_metadata,
    };
    return provider;
}

static const clap_preset_discovery_factory_t preset_factory = {
    .count = preset_count, .get_descriptor = preset_provider_descriptor, .create = create_preset_provider,
};

static bool CLAP_ABI entry_init(const char *path) {
    (void)path;
    return true;
}

static void CLAP_ABI entry_deinit(void) {}

static const void *CLAP_ABI get_factory(const char *id) {
    if (strcmp(id, CLAP_PLUGIN_FACTORY_ID) == 0)
        return &plugin_factory;
    if (strcmp(id, CLAP_PRESET_DISCOVERY_FACTORY_ID) == 0 ||
        strcmp(id, CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT) == 0)
        return &preset_factory;
    return NULL;
}

CLAP_EXPORT const clap_plugin_entry_t clap_entry = {.clap_version = CLAP_VERSION_INIT,
                                                   .init = entry_init,
                                                   .deinit = entry_deinit,
                                                   .get_factory = get_factory};
