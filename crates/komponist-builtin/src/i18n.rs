//! The texts of the built-in devices that come from the catalog (`komponist-i18n`): value words
//! and preset categories. Device, parameter and page names are built per language by the devices
//! themselves ([`crate::Internal::params_in`], [`crate::device::params_in`]).

use komponist_i18n::Lang;
use komponist_model::params::ValueTexts;

/// The words of a value text in `lang`: `On`/`Off` and the percent suffix. Every parameter of
/// the built-in devices carries them ([`komponist_model::params::ParamInfo::texts`]); the plugin
/// frame sets them on bridged plugins' parameters the same way.
pub fn value_texts(lang: Lang) -> ValueTexts {
    ValueTexts {
        on: lang.value_on(),
        off: lang.value_off(),
        percent: lang.value_percent(),
    }
}

/// The name of preset category `category` in `lang`. Categories are stored as their German
/// names (`hexa_core::categories`, [`crate::internal::Preset::category`]); a category the catalog
/// does not know is returned as it is. Preset names are English everywhere and not translated.
pub fn category_text(lang: Lang, category: &str) -> &str {
    match category {
        "Init" => lang.preset_category_init(),
        "Tasten" => lang.preset_category_keys(),
        "Mallets & Glocken" => lang.preset_category_mallets(),
        "Gezupft" => lang.preset_category_plucked(),
        "Bass" => lang.preset_category_bass(),
        "Blech & Holz" => lang.preset_category_winds(),
        "Streicher & Pads" => lang.preset_category_strings_pads(),
        "Leads" => lang.preset_category_leads(),
        "Synth" => lang.preset_category_synth(),
        "Effekte & Perkussion" => lang.preset_category_fx_percussion(),
        "Verzerrt & Lo-Fi" => lang.preset_category_distorted(),
        "Wavefolder" => lang.preset_category_wavefolder(),
        "Rhythmisch" => lang.preset_category_rhythmic(),
        "Filter-Operatoren" => lang.preset_category_filter_operators(),
        "Matrix" => lang.preset_category_matrix(),
        "Techno & House" => lang.preset_category_techno_house(),
        "Trance & Rave" => lang.preset_category_trance_rave(),
        "Synthbass" => lang.preset_category_synthbass(),
        "Chiptune" => lang.preset_category_chiptune(),
        "Ambient & Glitch" => lang.preset_category_ambient(),
        "Lead" => lang.preset_category_lead(),
        "Pad" => lang.preset_category_pad(),
        "Pluck" => lang.preset_category_pluck(),
        "Brass" => lang.preset_category_brass(),
        "FX" => lang.preset_category_fx(),
        other => other,
    }
}
