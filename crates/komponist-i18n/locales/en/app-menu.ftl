## The native macOS app menu (`macos_menu`, issue #47, owner decision 6): About, Settings, Hide, Quit. Everything else stays in the egui menu bar.

# App menu: the About panel.
app-menu-about = About komponist

# App menu: opens Settings (⌘,).
app-menu-settings = Settings…

# App menu: hides the app (⌘H).
app-menu-hide = Hide komponist

# App menu: hides every other app (⌥⌘H).
app-menu-hide-others = Hide Others

# App menu: shows all hidden apps.
app-menu-show-all = Show All

# App menu: quits, asking to save a dirty project first (⌘Q).
app-menu-quit = Quit komponist

## The Window menu of the native menu bar and the Dock menu (macOS, issue #95); the plugin windows also in the "⋯" menu of the transport bar.

# Window menu: its title (macOS's own term).
app-menu-window = Window

# Window menu: minimises the main window into the Dock (⌘M; macOS's own term).
app-menu-minimize = Minimize

# Window menu: zooms the main window (macOS's own term).
app-menu-zoom = Zoom

# Window menu, Dock menu and the "⋯" menu: one open plugin window, brings it to the front; `$plugin` is the plugin name, `$track` the track (or "Master"), as in the frame's window title.
app-menu-plugin-window = { $plugin } · { $track }
