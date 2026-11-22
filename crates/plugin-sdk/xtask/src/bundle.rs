use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

use crate::{
    BUNDLE_NAME, PLUGIN_BINARY, PLUGIN_ID, PLUGIN_PACKAGE, Result, USAGE, cargo, host_target,
};

#[derive(Default)]
pub struct Options {
    release: bool,
    target: Option<String>,
    plain: bool,
    out_dir: Option<PathBuf>,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--release" => options.release = true,
                "--no-default-features" => options.plain = true,
                "--target" => options.target = Some(args.next().ok_or(USAGE)?.to_owned()),
                "--out-dir" => options.out_dir = Some(args.next().ok_or(USAGE)?.into()),
                _ => return Err(USAGE.into()),
            }
        }
        Ok(options)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    fn from_target(target: &str) -> Result<Self> {
        if target.contains("-linux-") {
            Ok(Self::Linux)
        } else if target.ends_with("-apple-darwin") {
            Ok(Self::Macos)
        } else if target.contains("-windows-") {
            Ok(Self::Windows)
        } else {
            Err(format!("unsupported CLAP bundle target {target}").into())
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Linux => "so",
            Self::Macos => "dylib",
            Self::Windows => "dll",
        }
    }
}

pub fn build(options: Options) -> Result<PathBuf> {
    let target = options.target.clone().map_or_else(host_target, Ok)?;
    let platform = Platform::from_target(&target)?;
    let directory = match options.out_dir.as_deref() {
        Some(path) => env::current_dir()?.join(path),
        None => target_directory()?.join("bundled"),
    };
    let mut command = cargo();
    command.args([
        "build",
        "--locked",
        "-p",
        PLUGIN_PACKAGE,
        "--message-format=json-render-diagnostics",
        "--target",
        &target,
    ]);
    if options.release {
        command.arg("--release");
    }
    if options.plain {
        command.arg("--no-default-features");
    }
    let mut child = command.stdout(Stdio::piped()).spawn()?;
    let messages = BufReader::new(child.stdout.take().expect("piped Cargo stdout"));
    let artifact = read_artifact(messages, platform);
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("Cargo example build failed ({status})").into());
    }
    let artifact = artifact?.ok_or("Cargo did not report the example dynamic library")?;

    fs::create_dir_all(&directory)?;
    let bundle = directory.join(BUNDLE_NAME);
    if platform == Platform::Macos {
        let contents = bundle.join("Contents");
        let executable = contents.join("MacOS");
        fs::create_dir_all(&executable)?;
        fs::copy(&artifact, executable.join(PLUGIN_BINARY))?;
        fs::write(contents.join("Info.plist"), plist())?;
    } else {
        fs::copy(&artifact, &bundle)?;
    }
    Ok(bundle)
}

fn read_artifact(mut messages: impl BufRead, platform: Platform) -> Result<Option<PathBuf>> {
    let mut artifact = None;
    let mut line = String::new();
    while messages.read_line(&mut line)? != 0 {
        if let Ok(message) = serde_json::from_str::<Value>(&line) {
            if let Some(rendered) = message["message"]["rendered"].as_str() {
                eprint!("{rendered}");
            }
            if message["reason"] == "compiler-artifact"
                && message["target"]["name"] == PLUGIN_BINARY
                && let Some(paths) = message["filenames"].as_array()
            {
                artifact = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(PathBuf::from)
                    .find(|path| {
                        path.extension()
                            .is_some_and(|ext| ext == platform.extension())
                    });
            }
        }
        line.clear();
    }
    Ok(artifact)
}

fn target_directory() -> Result<PathBuf> {
    let metadata = cargo()
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .output()?;
    if !metadata.status.success() {
        return Err(String::from_utf8_lossy(&metadata.stderr)
            .into_owned()
            .into());
    }
    let data: Value = serde_json::from_slice(&metadata.stdout)?;
    Ok(PathBuf::from(
        data["target_directory"]
            .as_str()
            .ok_or("Cargo target directory is missing")?,
    ))
}

fn plist() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>{PLUGIN_BINARY}</string>
    <key>CFBundleIdentifier</key><string>{PLUGIN_ID}</string>
    <key>CFBundleName</key><string>Native CLAP Example</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
</dict>
</plist>
"#
    )
}

pub fn executable(bundle: &Path) -> Result<PathBuf> {
    if bundle.extension().is_none_or(|ext| ext != "clap") {
        return Err(format!("expected a .clap path: {}", bundle.display()).into());
    }
    if cfg!(target_os = "macos") {
        let path = bundle.join("Contents");
        let actual = fs::read_to_string(path.join("Info.plist"))?;
        if actual != plist() {
            return Err("unexpected macOS bundle Info.plist".into());
        }
        let executable = path.join("MacOS").join(PLUGIN_BINARY);
        if !executable.is_file() {
            return Err(
                format!("macOS bundle executable missing: {}", executable.display()).into(),
            );
        }
        Ok(executable)
    } else if bundle.is_file() {
        Ok(bundle.to_owned())
    } else {
        Err(format!("CLAP module is not a file: {}", bundle.display()).into())
    }
}
