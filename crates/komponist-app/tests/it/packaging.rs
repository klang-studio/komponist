//! The app id agrees across the code, the desktop file, the metainfo, the installer, the release
//! workflow and the install docs (#24).

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
