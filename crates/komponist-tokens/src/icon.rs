//! The Lucide icon subset (`assets/icons/`, ISC licence).

macro_rules! icons {
    ($( $(#[$doc:meta])* $variant:ident => $name:literal, )*) => {
        /// An icon of the UI: a Lucide SVG prepared for egui.
        ///
        /// [`svg_bytes`](Self::svg_bytes) returns the SVG with a 16 × 16 px intrinsic size
        /// (`size.icon`), a 1.5 px stroke at that size, and a **white** stroke: draw it with egui's
        /// image tint to give it a colour, e.g.
        /// `egui::Image::from_bytes(icon.uri(), icon.svg_bytes()).tint(text_colour)` (needs the
        /// egui_extras SVG loader). `assets/icons/README.md` maps every icon to the screens.
        ///
        /// The variants hide the file names, so the icon set can be replaced (the handoff calls
        /// Lucide a proposal). Play, Stop and Record are not icons: they are drawn as shapes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Icon {
            $( $(#[$doc])* $variant, )*
        }

        impl Icon {
            /// Every icon, in declaration order.
            pub const ALL: &'static [Icon] = &[ $( Icon::$variant, )* ];

            /// Lucide name (`"chevron-down"`); `star-filled` is the filled `star`.
            pub const fn name(self) -> &'static str {
                match self {
                    $( Icon::$variant => $name, )*
                }
            }

            /// The prepared SVG (see the type docs).
            pub const fn svg_bytes(self) -> &'static [u8] {
                match self {
                    $( Icon::$variant => include_bytes!(concat!(env!("OUT_DIR"), "/icons/", $name, ".svg")), )*
                }
            }

            /// A stable URI for egui's image cache: `bytes://komponist/icons/<name>.svg`. The
            /// `.svg` suffix lets the egui_extras SVG loader pick it up.
            pub const fn uri(self) -> &'static str {
                match self {
                    $( Icon::$variant => concat!("bytes://komponist/icons/", $name, ".svg"), )*
                }
            }
        }
    };
}

icons! {
    /// `search`: browser search field.
    Search => "search",
    /// `plus`: add tab, zoom in, add insert/effect/device/folder, icon buttons.
    Plus => "plus",
    /// `minus`: zoom out.
    Minus => "minus",
    /// `ellipsis` (`⋯`): transport menu, panel-header actions.
    Ellipsis => "ellipsis",
    /// `chevron-down` (`▾`): expanded panel header, dropdowns, "Maximieren".
    ChevronDown => "chevron-down",
    /// `chevron-right` (`▸`, `›`): collapsed panel header, browser folders, submenus, next preset.
    ChevronRight => "chevron-right",
    /// `chevron-left` (`‹`): previous preset.
    ChevronLeft => "chevron-left",
    /// `x` (`×`): close a dialog or the plugin window, remove a device.
    X => "x",
    /// `check` (`✓`): current dropdown value, checked menu item.
    Check => "check",
    /// `star` (`☆`): browser item that is not a favourite.
    Star => "star",
    /// `star`, filled (`★`): browser favourite.
    StarFilled => "star-filled",
    /// `triangle-exclamation-point` (`⚠`): "Plugin fehlt" in the track header.
    TriangleExclamationPoint => "triangle-exclamation-point",
    /// `settings`: settings dialog.
    Settings => "settings",
    /// `folder`: plugin folders, export folder.
    Folder => "folder",
    /// `folder-open`: open a project, show in folder.
    FolderOpen => "folder-open",
    /// `maximize-2`: maximise the dock.
    Maximize2 => "maximize-2",
    /// `minimize-2`: restore the maximised dock.
    Minimize2 => "minimize-2",
    /// `power`: device bypass.
    Power => "power",
    /// `sliders-horizontal`: generic plugin parameters.
    SlidersHorizontal => "sliders-horizontal",
    /// `piano`: Piano Roll editor.
    Piano => "piano",
    /// `grid-3x3`: Step Sequencer editor.
    Grid3x3 => "grid-3x3",
    /// `mouse-pointer`: piano-roll tool "Zeiger".
    MousePointer => "mouse-pointer",
    /// `pencil`: piano-roll tool "Stift".
    Pencil => "pencil",
    /// `eraser`: piano-roll tool "Radierer".
    Eraser => "eraser",
    /// `metronome`: metronome toggle in the transport bar.
    Metronome => "metronome",
    /// `repeat`: loop toggle in the transport bar.
    Repeat => "repeat",
}

/// Licence of the icons (Lucide ISC, plus MIT for the icons derived from Feather), for an about or
/// licence view.
pub const LICENSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/LICENSE"
));

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn text(icon: Icon) -> &'static str {
        std::str::from_utf8(icon.svg_bytes()).unwrap()
    }

    #[test]
    fn every_asset_has_a_variant_and_back() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons");
        let files: BTreeSet<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "svg"))
            .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        let names: BTreeSet<String> = Icon::ALL
            .iter()
            .map(|i| {
                i.name()
                    .strip_suffix("-filled")
                    .unwrap_or(i.name())
                    .to_owned()
            })
            .collect();
        assert_eq!(files, names);
        assert_eq!(Icon::ALL.len(), 26);
    }

    #[test]
    fn names_and_uris_are_unique() {
        let names: BTreeSet<_> = Icon::ALL.iter().map(|i| i.name()).collect();
        let uris: BTreeSet<_> = Icon::ALL.iter().map(|i| i.uri()).collect();
        assert_eq!(names.len(), Icon::ALL.len());
        assert_eq!(uris.len(), Icon::ALL.len());
        assert_eq!(
            Icon::ChevronDown.uri(),
            "bytes://komponist/icons/chevron-down.svg"
        );
    }

    #[test]
    fn icons_are_prepared() {
        for &icon in Icon::ALL {
            let svg = text(icon);
            assert!(
                svg.contains("@license lucide-static"),
                "{icon:?} lost its licence header"
            );
            assert!(
                svg.contains("width=\"16\"") && svg.contains("height=\"16\""),
                "{icon:?}"
            );
            assert!(svg.contains("stroke=\"#ffffff\""), "{icon:?}");
            assert!(svg.contains("stroke-width=\"2.25\""), "{icon:?}");
            assert!(!svg.contains("currentColor"), "{icon:?}");
            let filled = svg.contains("fill=\"#ffffff\"");
            assert_eq!(filled, icon == Icon::StarFilled, "{icon:?}");
        }
        assert!(LICENSE.starts_with("ISC License"));
    }
}
