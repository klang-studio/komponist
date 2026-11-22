//! Presse exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, Effect};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{DeviceLayout, WidgetKind};
use nice_plug::prelude::*;

struct Presse;

#[derive(Clone, Copy, Enum, PartialEq)]
enum Detector {
    #[id = "peak"]
    Peak,
    #[id = "rms"]
    #[name = "RMS"]
    Rms,
}

impl DeviceDefinition for Presse {
    const SPEC: &'static Spec = &komponist_presse::SPEC;
    const NAME: &'static str = "Presse";
    const ID: &'static str = "org.klang-studio.komponist.presse";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const SIDECHAIN: bool = true;
    const SIDECHAIN_FROM_MAIN: bool = true;
    const LATENCY_PARAMETER: Option<&'static str> = Some("lookahead");
    const REPORTS_REDUCTION: bool = true;
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Compressor,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        (info.id.as_str() == "detector").then(|| Binding::choice::<Detector>(info))
    }

    fn layout(lang: Lang) -> DeviceLayout {
        let mut grid = Self::SPEC.layout_in(lang).clone();
        for page in &mut grid.pages {
            page.cols = 7;
            page.dividers.clear();
            page.cells
                .retain(|cell| !matches!(cell.widget, WidgetKind::Graph | WidgetKind::MeterGr));
            for cell in &mut page.cells {
                cell.disabled_by = None;
            }
        }
        grid
    }
}

type PressePlugin = Effect<Presse>;

impl komponist_bundled_clap_adapter::AudioDefinition for Presse {
    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_presse::dsp()
    }
}

nice_export_clap!(PressePlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<PressePlugin>(14);
}

#[cfg(test)]
#[test]
fn defaults_to_stereo_and_advertises_the_alternate_sidechain_configuration() {
    use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_plugin_audio_ports};
    use clap_sys::ext::audio_ports_config::{
        CLAP_EXT_AUDIO_PORTS_CONFIG, clap_plugin_audio_ports_config,
    };

    let instance = layout_contract::Instance::new::<PressePlugin>();
    let plugin = instance.0;
    // SAFETY: The instance is live, initialized and inactive on this test's main thread.
    unsafe {
        let get = (*plugin).get_extension.unwrap();
        let ports = get(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
            .cast::<clap_plugin_audio_ports>()
            .as_ref()
            .unwrap();
        let count = ports.count.unwrap();
        assert_eq!(count(plugin, true), 1);
        assert_eq!(count(plugin, false), 1);

        let fallback = get(
            plugin,
            c"org.klang-studio.komponist.sidechain-fallback/1".as_ptr(),
        )
        .cast::<[u32; 2]>()
        .as_ref()
        .unwrap();
        assert_eq!(fallback[0], 1); // Main detector fallback.
        let configs = get(plugin, CLAP_EXT_AUDIO_PORTS_CONFIG.as_ptr())
            .cast::<clap_plugin_audio_ports_config>()
            .as_ref()
            .unwrap();
        assert!(configs.select.unwrap()(plugin, fallback[1]));
        assert_eq!(count(plugin, true), 2);
        assert_eq!(count(plugin, false), 1);
    }
}
