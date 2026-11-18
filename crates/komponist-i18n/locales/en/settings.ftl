## S07 settings (`dialogs/settings.rs`).

# S07 title of the dialog.
settings-title = Settings

# S07 navigation tab: audio.
settings-nav-audio = Audio

# S07 navigation tab: MIDI.
settings-nav-midi = MIDI

# S07 navigation tab: plugins.
settings-nav-plugins = Plugins

# S07 navigation tab: appearance.
settings-nav-display = Appearance

# S07 navigation tab: keyboard.
settings-nav-keyboard = Keyboard

# S07 section "Audio".
settings-audio = Audio

# S07 section "MIDI-Eingänge".
settings-midi-inputs = MIDI Inputs

# S07 section "Plugin-Ordner".
settings-plugin-dirs = Plugin Folders

# S07 section "Darstellung".
settings-display = Appearance

# S07 section "Tastatur".
settings-keyboard = Keyboard

# S07 row label and name of the driver control.
settings-driver = Driver

# S07 accessible name of the "ASIO Compatible" logo next to the driver segments (Windows builds with ASIO).
settings-asio-logo = ASIO compatible

# S07 row label and name of the output device dropdown.
settings-device = Output device

# S07 row label and name of the sample rate dropdown.
settings-rate = Sample rate

# S07 row label and name of the buffer size control.
settings-buffer = Buffer size

# S07 hint under the buffer size segments when the driver fixes the size (WASAPI shared mode: the device period); `$frames` is the real size, e.g. "480".
settings-buffer-fixed = The driver sets { $frames } samples.

# S07 first entry of the output device dropdown: the driver's default.
settings-default-device = Default Device

# S07 notice without a device.
settings-no-device-notice = Device not connected. Playback and recording are stopped; the project stays editable.

# S07 button of that notice.
settings-retry = Search Again

# S07 hint when the audio thread got no real-time priority.
settings-no-realtime = Audio runs without real-time priority; dropouts are possible. RTKit or the “audio” group allows it.

# S07 button: add a plugin folder.
settings-add-folder = + Folder

# S07 button: scan the plugin folders again.
settings-rescan = Rescan

# S07 name of the × button of an added plugin folder.
settings-remove-folder = Remove folder

# S07 text without MIDI inputs.
settings-no-midi = No MIDI inputs found

# S07 MIDI row, input switched off.
settings-midi-off = off

# S07 MIDI row, input on and connected.
settings-midi-active = active · channels 1–16

# S07 MIDI row, input on but not present.
settings-midi-disconnected = not connected

# S07 MIDI row, input on and present but another program holds it (Windows: a MIDI input is exclusive).
settings-midi-busy = in use · another program holds the input

# S07 row label and name of the UI scale control.
settings-scale = UI scale

# S07 UI scale segment: follow the display.
settings-scale-auto = Automatic

# S07 UI scale segment; `$percent` is 100, 125, 150 or 200.
settings-scale-percent = { $percent }%

# S07 row label and name of the UI language control (issue #5; not in the design handoff).
settings-language = Language

# S07 UI language segment: follow the desktop's language.
settings-language-system = System

# S07 UI language segment; the name of a language is written in that language in every UI language.
settings-language-de = Deutsch

# S07 UI language segment; the name of a language is written in that language in every UI language.
settings-language-en = English

# S07 label of the computer keyboard switch.
settings-keyboard-midi = Computer keyboard plays notes

# S07 hint under that switch (prototype start screen "Tastatur").
settings-keyboard-hint = A W S E D F T G Z H U J K play notes while a track is armed or this switch is on.

# S07 scan summary while a scan runs.
settings-scan-running = Scanning…

# S07 scan summary before the first scan.
settings-scan-none = No scan yet

# S07 scan summary: plugins found.
settings-scan-found = { $n ->
        [one] 1 plugin found
       *[other] { $n } plugins found
    }

# S07 scan summary: when the last scan ended; `$when` is a day and time ("heute 09:12").
settings-scan-last = last scan { $when }

# S07 scan summary: plugins that could not be loaded.
settings-scan-skipped = { $n ->
        [one] 1 plugin skipped (cannot be loaded)
       *[other] { $n } plugins skipped (cannot be loaded)
    }

settings-input-device = Input device
settings-capture-format = Recording format
settings-capture-pcm24 = 24-bit WAV
settings-capture-float32 = 32-bit Float WAV

settings-jack-playback-change-recording = Finish recording before changing the JACK outputs.
settings-jack-playback-left = Master left
settings-jack-playback-right = Master right
settings-jack-automatic = Automatic
settings-jack-disconnected = Disconnected
settings-jack-capture-channel = Input { $channel }
settings-jack-add-capture = Add Input Port
settings-jack-remove-capture = Remove Input
settings-jack-auto-capture = Automatic Input Routing
settings-jack-port-missing = Disconnected · { $port }
settings-jack-scan-failed = Could not read JACK ports.
settings-jack-playback-unconnected = Could not connect JACK output · { $port }
settings-jack-inputs = JACK input ports
