# Installing komponist

## macOS

Every [release](https://github.com/klang-studio/komponist/releases) has `komponist-<tag>-macos-arm64.zip` (macOS 14.4
or newer, Apple Silicon only) and its `.sha256`.

1. Download and unzip it (Safari unzips automatically). Check the download with `shasum -a 256 -c komponist-<tag>-macos-arm64.zip.sha256`
   in the folder that holds both files.
2. Move `komponist.app` to `/Applications`.
3. First start: **v0.3 is signed ad-hoc, not with an Apple Developer ID**, so macOS refuses to open it ("komponist
   cannot be opened" / "Apple could not verify ..."). Allow it once:
   - macOS 15 and newer: try to open the app, then System Settings → Privacy & Security → scroll to the message
     about komponist → **Open Anyway**, and confirm. (Control-click → Open no longer works since macOS 15.)
   - On any version, in Terminal: `xattr -dr com.apple.quarantine /Applications/komponist.app`
     (the app starts normally afterwards).
   You do this once per downloaded version. A Developer ID signature with notarization will make it unnecessary;
   it follows once an Apple Developer account exists.
4. The first recording asks for microphone access (System Settings → Privacy & Security → Microphone).

**Projects:** a double-click on a `.komponist` file in Finder opens it in komponist, and so does dropping it on
komponist's Dock icon; with unsaved changes komponist asks first, as for a project from the recent list.

**Plugins** downloaded in a browser carry the same quarantine flag and may be refused or silently skipped. If a
downloaded CLAP or VST3 plugin does not show up or does not load, remove the flag from the bundle, e.g.
`xattr -dr com.apple.quarantine "/Library/Audio/Plug-Ins/CLAP/Name.clap"`, then rescan. komponist itself is signed with
`com.apple.security.cs.disable-library-validation`, so plugins signed by other teams (or ad-hoc) can load into it.
Plugin folders: `~/Library/Audio/Plug-Ins/CLAP`, `/Library/Audio/Plug-Ins/CLAP` and `$CLAP_PATH`;
`~/Library/Audio/Plug-Ins/VST3` and `/Library/Audio/Plug-Ins/VST3`. A plugin that compiles code at run time (JIT,
some WebView GUIs) could fail under komponist's hardened runtime; please report such a plugin.

**Where komponist keeps its files:** settings and recent projects in `~/Library/Application Support/komponist/`
(`settings.json`, `recent.json`), the plugin scan cache in `~/Library/Caches/komponist/plugins.json`. Delete the cache
file to force a full rescan; remove the folders to reset komponist.

**Building the app bundle yourself:** `scripts/bundle-macos.sh` builds `dist/komponist.app` (executable, `Info.plist`,
icon, hardened-runtime signature; ad-hoc unless `KOMPONIST_SIGN_IDENTITY` is set, see the header of the script).
`ARCHITECTURE.md` §8.7 describes the bundle.

## Windows

The Windows 11 (x86_64) port is in progress; the first release zip follows once the app builds there. Every
[release](https://github.com/klang-studio/komponist/releases) then has `komponist-<tag>-windows-x86_64.zip` and its
`.sha256`.

1. Download the zip. Check it in PowerShell: `Get-FileHash komponist-<tag>-windows-x86_64.zip -Algorithm SHA256` must
   print the hash in the `.sha256` file.
2. Unzip it anywhere (right-click → Extract All). komponist is one `komponist.exe`; there is nothing to install.
3. First start: **the zip is not signed with a code signing certificate (Authenticode)**, so SmartScreen shows
   "Windows protected your PC". Click **More info**, then **Run anyway**. Alternatively, before unzipping: right-click the zip →
   Properties → tick **Unblock**. You do this once per downloaded version. A signed build will make it unnecessary.
4. Audio goes through WASAPI. ASIO is not supported yet.

**Projects:** a double-click on a `.komponist` file does not open komponist yet; the file association (and a Start menu
entry) come with a later installer. Open a project from inside komponist, or pass it on the command line:
`komponist.exe "C:\path\to\project.komponist"`.

**Plugins:** CLAP plugins are searched in `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` and
`%CLAP_PATH%` (separated by `;`); VST3 plugins in `%COMMONPROGRAMFILES%\VST3` and `%LOCALAPPDATA%\Programs\Common\VST3`.
More folders can be added in the settings.

**Where komponist keeps its files:** settings and recent projects in `%APPDATA%\komponist\` (`settings.json`,
`recent.json`). Remove the folder to reset komponist.

**Command line:** `komponist.exe` is a windows-subsystem program (no console window). Started from a terminal it
attaches to that console for `--help` and error messages; the terminal does not wait for it, so the output can appear
after the next prompt. Piping it (`komponist.exe --help | more`) makes the shell wait.

**Building the zip yourself:** `scripts/package-windows.sh` (Git Bash) builds `komponist.exe` and writes
`dist/komponist-<version>-windows-x86_64.zip` with its `.sha256`; signing is optional, see the header of the script.
`ARCHITECTURE.md` §8.8 describes the package.

## Build from source

You need a Rust toolchain (`rust-toolchain.toml` pins the version) and the usual system libraries for egui/wgpu and
ALSA/PipeWire/JACK. Everything else is fetched by cargo: komponist depends on the public DSP library
[klang-dsp](https://github.com/klang-studio/klang-dsp) as a git dependency pinned to one commit (`Cargo.toml`,
`[workspace.dependencies]`), so a plain clone builds without any sibling checkout.

**macOS** (14.4 or newer, Apple Silicon only): install the Xcode command line tools (`xcode-select --install`) and
Rust through [rustup](https://rustup.rs); no further system libraries are needed. The scripts in `scripts/` run with
the stock `/bin/bash` 3.2; `scripts/check-gui.sh` is Linux only.

Working on komponist and klang-dsp together: check out klang-dsp next to komponist and patch the git dependency to
the local path. Do **not** edit the tracked `Cargo.toml` or `.cargo/config.toml` for this. Pick one:

```bash
# once per command (nothing to remember afterwards, affects nothing else):
cargo --config 'patch."https://github.com/klang-studio/klang-dsp".klang-dsp.path="../klang-dsp"' check --workspace

# or for every clone below one folder: put this into <parent folder>/.cargo/config.toml
# (cargo reads config files of all parent directories; clones elsewhere are not affected)
[patch."https://github.com/klang-studio/klang-dsp"]
klang-dsp = { path = "../klang-dsp" }   # relative to the folder that contains the .cargo directory
```

While the patch is active cargo rewrites the klang-dsp entry in `Cargo.lock`; do not commit that change
(`git checkout Cargo.lock`). When a klang-dsp change is pushed, bump `rev` in the root `Cargo.toml` and commit it
together with the komponist change that needs it.

## Bumping pinned git dependencies

Dependabot (`.github/dependabot.yml`) updates crates.io dependencies and GitHub Actions, but it does **not** move git
dependencies that are pinned by `rev`. Bump them by hand:

- **klang-dsp** (`Cargo.toml`, `[workspace.dependencies]`): push the klang-dsp change, then set `rev` to the new commit
  hash and run `cargo update -p klang-dsp` so `Cargo.lock` follows. Commit both with the komponist change that needs it.
- **test-plugins** (`test-plugins/Cargo.toml`): the `baseview`, `egui-baseview` and `nice-plug*` forks of klang-studio
  are pinned by `rev` as well. Set the new hash on every line of the same repository (all `nice-plug*` entries share
  one `rev`), run `cargo update` inside `test-plugins/` and check with `scripts/build-test-plugins.sh`.

## Executable

```bash
cargo build --release -p komponist-app --bin komponist
install -Dm755 target/release/komponist ~/.local/bin/komponist   # or $PREFIX/bin
```

The plugin bridge is part of the executable (`komponist --bridge`), so nothing else is needed.

## Icon, desktop entry, mime type, metainfo

The app id is **`io.github.klang_studio.komponist`** (`komponist_app::APP_ID`): desktop file name,
metainfo file name and `<id>`, `StartupWMClass`, and the window's Wayland app id / X11 `WM_CLASS`.
The icon name stays `komponist`. Icons come from
`assets/icons/hicolor/` (generated from the icon design sources, which are kept with the maintainers' notes).

```bash
scripts/install-desktop.sh                  # into ~/.local/share
PREFIX=/usr scripts/install-desktop.sh      # system-wide; packagers add DESTDIR=...
```

By hand (`$PREFIX` = `/usr` or `~/.local`):

```bash
for n in 16 24 32 48 64 128 256 512; do
  install -Dm644 assets/icons/hicolor/${n}x${n}/apps/komponist.png $PREFIX/share/icons/hicolor/${n}x${n}/apps/komponist.png
done
install -Dm644 assets/icons/hicolor/scalable/apps/komponist.svg $PREFIX/share/icons/hicolor/scalable/apps/komponist.svg
install -Dm644 assets/icons/hicolor/symbolic/apps/komponist-symbolic.svg $PREFIX/share/icons/hicolor/symbolic/apps/komponist-symbolic.svg
install -Dm644 assets/io.github.klang_studio.komponist.desktop $PREFIX/share/applications/io.github.klang_studio.komponist.desktop
install -Dm644 assets/komponist-mime.xml $PREFIX/share/mime/packages/komponist.xml
install -Dm644 assets/io.github.klang_studio.komponist.metainfo.xml $PREFIX/share/metainfo/io.github.klang_studio.komponist.metainfo.xml
gtk-update-icon-cache -f -t $PREFIX/share/icons/hicolor
update-desktop-database $PREFIX/share/applications
update-mime-database $PREFIX/share/mime
```

`Exec=komponist` needs the executable on `PATH`. The window shows its icon without any of this
(embedded 256 px PNG); the files are for launchers, task bars matching the desktop entry, and
`.komponist` file associations: the file manager opens a project with `komponist <file>` (`%f`, one project per
start).

Validate: `desktop-file-validate assets/io.github.klang_studio.komponist.desktop`,
`appstreamcli validate --no-net assets/io.github.klang_studio.komponist.metainfo.xml`.
