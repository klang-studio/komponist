/* A main-thread CLAP host for checking the published C ABI across a shared-library boundary. */
#include <clap-device-layout.h>
#include <clap/entry.h>
#include <clap/ext/params.h>
#include <clap/factory/plugin-factory.h>

#include <inttypes.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#include <windows.h>
typedef HMODULE module_handle;
#else
#include <dlfcn.h>
typedef void *module_handle;
#endif

#define FIXTURE_ID "org.klang-studio.komponist.layout-fixture."

struct module {
    module_handle handle;
    const clap_plugin_entry_t *entry;
    const clap_plugin_factory_t *factory;
};

struct layout_response {
    uint32_t status;
    uint32_t size;
    uint8_t *payload;
};

static void CLAP_ABI request_no_op(const clap_host_t *host) {
    (void)host;
}

static const void *CLAP_ABI host_extension(const clap_host_t *host, const char *id) {
    (void)host;
    (void)id;
    return NULL;
}

static const clap_host_t host = {
    .clap_version = CLAP_VERSION_INIT,
    .name = "C Layout Probe",
    .vendor = "klang-studio",
    .url = "",
    .version = "1.0.0",
    .get_extension = host_extension,
    .request_restart = request_no_op,
    .request_process = request_no_op,
    .request_callback = request_no_op,
};

static module_handle open_library(const char *path) {
#ifdef _WIN32
    return LoadLibraryA(path);
#else
    return dlopen(path, RTLD_NOW | RTLD_LOCAL);
#endif
}

static void close_library(module_handle module) {
#ifdef _WIN32
    FreeLibrary(module);
#else
    dlclose(module);
#endif
}

static const clap_plugin_entry_t *load_entry(module_handle module) {
#ifdef _WIN32
    const FARPROC symbol = GetProcAddress(module, "clap_entry");
    const clap_plugin_entry_t *entry;
    /* The Windows loader returns a pointer-sized address for exported data symbols as well. */
    _Static_assert(sizeof(symbol) == sizeof(entry), "unexpected Windows symbol pointer size");
    memcpy(&entry, &symbol, sizeof(entry));
    return entry;
#else
    return dlsym(module, "clap_entry");
#endif
}

static bool open_module(const char *path, struct module *module) {
    module->handle = open_library(path);
    if (!module->handle) {
#ifdef _WIN32
        fprintf(stderr, "LoadLibrary failed for %s (Windows error %lu)\n", path, GetLastError());
#else
        fprintf(stderr, "dlopen failed for %s: %s\n", path, dlerror());
#endif
        return false;
    }
    module->entry = load_entry(module->handle);
    if (!module->entry || !clap_version_is_compatible(module->entry->clap_version) ||
        !module->entry->init(path)) {
        fprintf(stderr, "could not initialize the CLAP entry in %s\n", path);
        close_library(module->handle);
        return false;
    }
    module->factory = module->entry->get_factory(CLAP_PLUGIN_FACTORY_ID);
    if (!module->factory) {
        fprintf(stderr, "CLAP plugin factory missing in %s\n", path);
        module->entry->deinit();
        close_library(module->handle);
        return false;
    }
    return true;
}

static const clap_plugin_t *instantiate(const struct module *module, const char *id) {
    const clap_plugin_t *plugin = module->factory->create_plugin(module->factory, &host, id);
    if (!plugin) {
        fprintf(stderr, "CLAP plugin not found: %s\n", id);
        return NULL;
    }
    if (!plugin->init(plugin)) {
        fprintf(stderr, "CLAP plugin init failed: %s\n", id);
        plugin->destroy(plugin);
        return NULL;
    }
    return plugin;
}

static struct layout_response read_layout(const clap_plugin_t *plugin,
                                          const char *extension_id) {
    struct layout_response result = {.status = KOMPONIST_CLAP_LAYOUT_ABSENT};
    const komponist_clap_device_layout_t *extension =
        plugin->get_extension(plugin, extension_id);
    if (!extension)
        return result;
    result.status = KOMPONIST_CLAP_LAYOUT_ERROR;
    if (!extension->get_layout)
        return result;

    uint32_t required = 0;
    const uint32_t status = extension->get_layout(plugin, NULL, 0, &required);
    result.size = required;
    if (status == KOMPONIST_CLAP_LAYOUT_ABSENT && required == 0) {
        result.status = KOMPONIST_CLAP_LAYOUT_ABSENT;
        return result;
    }
    if (status != KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL || required == 0 ||
        required > KOMPONIST_CLAP_LAYOUT_MAX_BYTES)
        return result;

    uint8_t *buffer = malloc(required);
    if (!buffer)
        return result;
    uint32_t written = 0;
    const uint32_t copy_status = extension->get_layout(plugin, buffer, required, &written);
    result.size = written;
    if (copy_status != KOMPONIST_CLAP_LAYOUT_OK || written != required) {
        free(buffer);
        return result;
    }
    result.status = KOMPONIST_CLAP_LAYOUT_OK;
    result.payload = buffer;
    return result;
}

