//! The komponist design system on egui.
//!
//! Theme and fonts from `komponist-tokens`, pixel snapping, state styles, drag math, tooltips,
//! icons and the shared eframe setup. The binary `komponist-gallery` shows every component in
//! every state, like `Designsystem.dc.html`.
//!
//! - [`run`] starts an eframe app the komponist way (wgpu, X11, theme installed); the app
//!   (WP-30) and the gallery use it.
//! - [`install`] sets up an existing [`egui::Context`] (fonts, theme, icon loaders,
//!   focus-visible tracking); [`run`] and the snapshot harness call it.
//! - [`theme`], [`fonts`]: egui style and text styles from the tokens.
//! - [`px`]: crisp 1 px lines and pixel-snapped rects at any scale.
//! - [`state`], [`focus`]: hover/pressed/focus-visible/selected/disabled → colours, rings.
//! - [`drag`]: drag math (1 px = 1 step, 200 px = full range, Shift × 0.1, Ctrl × 10).
//! - [`tooltip`], [`icons`]: tooltip timing and frames; Lucide icons and drawn transport shapes.
//! - [`gallery_shell`]: the gallery frame and its sections, filled by WP-20/21/22.
//! - [`widgets`]: the components; value controls and meters (WP-21): [`widgets::number_field`],
//!   [`widgets::knob`], [`widgets::fader`], [`widgets::meter`].
//! - [`plugin_frame`]: the plugin frame's header and its measures, drawn by the app (Linux) and
//!   by the bridge's own frame window (macOS).
//! - [`timeline`]: paint functions of the timeline elements (track header, clip, note, ruler,
//!   grid) with their accessibility names, for the canvases of wave 4.
//! - `snapshot` (feature `snapshot`): the `egui_kittest` harness for snapshot tests at 1× and 2×.

pub mod drag;
pub mod focus;
pub mod fonts;
pub mod gallery_shell;
pub mod icons;
pub mod platform;
pub mod plugin_frame;
pub mod px;
pub mod state;
pub mod theme;
pub mod timeline;
pub mod tooltip;
pub mod widgets;

#[cfg(feature = "snapshot")]
pub mod snapshot;

pub use komponist_i18n::Lang;
pub use state::State;

/// The DAW's name in the wordmark and window titles (ARCHITECTURE §13.6).
pub const WORDMARK: &str = "komponist";

/// Where the UI language lives in an egui [`Context`](egui::Context).
const LANG_ID: &str = "komponist_lang";

/// The language of the UI in `ctx`: [`Lang::De`] (the source language and the test default)
/// until [`set_lang`] sets another. Each context has its own, so parallel tests do not share it.
pub fn lang(ctx: &egui::Context) -> Lang {
    ctx.data(|d| d.get_temp::<Lang>(egui::Id::unique(LANG_ID)))
        .unwrap_or_default()
}

/// Sets the language of the UI in `ctx` (the app does it once per frame). Widgets that own
/// texts read it with [`lang`].
pub fn set_lang(ctx: &egui::Context, lang: Lang) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique(LANG_ID), lang));
}

/// Sets up `ctx` for komponist: fonts, theme (dark, no animation), icon loaders, focus-visible
/// tracking, and no egui zoom shortcuts (Ctrl +/− belong to the arrangement zoom).
///
/// Idempotent: calling it again re-applies fonts and style.
pub fn install(ctx: &egui::Context) {
    fonts::install(ctx);
    theme::apply(ctx);
    icons::install(ctx);
    focus::install(ctx);
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

/// The application icon: the 256 px render of `assets/icons/hicolor` (concept "Zeiger").
const APP_ICON_PNG: &[u8] =
    include_bytes!("../../../assets/icons/hicolor/256x256/apps/komponist.png");

/// The window icon for every komponist window (root viewport, plugin frames), decoded once.
/// It shows in the title bar and task bar even where the icon theme has no `komponist` icon.
pub fn app_icon() -> std::sync::Arc<egui::IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        // The bytes are embedded and covered by a unit test, so a decode error is a build defect.
        std::sync::Arc::new(
            eframe::icon_data::from_png_bytes(APP_ICON_PNG).expect("embedded app icon is a PNG"),
        )
    })
    .clone()
}

/// Window and scale settings for [`run`].
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// eframe app name: key for persisted state and the label the inspection protocol reports.
    pub app_name: String,
    /// Title of the root viewport. Every viewport gets a distinct title, so the egui MCP server
    /// and X11 tools can tell them apart.
    pub title: String,
    /// X11 `WM_CLASS` / Wayland app id.
    pub app_id: String,
    /// Initial inner size of the window in points.
    pub inner_size: egui::Vec2,
    /// Smallest inner size in points, if any.
    pub min_inner_size: Option<egui::Vec2>,
    /// Fixed scale (`pixels_per_point`, 1× / 2×) instead of the monitor's, e.g. from the
    /// settings.
    pub pixels_per_point: Option<f32>,
}

