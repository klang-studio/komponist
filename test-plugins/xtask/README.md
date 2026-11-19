# xtask

Builds and bundles the fixture plugins in the test workspace.

## Structure

- [src/main.rs](src/main.rs) — Parses the bundle command and calls the build and bundle helpers
  from nice-plug-xtask.

## How it works

The entry point selects the fixture workspace explicitly because it sits inside the main
repository. From `test-plugins/` , `cargo xtask bundle` builds all fixtures and writes CLAP and
VST3 bundles under `target/bundled/` . Use `-p <package>` to select a fixture and `--release` for a
release build.

See the [test workspace README](../README.md) for fixture setup.
