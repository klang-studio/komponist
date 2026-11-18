//! Localized reasons shared by the Warp editor and export status; engine errors stay UI-free.
use komponist_engine::audio::StretchError;
use komponist_i18n::Lang;

pub(crate) fn reason(t: Lang, error: StretchError) -> &'static str {
    match error {
        StretchError::Settings => t.export_warp_settings(),
        StretchError::Input => t.export_warp_input(),
        StretchError::Position => t.export_warp_position(),
        StretchError::Backend => t.export_warp_backend(),
        StretchError::SourceRead => t.export_warp_source_read(),
        StretchError::SourceSeek => t.export_warp_source_seek(),
        StretchError::SourceResample => t.export_warp_source_resample(),
    }
}
