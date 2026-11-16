# Vendored: Steinberg ASIO SDK (host subset)

- **Source:** https://www.steinberg.net/asiosdk (ASIO SDK 2.3.4: `changes.txt` starts with "Changes in ASIO 2.3.4";
  file dates 2025-09-05 … 2025-10-13). Taken on 2026-11-16 (W8b).
- **Used by:** `asio-sys` 0.4 (cpal feature `asio`, Windows builds with the crate feature
  `komponist-audio/asio`). `.cargo/config.toml` sets `CPAL_ASIO_DIR` to this folder, so the build
  never downloads the SDK. `asio-sys`'s `build.rs` compiles every `.cpp` at depth 1 of `host`,
  `host/pc` and `common` (except `asiodrvr.cpp`) and runs bindgen over `asio.h`, `asiosys.h`,
  `asiodrivers.h`; the layout is the SDK's own.
- **Licence:** `LICENSE.txt` dual-licenses the SDK: (a) the proprietary Steinberg ASIO License, or
  (b) the GNU General Public License version 3. komponist is GPL-3.0-or-later and uses
  **option (b)**. Only option (a) forbids redistribution in parts or entirety; (b) allows it.
  The files carry either "subject to the license terms in the LICENSE file" (`common/*`) or the
  embedded BSD-style text of the same copyright holder (`host/*`); each file references the
  Steinberg licence. Nothing here is under the proprietary terms of option (a).
- **Byte-identical to upstream** (CRLF line endings kept): `.gitattributes` marks `vendor/asio-sdk/**` as
  `-text`. Do not reformat or edit these files.

## Taken

| Path | Why |
|---|---|
| `LICENSE.txt`, `common/LICENSE.txt`, `README.md`, `changes.txt` | licence and upstream notes |
| `common/asio.cpp`, `common/asio.h`, `common/asiosys.h`, `common/iasiodrv.h` | the host-side API (compiled / bindgen input) |
| `host/asiodrivers.cpp`, `host/asiodrivers.h`, `host/ginclude.h` | driver loading (`AsioDrivers`) |
| `host/pc/asiolist.cpp`, `host/pc/asiolist.h` | driver registry list (Windows) |
| `Steinberg ASIO Logo Artwork/*.svg`, `*.png` | the "ASIO compatible" logo in S07: the app embeds `ASIO-compatible-logo-Steinberg-®-white-transparent-RGB.svg` (`include_bytes!`, `dialogs/settings.rs`); the other three files are unused. Use is governed by Steinberg's ASIO usage guidelines (PDF, not vendored). |

## Left out

- **`common/combase.cpp`, `common/combase.h`, `common/dllentry.cpp`, `common/wxdebug.h`:** old Microsoft
  headers ("Copyright (c) 1992 - 1996 Microsoft Corporation. All Rights Reserved.") without a licence
  grant. They are driver-side COM helpers (`combase`, DLL entry points), not needed by a host. Never
  vendor them.
- **Driver side:** `driver/` (sample driver), `common/asiodrvr.cpp`, `common/asiodrvr.h`
  (`asio-sys` skips `asiodrvr.cpp`; the host build includes `asiodrvr.h` only on the Mac),
  `common/register.cpp` (driver registration), `common/debugmessage.cpp` (empty unless `DEBUG` on classic Mac;
  an `#error` otherwise).
- **`host/ASIOConvertSamples.{cpp,h}`, `host/sample/`:** sample conversion helper and the SDK's host sample;
  komponist converts in `komponist-audio`/cpal. The build proves they are not needed.
- Project files (`asio/*.dsw`, `*.dsp`, `*.vcproj`), the PDFs (guidelines, licensing agreement, manual),
  the EPS/PDF/JPG logo variants.
