# Installing komponist

## Build from source

You need a Rust toolchain (`rust-toolchain.toml` pins the version) and the usual system libraries for egui/wgpu and
ALSA/PipeWire/JACK. Everything else is fetched by cargo: komponist depends on the public DSP library
[klang-dsp](https://github.com/klang-studio/klang-dsp) as a git dependency pinned to one commit (`Cargo.toml`,
`[workspace.dependencies]`), so a plain clone builds without any sibling checkout.

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
`.komponist` file associations.

Validate: `desktop-file-validate assets/io.github.klang_studio.komponist.desktop`,
`appstreamcli validate --no-net assets/io.github.klang_studio.komponist.metainfo.xml`.