static void json_name(const char name[CLAP_NAME_SIZE]) {
    putchar('"');
    for (size_t i = 0; i < CLAP_NAME_SIZE && name[i] != '\0'; ++i) {
        const unsigned char byte = (unsigned char)name[i];
        if (byte == '\\' || byte == '"')
            printf("\\%c", byte);
        else if (byte < 32)
            printf("\\u%04x", (unsigned)byte);
        else
            putchar(byte);
    }
    putchar('"');
}

static bool print_probe(const clap_plugin_t *plugin) {
    const clap_plugin_params_t *params = plugin->get_extension(plugin, CLAP_EXT_PARAMS);
    fputs("{\"params\":[", stdout);
    const uint32_t count = params ? params->count(plugin) : 0;
    for (uint32_t index = 0; index < count; ++index) {
        clap_param_info_t info;
        if (!params->get_info(plugin, index, &info) || !isfinite(info.min_value) ||
            !isfinite(info.max_value) || !isfinite(info.default_value)) {
            fprintf(stderr, "malformed CLAP parameter metadata at index %" PRIu32 "\n", index);
            return false;
        }
        printf("%s{\"id\":%" PRIu32 ",\"flags\":%" PRIu32 ",\"name\":",
               index == 0 ? "" : ",", info.id, info.flags);
        json_name(info.name);
        printf(",\"min\":%.17g,\"max\":%.17g,\"default\":%.17g}",
               info.min_value, info.max_value, info.default_value);
    }
    struct layout_response layout = read_layout(plugin, KOMPONIST_CLAP_EXT_DEVICE_LAYOUT);
    printf("],\"status\":%" PRIu32 ",\"size\":%" PRIu32 ",\"layout\":", layout.status, layout.size);
    const bool ok = layout.payload ? fwrite(layout.payload, 1, layout.size, stdout) == layout.size
                                   : fputs("null", stdout) != EOF;
    free(layout.payload);
    return ok && puts("}") != EOF;
}

static bool check_params(const clap_plugin_t *plugin) {
    const clap_plugin_params_t *params = plugin->get_extension(plugin, CLAP_EXT_PARAMS);
    if (!params || params->count(plugin) != 2)
        return false;
    clap_param_info_t gain;
    clap_param_info_t mode;
    if (!params->get_info(plugin, 0, &gain) || !params->get_info(plugin, 1, &mode) ||
        gain.id != 42 || mode.id != 7 || strcmp(gain.name, "Gain") != 0 ||
        strcmp(mode.name, "Mode") != 0 || gain.min_value != 0 || gain.max_value != 1 ||
        mode.min_value != 0 || mode.max_value != 2 ||
        (gain.flags & (CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM)) != 0 ||
        (mode.flags & (CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM)) !=
            (CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM))
        return false;
    double value;
    if (!params->get_value(plugin, gain.id, &value) || value != gain.default_value ||
        !params->get_value(plugin, mode.id, &value) || value != mode.default_value)
        return false;
    char label[32];
    for (int i = 0; i <= 2; ++i) {
        if (!params->value_to_text(plugin, mode.id, i, label, sizeof(label)) ||
            !params->text_to_value(plugin, mode.id, label, &value) || value != i)
            return false;
    }
    return true;
}

