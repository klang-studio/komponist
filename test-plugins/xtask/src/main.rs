//! `cargo xtask bundle [-p <package>]... [<cargo build args>]`: builds the test plugins and bundles
//! them as CLAP and VST3 into `target/bundled/`. Without `-p` it bundles all of them.
//!
//! `nice_plug_xtask::main()` takes the topmost ancestor directory with a `Cargo.toml` as the
//! workspace root. This workspace is nested in komponist's, so that would be komponist's root.
//! Hence this sets the root itself and calls nice-plug-xtask's build and bundle steps directly.

use std::path::Path;

/// The plugin packages of this workspace.
const PLUGINS: [&str; 4] = [
    "kp-test-synth",
    "kp-test-fx",
    "kp-test-crash",
    "kp-test-slow",
];

const USAGE: &str = "usage: cargo xtask bundle [-p <package>]... [<cargo build args>]";

fn main() -> nice_plug_xtask::Result<()> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lies inside the workspace");
    std::env::set_current_dir(workspace)?;

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("bundle") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let mut packages = Vec::new();
    let mut cargo_args = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "-p" || arg == "--package" {
            let Some(package) = args.next() else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            packages.push(package);
        } else {
            cargo_args.push(arg);
        }
    }
    if packages.is_empty() {
        packages = PLUGINS.map(String::from).to_vec();
    }

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(workspace.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let target_dir = metadata.target_directory.as_std_path();

    nice_plug_xtask::build(&packages, &cargo_args)?;
    for package in &packages {
        nice_plug_xtask::bundle(target_dir, package, &cargo_args, false)?;
    }
    Ok(())
}
