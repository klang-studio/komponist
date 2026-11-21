use clack_extensions::audio_ports_config::{AudioPortsConfigBuffer, PluginAudioPortsConfig};
use clack_host::prelude::*;
use komponist_ipc::sidechain_fallback::{
    CURRENT_CONFIGURATION, EXTENSION_ID, MAIN_INPUT, SidechainFallback,
};

/// Registers the disconnected-sidechain policy and, when advertised, selects the plugin's
/// standard CLAP auxiliary port configuration before activation.
pub(super) fn configure(handle: &PluginMainThreadHandle) -> bool {
    let plugin = handle.as_raw();
    let Some(extension) = plugin.get_extension else {
        return false;
    };
    // SAFETY: The CLAP plugin is initialized, inactive and on its main thread. For this ID the
    // plugin either returns null or its immutable v1 table.
    let fallback = unsafe {
        extension(plugin, EXTENSION_ID.as_ptr())
            .cast::<SidechainFallback>()
            .as_ref()
    };
    let Some(fallback) = fallback.filter(|table| table.mode == MAIN_INPUT) else {
        return false;
    };
    if fallback.auxiliary_config_id == CURRENT_CONFIGURATION {
        return true;
    }

    let Some(ports) = handle.get_extension::<PluginAudioPortsConfig>() else {
        return false;
    };
    let mut buffer = AudioPortsConfigBuffer::new();
    for index in 0..ports.count(handle) {
        let Some(config) = ports.get(handle, index, &mut buffer) else {
            continue;
        };
        if config.id.get() == fallback.auxiliary_config_id
            && config.input_port_count >= 2
            && config.main_input.is_some()
        {
            return ports.select(handle, config.id).is_ok();
        }
    }
    false
}
