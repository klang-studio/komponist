# komponist-i18n

Generates the typed English and German text catalog used by the app, device cards and accessible
controls.

## Structure

- [locales/](locales/) — Holds the German source and English translation as Fluent files.
- [build.rs](build.rs) — Runs the catalog generator during compilation.
- [generator.rs](generator.rs) — Parses and validates messages and emits Rust accessors.
- [src/lib.rs](src/lib.rs) — Exports the catalog and defines plural selection support.

## How it works

At build time the generator validates message IDs, variables and the supported Fluent syntax across
languages. It emits a `Lang` method per message. Plain messages return static strings, while
messages with variables format a new string. Callers choose the active language and pass typed
arguments. See the [translation guide](../../docs/I18N.md) for catalog conventions.
