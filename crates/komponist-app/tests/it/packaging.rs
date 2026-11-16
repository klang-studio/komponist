//! The app id agrees across the code, the desktop file, the metainfo, the installer, the release
//! workflow and the install docs (#24); the macOS bundle opens project files and its icon has every
//! size, and Linux registers the same project type (#96).

use std::path::PathBuf;

use komponist_app::APP_ID;

const HOMEPAGE: &str = "https://github.com/klang-studio/komponist";

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn read(path: &str) -> String {
    std::fs::read_to_string(repo(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn the_app_id_is_reverse_dns_of_the_repository() {
    assert_eq!(APP_ID, "io.github.klang_studio.komponist");
}

#[test]
fn desktop_file_and_metainfo_carry_the_app_id() {
    let desktop = read(&format!("assets/{APP_ID}.desktop"));
    assert!(
        desktop
            .lines()
            .any(|l| l == format!("StartupWMClass={APP_ID}"))
    );
    assert!(desktop.lines().any(|l| l == "Icon=komponist"));

    let info = read(&format!("assets/{APP_ID}.metainfo.xml"));
    assert!(info.contains(&format!("<id>{APP_ID}</id>")));
    assert!(info.contains(&format!(
        r#"<launchable type="desktop-id">{APP_ID}.desktop</launchable>"#
    )));
    assert!(info.contains(&format!(r#"<url type="homepage">{HOMEPAGE}</url>"#)));
}

#[test]
fn no_old_file_name_is_left_behind() {
    for old in ["assets/komponist.desktop", "assets/komponist.metainfo.xml"] {
        assert!(!repo(old).exists(), "{old} was renamed");
    }
    // The old names do not appear in the installer, the release workflow or the docs.
    for file in [
        "scripts/install-desktop.sh",
        ".github/workflows/release.yml",
        "docs/INSTALL.md",
    ] {
        let text = read(file);
        for old in ["komponist.desktop", "komponist.metainfo.xml"] {
            let stale = text.match_indices(old).any(|(at, _)| {
                !text[..at].ends_with("klang_studio.") && !text[..at].ends_with("$APP_ID.")
            });
            assert!(!stale, "{file} still names {old}");
        }
    }
    assert!(read("scripts/install-desktop.sh").contains(&format!("APP_ID={APP_ID}")));
    assert!(read(".github/workflows/release.yml").contains(&format!("{APP_ID}.desktop")));
}

/// The lines of `text` trimmed and joined, for comparing plist snippets.
fn squeezed(text: &str) -> String {
    text.lines().map(str::trim).collect()
}

/// The width and height of a PNG file (its IHDR chunk).
fn png_size(path: &str) -> (u32, u32) {
    let bytes = std::fs::read(repo(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{path} is a PNG");
    assert_eq!(&bytes[12..16], b"IHDR");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn the_macos_bundle_opens_project_files() {
    let plist = squeezed(&read("assets/macos/Info.plist.in"));
    let uti = "io.github.klang-studio.komponist.project";
    assert!(plist.contains(&format!(
        "<key>UTTypeIdentifier</key><string>{uti}</string>"
    )));
    // #96: komponist edits and owns its project type, so Finder opens it with komponist.
    let types = plist
        .split_once("<key>CFBundleDocumentTypes</key><array>")
        .and_then(|(_, rest)| rest.split_once("</array></dict></array>"))
        .map(|(types, _)| types)
        .expect("CFBundleDocumentTypes");
    for entry in [
        "<key>CFBundleTypeName</key><string>komponist project</string>",
        "<key>CFBundleTypeRole</key><string>Editor</string>",
        "<key>LSHandlerRank</key><string>Owner</string>",
        &format!("<key>LSItemContentTypes</key><array><string>{uti}</string>"),
    ] {
        assert!(types.contains(entry), "{entry} in {types}");
    }
    // #100: the plugin bridges (this executable) are UI elements, never a second Dock icon.
    assert!(plist.contains("<key>LSUIElement</key><true/>"));
    // The type's name in both languages.
    for (lang, name) in [("de", "komponist-Projekt"), ("en", "komponist project")] {
        let strings = read(&format!("assets/macos/{lang}.lproj/InfoPlist.strings"));
        assert!(
            strings.contains(&format!("\"komponist project\" = \"{name}\";")),
            "{lang}"
        );
    }
}

#[test]
fn linux_and_macos_register_the_same_project_type() {
    let plist = squeezed(&read("assets/macos/Info.plist.in"));
    let mime = "application/x-komponist-project";
    assert!(plist.contains(&format!(
        "<key>public.mime-type</key><string>{mime}</string>"
    )));
    assert!(
        plist.contains("<key>public.filename-extension</key><array><string>komponist</string>")
    );
    let shared = read("assets/komponist-mime.xml");
    assert!(shared.contains(&format!(r#"<mime-type type="{mime}">"#)));
    assert!(shared.contains(r#"<glob pattern="*.komponist"/>"#));
    let desktop = read(&format!("assets/{APP_ID}.desktop"));
    assert!(desktop.lines().any(|l| l == format!("MimeType={mime};")));
    // One project at a time: a file manager starts komponist once per file (`%f`), never with a
    // list of files the command line would refuse.
    assert!(desktop.lines().any(|l| l == "Exec=komponist %f"));
    assert!(read("scripts/install-desktop.sh").contains("assets/komponist-mime.xml"));
}

#[test]
fn the_icns_gets_every_size_up_to_1024_px() {
    let script = read("scripts/bundle-macos.sh");
    // The iconset names and the hicolor size each comes from (`16:icon_16x16 …`).
    let pairs: Vec<(u32, String)> = script
        .split_whitespace()
        .filter_map(|word| {
            let (size, name) = word.split_once(":icon_")?;
            Some((
                size.parse().ok()?,
                format!("icon_{}", name.trim_end_matches(';')),
            ))
        })
        .collect();
    let mut names: Vec<String> = pairs.iter().map(|(_, n)| n.clone()).collect();
    for (size, name) in &pairs {
        let png = format!("assets/icons/hicolor/{size}x{size}/apps/komponist.png");
        assert_eq!(png_size(&png), (*size, *size), "{png} for {name}");
    }
    // 512@2x is a render of its own (#96).
    let big = "assets/macos/icon_512x512@2x.png";
    assert!(script.contains(&format!("cp \"{big}\" \"$iconset/icon_512x512@2x.png\"")));
    assert_eq!(png_size(big), (1024, 1024));
    names.push("icon_512x512@2x".into());
    names.sort();
    let mut want: Vec<String> = [16, 32, 128, 256, 512]
        .iter()
        .flat_map(|n| [format!("icon_{n}x{n}"), format!("icon_{n}x{n}@2x")])
        .collect();
    want.sort();
    assert_eq!(names, want, "every size of an iconset");
}

/// The icns of the bundle holds the 1024 px image (`ic10`): the iconset as `bundle-macos.sh`
/// assembles it, through `iconutil`.
#[cfg(target_os = "macos")]
#[test]
fn iconutil_puts_the_1024_px_image_into_the_icns() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("icns-{}", std::process::id()));
    let iconset = dir.join("komponist.iconset");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&iconset).unwrap();
    for n in [16, 32, 128, 256, 512] {
        let src = |m: u32| repo(&format!("assets/icons/hicolor/{m}x{m}/apps/komponist.png"));
        std::fs::copy(src(n), iconset.join(format!("icon_{n}x{n}.png"))).unwrap();
        let double = if n == 512 {
            repo("assets/macos/icon_512x512@2x.png")
        } else {
            src(2 * n)
        };
        std::fs::copy(double, iconset.join(format!("icon_{n}x{n}@2x.png"))).unwrap();
    }
    let icns = dir.join("komponist.icns");
    let status = std::process::Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(&icns)
        .status()
        .expect("iconutil");
    assert!(status.success());
    let bytes = std::fs::read(&icns).unwrap();
    assert_eq!(&bytes[..4], b"icns");
    let mut types = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        types.push(String::from_utf8_lossy(&bytes[at..at + 4]).into_owned());
        let len = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        assert!(len >= 8);
        at += len;
    }
    // ic10: 512@2x (1024 px); ic09: 512 px.
    assert!(types.iter().any(|t| t == "ic10"), "{types:?}");
    assert!(types.iter().any(|t| t == "ic09"), "{types:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