impl RunOptions {
    /// Options for `app_name` with window title `title`; the app id is `app_name`.
    pub fn new(app_name: impl Into<String>, title: impl Into<String>) -> Self {
        let app_name = app_name.into();
        Self {
            app_id: app_name.clone(),
            app_name,
            title: title.into(),
            inner_size: egui::vec2(1920.0, 1080.0),
            min_inner_size: None,
            pixels_per_point: None,
        }
    }

    /// The eframe options: wgpu renderer, X11 forced on Linux (also under XWayland), the viewport
    /// from these settings.
    pub fn native_options(&self) -> eframe::NativeOptions {
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(self.title.clone())
            .with_app_id(self.app_id.clone())
            .with_icon(app_icon())
            .with_inner_size(self.inner_size);
        if let Some(min) = self.min_inner_size {
            viewport = viewport.with_min_inner_size(min);
        }
        eframe::NativeOptions {
            viewport,
            renderer: eframe::Renderer::Wgpu,
            // X11 only exists on Linux; macOS uses winit's default (Cocoa) event loop.
            #[cfg(target_os = "linux")]
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::x11::EventLoopBuilderExtX11 as _;
                builder.with_x11();
            })),
            ..Default::default()
        }
    }
}

/// Runs an eframe app with the komponist setup: [`RunOptions::native_options`], then
/// [`install`] and the scale override on the new context, then `make_app`.
///
/// With `EGUI_INSPECTION=1` in the environment, eframe serves the `egui_inspection` protocol on
/// 127.0.0.1:5719, so `egui-mcp` can attach (tree, input, screenshots).
pub fn run<A>(
    options: RunOptions,
    make_app: impl FnOnce(&eframe::CreationContext<'_>) -> A + 'static,
) -> eframe::Result
where
    A: eframe::App + 'static,
{
    let native = options.native_options();
    let scale = options.pixels_per_point;
    eframe::run_native(
        &options.app_name,
        native,
        Box::new(move |cc| {
            install(&cc.egui_ctx);
            if let Some(ppp) = scale {
                cc.egui_ctx.set_pixels_per_point(ppp);
            }
            Ok(Box::new(make_app(cc)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_options_force_wgpu_and_x11() {
        let o = RunOptions::new("komponist-test", "komponist · Test").native_options();
        assert_eq!(o.renderer, eframe::Renderer::Wgpu);
        // X11 is forced on Linux only.
        assert_eq!(o.event_loop_builder.is_some(), cfg!(target_os = "linux"));
        assert_eq!(o.viewport.title.as_deref(), Some("komponist · Test"));
        assert_eq!(o.viewport.app_id.as_deref(), Some("komponist-test"));
    }

    #[test]
    fn embedded_icon_decodes_to_256_px_rgba() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        // Rounded plate: the corner is transparent, the centre is not.
        assert_eq!(icon.rgba[3], 0);
        assert!(icon.rgba[(128 * 256 + 128) * 4 + 3] > 0);
        let o = RunOptions::new("komponist-test", "t").native_options();
        assert!(o.viewport.icon.is_some());
    }

    /// Every hicolor PNG exists with the dimensions its folder names (read from the IHDR chunk).
    #[test]
    fn hicolor_pngs_have_the_size_of_their_folder() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/hicolor");
        for n in [16u32, 24, 32, 48, 64, 128, 256, 512] {
            let path = root.join(format!("{n}x{n}/apps/komponist.png"));
            let b = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
            let w = u32::from_be_bytes(b[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(b[20..24].try_into().unwrap());
            assert_eq!((w, h), (n, n), "{}", path.display());
        }
        for f in [
            "scalable/apps/komponist.svg",
            "symbolic/apps/komponist-symbolic.svg",
        ] {
            assert!(root.join(f).is_file(), "{f}");
        }
    }

    #[test]
    fn install_sets_up_the_context() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        assert_eq!(ctx.style_of(egui::Theme::Dark).animation_time, 0.0);
        assert!(!ctx.options(|o| o.zoom_with_keyboard));
        assert!(ctx.style_of(egui::Theme::Light).visuals.dark_mode);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        install(&ctx);
    }

    #[test]
    fn language_is_per_context_and_german_by_default() {
        let a = egui::Context::default();
        let b = egui::Context::default();
        assert_eq!(lang(&a), Lang::De);
        set_lang(&a, Lang::En);
        assert_eq!(lang(&a), Lang::En);
        assert_eq!(lang(&b), Lang::De);
        set_lang(&a, Lang::De);
        assert_eq!(lang(&a), Lang::De);
    }
}