static bool check_copy_contract(const clap_plugin_t *plugin, const char *extension_id,
                                const struct layout_response *layout) {
    const komponist_clap_device_layout_t *extension =
        plugin->get_extension(plugin, extension_id);
    if (!extension)
        return false;
    uint32_t reported = 0;
    if (extension->get_layout(plugin, NULL, 0, &reported) != KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL ||
        reported != layout->size ||
        extension->get_layout(plugin, NULL, 0, NULL) != KOMPONIST_CLAP_LAYOUT_ERROR)
        return false;
    uint8_t *buffer = malloc((size_t)layout->size + 1);
    if (!buffer)
        return false;
    memset(buffer, 0xab, (size_t)layout->size + 1);
    bool ok =
        extension->get_layout(plugin, buffer, layout->size - 1, &reported) ==
            KOMPONIST_CLAP_LAYOUT_BUFFER_TOO_SMALL &&
        reported == layout->size;
    for (uint32_t i = 0; ok && i <= layout->size; ++i)
        ok = buffer[i] == 0xab;
    ok = ok &&
         extension->get_layout(plugin, buffer, layout->size + 1, &reported) ==
             KOMPONIST_CLAP_LAYOUT_OK &&
         reported == layout->size && memcmp(layout->payload, buffer, layout->size) == 0 &&
         buffer[layout->size] == 0xab;
    free(buffer);
    return ok;
}

static bool self_test(const struct module *module) {
    static const struct {
        const char *id;
        uint32_t status;
    } cases[] = {
        {FIXTURE_ID "valid", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "presentation", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "missing", KOMPONIST_CLAP_LAYOUT_ABSENT},
        {FIXTURE_ID "absent", KOMPONIST_CLAP_LAYOUT_ABSENT},
        {FIXTURE_ID "invalid-length", KOMPONIST_CLAP_LAYOUT_ERROR},
        {FIXTURE_ID "wrong-copy-length", KOMPONIST_CLAP_LAYOUT_ERROR},
        {FIXTURE_ID "oversize", KOMPONIST_CLAP_LAYOUT_ERROR},
        {FIXTURE_ID "unknown-param", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "optional-field", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "required-capability", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "graphs", KOMPONIST_CLAP_LAYOUT_OK},
        {FIXTURE_ID "invalid-graphs", KOMPONIST_CLAP_LAYOUT_OK},
    };
    const clap_plugin_factory_t *factory = module->factory;
    if (factory->get_plugin_count(factory) != sizeof(cases) / sizeof(*cases)) {
        fputs("wrong CLAP factory descriptor count\n", stderr);
        return false;
    }
    for (uint32_t index = 0; index < sizeof(cases) / sizeof(*cases); ++index) {
        const clap_plugin_descriptor_t *desc = factory->get_plugin_descriptor(factory, index);
        if (!desc || strcmp(desc->id, cases[index].id) != 0) {
            fprintf(stderr, "missing fixture descriptor %s\n", cases[index].id);
            return false;
        }
        const clap_plugin_t *plugin = instantiate(module, desc->id);
        if (!plugin)
            return false;

        const char *extension_id = KOMPONIST_CLAP_EXT_DEVICE_LAYOUT;
        struct layout_response layout = read_layout(plugin, extension_id);
        bool ok = check_params(plugin) && layout.status == cases[index].status;
        if (strcmp(desc->id, FIXTURE_ID "oversize") == 0)
            ok = ok && layout.size > KOMPONIST_CLAP_LAYOUT_MAX_BYTES;
        if (layout.status == KOMPONIST_CLAP_LAYOUT_OK)
            ok = ok && check_copy_contract(plugin, extension_id, &layout);
        free(layout.payload);
        plugin->destroy(plugin);
        if (!ok) {
            fprintf(stderr, "C ABI check failed for %s (status %" PRIu32 ", size %" PRIu32 ")\n",
                    desc->id, layout.status, layout.size);
            return false;
        }
    }
    printf("C CLAP layout ABI passed for %zu fixture variants\n", sizeof(cases) / sizeof(*cases));
    return true;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s PLUGIN_PATH PLUGIN_ID | --self-test PLUGIN_PATH\n", argv[0]);
        return EXIT_FAILURE;
    }
#ifdef _WIN32
    if (_setmode(_fileno(stdout), _O_BINARY) == -1) {
        perror("cannot set stdout to binary mode");
        return EXIT_FAILURE;
    }
#endif
    const bool fixture_test = strcmp(argv[1], "--self-test") == 0;
    const char *path = fixture_test ? argv[2] : argv[1];
    struct module module;
    if (!open_module(path, &module))
        return EXIT_FAILURE;

    bool ok = false;
    if (fixture_test) {
        ok = self_test(&module);
    } else {
        const clap_plugin_t *plugin = instantiate(&module, argv[2]);
        if (plugin) {
            ok = print_probe(plugin);
            plugin->destroy(plugin);
        }
    }
    module.entry->deinit();
    close_library(module.handle);
    return ok ? EXIT_SUCCESS : EXIT_FAILURE;
}
