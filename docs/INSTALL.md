# Installing komponist

## Executable

```bash
cargo build --release -p komponist-app --bin komponist
install -Dm755 target/release/komponist ~/.local/bin/komponist   # or $PREFIX/bin
```

The plugin bridge is part of the executable (`komponist --bridge`), so nothing else is needed.

## Icon, desktop entry, mime type, metainfo

The app id is **`komponist`**: icon name, desktop file name, `StartupWMClass`, and the window's
Wayland app id / X11 `WM_CLASS` (`panels::transport::WORDMARK`). Icons come from
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
install -Dm644 assets/komponist.desktop $PREFIX/share/applications/komponist.desktop
install -Dm644 assets/komponist-mime.xml $PREFIX/share/mime/packages/komponist.xml
install -Dm644 assets/komponist.metainfo.xml $PREFIX/share/metainfo/komponist.metainfo.xml
gtk-update-icon-cache -f -t $PREFIX/share/icons/hicolor
update-desktop-database $PREFIX/share/applications
update-mime-database $PREFIX/share/mime
```

`Exec=komponist` needs the executable on `PATH`. The window shows its icon without any of this
(embedded 256 px PNG); the files are for launchers, task bars matching the desktop entry, and
`.komponist` file associations.

Validate: `desktop-file-validate assets/komponist.desktop`,
`appstreamcli validate --no-net assets/komponist.metainfo.xml`.
