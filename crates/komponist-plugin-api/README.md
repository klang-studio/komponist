# komponist-plugin-api

Defines the CLAP extension contracts, parameter metadata and frame types shared by the plugin SDK,
the app and plugin hosts.

- [Native layout](src/native_layout.rs), [canvas data](src/device_canvas.rs) and
  [parameter metadata](src/params.rs) describe and validate device panels.
- [Frame data](src/frame.rs) describes the header, position and keyboard policy for plugin windows.
- [Extension contracts](../../docs/clap-device-layout/README.md) cover asset links, waveform editing,
  signal taps and other optional CLAP capabilities.

The bridge control protocol uses these types as payloads. The crate has no dependency on bridge
processes, OS signalling or the UI toolkit.
