# komponist

A MIDI DAW for Linux in Rust (egui/eframe, cpal, CLAP and VST3 hosting in bridge processes).
Architecture: `docs/ARCHITECTURE.md`.

**Work items live on the GitHub board** (owner, 2026-11-15): https://github.com/orgs/klang-studio/projects/2, backed by
issues in `klang-studio/komponist` (labels `area: …`, `epic`; epics have sub-issues). An agent's brief names its issue;
the branch is `wp/<issue-number>-<slug>`, and the merge commit says `Closes #<n>`. Status moves Backlog → Ready →
In progress → In review → Done. Questions to the owner are draft items on the board. The old `PLAN.md` / `PROGRESS.md`
in the internal notes are an archive only.

**Internal notes live outside the repo (owner, 2026-11-15):** paths like `../.claude/komponist/` are relative to the
repo root and resolve in worktrees too (`.claude/worktrees/.claude` links back to `.claude/`). proposals (with mockups), evaluations,
spikes and reviews go to the internal notes folder `../.claude/komponist/` next to the repo (`proposals/<topic>/`, …),
never into `docs/`. `docs/` holds only public documentation
(`ARCHITECTURE.md`, `I18N.md`, `INSTALL.md`, `PERFORMANCE.md`). In code and public docs, cite such notes without a
`docs/` path, e.g. „fader-thumb proposal, variant D (internal notes)“.

**Naming:** the DAW is called **komponist**. *klang-studio* is the umbrella name of the workspace
(the parent folder of this repo: komponist, pisynth, klopfer, klang-dsp, the historical design handoff). The
design handoff says "klang-studio" where it means the DAW; read it as "komponist".

## Worktrees

All worktrees live in **`../.claude/worktrees/`** of the workspace (owner's rule, 2026-11-14). komponist builds
without any sibling folder (klang-dsp is a git dependency, `hexa-core` and `sampler-core` are vendored), so a worktree
needs no symlinks for the build. That folder still holds symlinks to the siblings (`klang-dsp`, `pisynth`, `klopfer`,
`komponist`, `klang-studio-handoff`, `klang-studio-geraete-handoff`, `daw-design-briefing.md`) for the local klang-dsp
patch below and the design handoff files. Do **not** use the Agent tool's `isolation: "worktree"` (it puts the worktree
where the symlinks are missing).

```bash
cd komponist                                       # the main checkout
scripts/worktree.sh add kp-wt-WP-60 wp/WP-60      # → ../.claude/worktrees/kp-wt-WP-60
scripts/worktree.sh remove kp-wt-WP-60            # after the merge
```

Each worktree builds into its own `target/`, which `scripts/worktree.sh` makes a symlink into
`$KOMPONIST_TARGET_ROOT/<name>/` when that variable is set (on the owner's machine an NVMe folder, set in the
untracked `scripts/local.env`), so build I/O stays off the encrypted system disk. The first
build of egui/wgpu takes a few minutes.

**Build load (owner, 2026-11-14):** builds froze the owner's desktop. Therefore: at most **2 agents
build at the same time**; `.cargo/config.toml` caps cargo at 6 jobs; while working, iterate narrowly
(`cargo check -p <crate>`, `cargo test -p <crate> --test it <module>::`) and run `scripts/check.sh` **once**
before reporting. `check.sh`, `check-gui.sh` and `run.sh` take a shared build lock and run at low CPU/IO
priority, so the gate may wait for another worktree: start it with `run_in_background`.

**Never run `cargo clean` in a worktree or in `komponist/`:** it deletes the `target` symlink and the next build lands on
the system disk again. Remove build output with `scripts/worktree.sh remove` (worktrees) or by deleting the NVMe folder.

**Test binaries (WP-66):** every file in a crate's `tests/` is its own binary that links egui/wgpu again, so
each crate has **one** integration binary, `tests/it/main.rs`, with one `mod` per topic (shared helpers such as
`tests/common/` are declared there once with `#[path]`). A new test goes into a module of `it`, not into a new
`tests/*.rs`. Only tests that need their own process stay separate: winit event loops (`plugin_frame_gui`,
`acceptance_flow4_gui`, `plugin_frame_installed`, run by `scripts/check-gui.sh`) and the `--release --ignored`
perf suites that `docs/PERFORMANCE.md` names (`soak`, `engine_perf`, `arrangement_perf`), and tests that count
the child processes of the test process (`plugin_exit`). Run one module:
`cargo test -p komponist-app --test it piano_roll::`.

**klang-dsp is a git dependency** (`Cargo.toml`, pinned `rev`). To change klang-dsp and komponist together, patch
it to `../klang-dsp` without touching tracked files: either
`cargo --config 'patch."https://github.com/klang-studio/klang-dsp".klang-dsp.path="../klang-dsp"' <cmd>`, or an
untracked `.cargo/config.toml` in a *parent* folder of the clones (cargo merges the config files of all parent
directories, so other clones are unaffected) with `[patch."https://github.com/klang-studio/klang-dsp"]` and
`klang-dsp = { path = "../klang-dsp" }` (see `docs/INSTALL.md`). Never commit the resulting `Cargo.lock` change;
after pushing klang-dsp, bump `rev`.

## macOS (epic #41)

komponist runs on macOS 14.4+ (Apple Silicon only) as well as on Linux. Agents on a Mac:

- **macOS gate:** `scripts/check.sh` as on Linux (the lock uses `mkdir` where `flock` is missing; scripts run with the
  stock bash 3.2, so no bash-4 features in them). `scripts/check-gui.sh` is Linux only and exits cleanly with a note.
  Run `scripts/build-test-plugins.sh` once before it when you touch catalog, plugins, devices, browser or engine.
- **Linux gate:** the Linux side is checked in Docker (aarch64), with the Linux gate (internal notes), which mounts the
  worktree and runs the commands you give it. Linux and macOS test plugins coexist in one worktree: bundles live in
  `target/test-plugins/<os>/{clap,vst3}` (`<os>` = `std::env::consts::OS`, i.e. `linux` or `macos`) and the plugin
  workspace builds into `test-plugins/target/<os>`. Test helpers look them up through that path.
- **Snapshots:** Linux (lavapipe) is the reference. On macOS `kittest.toml` `[mac]` renders every snapshot without
  comparing; **never run `UPDATE_SNAPSHOTS` on macOS**, it would overwrite the reference PNGs with Metal pixels.
- **No `/proc` on macOS:** tests that list or inspect processes have a `cfg(target_os)` path (`ps`, `kill -0`) or
  are gated with a reason.
- CI has a macOS arm64 job (clippy, tests, test plugins; no GUI tests, no format check).

## Windows (port in progress)

komponist is being ported to Windows 11 x86_64 (MSVC; integration branch `windows`; no GitHub issues yet). The workspace
builds and its gate passes there; plugin GUI frames are not done yet (W6). What is true now:

- **Shell:** the scripts are bash: run them in Git Bash (`scripts/check.sh`, `scripts/build-test-plugins.sh`). The gate is
  the same as on Linux: `scripts/check.sh`, which sets `WGPU_BACKEND=vulkan` on Windows (DX12 crashed parallel kittest
  tests). `scripts/check-gui.sh` is Linux only.
- **Audio and MIDI:** always `--null-audio` for live runs (rule 14). WASAPI is the default host; ASIO needs the cargo
  feature `asio` and LLVM (`LIBCLANG_PATH`), the SDK comes from `vendor/asio-sdk/`.
- **Cross clippy from Windows** (Linux and macOS code stays green while the port lands):
  `cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings` and
  `cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu --exclude komponist-audio --exclude komponist-app -- -D warnings`.
- **Test plugins:** `scripts/build-test-plugins.sh` builds them into `test-plugins/target/windows` and copies them to
  `target/test-plugins/windows/{clap,vst3}`: CLAP is a single `<name>.clap` DLL, VST3 a bundle
  `<name>.vst3/Contents/x86_64-win/<name>.vst3`.
- **Snapshots:** Linux (lavapipe) is the reference. On Windows `kittest.toml` `[windows]` renders every snapshot without
  comparing; **never run `UPDATE_SNAPSHOTS` on Windows**, it would overwrite the reference PNGs with the GPU's pixels.
- CI has a `windows` job (clippy, test plugins, tests; no GUI tests, no format check), not blocking while the port is in
  progress.

## Commands

| Command | What it does |
|---|---|
| `scripts/run.sh [args]` | Builds `komponist` (release) and starts it with the arguments (`--demo`, `--null-audio`, `--open-plugin NAME`, `--scale F`). The plugin bridge is part of the executable (`komponist --bridge`, WP-94), so `cargo run -p komponist-app` works as well. |
| `scripts/worktree.sh add\|remove <name> [branch]` | Creates an agent worktree in `../.claude/worktrees/` with its build folders on the NVMe, or removes it with them. |
| `scripts/check.sh` | The gate before every report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. |
| `../.claude/komponist/scripts/render-reference.sh` | (internal, historical) Renders the handoff `.dc.html` files with headless Chromium into `../.claude/komponist/design/reference/*.png` (needs network for the Google fonts). |
| `scripts/build-klopfer.sh` | Optional, not needed to build komponist: bundles a klopfer checkout (`$KLOPFER_DIR`, default `../klopfer`) with `cargo xtask bundle klopfer --release` for the plugin tests that use an installed Klopfer (they skip without it), and prints the bundle paths. |

The `egui` MCP server (`egui-mcp` from `egui_mcp` 0.3.0, matching egui 0.37) is configured in
`.mcp.json`. It attaches to a running app or gallery started with `EGUI_INSPECTION=1`
(127.0.0.1:5719).

## Design references

- **The app is the reference (owner, 2026-11-16).** komponist is ahead of the design handoff; the handoff is
  historical and no longer a requirement. The reference for existing UI is the app itself and its Linux snapshots
  (`crates/*/tests/snapshots/*.png`). New screens and larger UI changes get a proposal with mockups in the internal
  notes (`proposals/<topic>/`) and the owner's approval before implementation.
- `design/tokens.json` (+ `design/device-tokens.json`): all design tokens, the single source of truth, used through
  `komponist-tokens`.
- Historical only (to understand old decisions, never to override the app): the handoff sources
  `../klang-studio-handoff/` (`HANDOFF.md`, `reference/*.dc.html`) and the rendered screens and device handoff copy in
  `../.claude/komponist/design/`. Screen numbers S01 … S10 in code and tests come from there.

## Rules for all agents

1. **Language:** code, comments, docs, commits in English. UI texts come from the catalogs in
   `crates/komponist-i18n/locales/` (`de` is the source, `en` the translation), never as literals in the app, the
   dialogs or the widgets (the gallery is the one exception). New German texts follow the existing catalogs and the
   style guide in `docs/I18N.md`, including typographic characters (U+2212 minus, „…“, ·); the English texts follow the
   style guide and glossary there too. Every new text is added to both languages in the same change. The DAW is called
   **komponist** (lowercase); klang-studio is the umbrella name of the workspace. Built-in device names start with a
   capital: Hexa, Volt, Presse (owner, 2026-11-16).
2. **Tokens:** no hard-coded colours, sizes, radii or font sizes. Always use `komponist-tokens`. Missing
   colour or type token → contract change request. Component metrics (e.g. caret size, menu paddings, swatch size)
   go into `komponist_ui::widgets::metrics`, one constant each with a comment on where the value comes from — never
   inline in widget code. Layout metrics of the app shell or of one view live in that crate's or view's own `metrics`
   module, with sources too.
3. **Design consistency:** new UI follows the existing app and the design system (tokens, the `komponist-ui`
   widgets). The design handoff is historical (see Design references); where it disagrees with the app, the app wins.
   New screens or larger changes start with an approved proposal (mockups in the internal notes).
4. **Realtime:** inside `Engine::process`, `Processor::process` and the bridge RT loop: no allocation
   or freeing, no locks, no logging, no IO, no blocking syscalls except the specified futex wait
   with a deadline. Tests wrap `process` in `assert_no_alloc`.
5. **Scope:** change only the paths your WP owns. Never touch the sibling projects (`../pisynth`,
   `../klopfer`, `../klang-dsp`) unless your WP says so.
6. **Tests first-class:** every WP ships tests. Before reporting, `scripts/check.sh` must pass
   (fmt, clippy `-D warnings`, tests). Run it once at the end, in the background (it waits for the
   shared build lock); iterate with crate-level `cargo check`/`cargo test` before that.
   Tests with real plugins **skip silently** when the test plugins are not built in your worktree: if
   you touch the catalog, plugins, devices, browser or engine, run `scripts/build-test-plugins.sh` once
   before the final `check.sh` (a fresh worktree has none; WP-85 missed a broken test that way).
7. **UI work:** render your component/view to PNG with `egui_kittest`, **look at the PNG** (Read
   tool) and compare it with the existing app (its snapshots) or the approved proposal before you report. Look at the
   English rendering too (`Lang::En`, snapshot name with `-en`): nothing may be cut off or overlap; where English does not fit,
   shorten the English text, not the layout. Changing English never changes a German snapshot. Once a binary shows
   your work (gallery from WP-14, app from WP-30), also check it live: start it with `EGUI_INSPECTION=1` (under Xvfb
   if needed; `--lang de|en` picks the language), attach with the `egui` MCP server, read the AccessKit tree, click
   and type through it and take a screenshot. Parallel agents: use your own port, e.g.
   `EGUI_INSPECTION=127.0.0.1:57xyz`, not the default 5719. Foreign plugin GUIs are invisible to egui: use `xdotool` +
   screenshots (`import -window`) under Xvfb for those.
8. **Dependencies:** use workspace dependencies. A new crate needs one sentence of justification in
   the report and a GPL-3.0-compatible licence (komponist is GPL-3.0-or-later, like everything in
   klang-studio; nothing proprietary or GPL-incompatible such as CDDL or EPL-1.0).
9. **Commits:** small logical commits on your branch, imperative English subject, ending with the
   attribution line your environment specifies.
10. **Report** (final message): summary · changed files · how verified (commands + results) ·
    deviations from plan/design · open questions · contract change requests.
11. **Tools: no devenv in this workspace** (owner's decision). Use what is installed on the system
    (Xvfb, ImageMagick, chromium, fontTools, clap-validator, egui-mcp are). If a tool is missing, run it
    once with `nix shell nixpkgs#<pkg> --command <cmd>` (e.g. xdotool) and name it in the report.
12. **Accessibility is part of the UI contract:** every interactive element has an AccessKit role and
    an accessible name from the catalogs, in both languages (icon-only buttons too, usually the tooltip text). The
    AccessKit root carries the UI language. Canvases (arrangement, piano roll, step grid, mixer) expose AccessKit nodes
    for their **visible** objects (track, clip, note, step, strip) with names such as "Clip Beat A, Takt 1–4" (`de`) or
    "Clip Beat A, bars 1–4" (`en`), "Note C4, 1.1.000". Read the language once before a `widget_info` closure, never
    inside it (`docs/I18N.md`, pitfall). Without these nodes neither kittest queries nor the egui MCP server can find
    anything.

13. **Waiting for background work:** never poll with `pgrep -f "<pattern>"` — the polling shell's own
    command line contains the pattern, so the loop never ends (six such loops ran forever on 2026-11-13).
    Start long commands with `run_in_background` and wait for their completion notification; if you
    must poll, wait on a PID (`while kill -0 $PID; do sleep 3; done`) or use `pgrep -x`.

14. **Hands off the owner's system:** never connect to the owner's real MIDI or audio hardware (tests use virtual
    ports only, e.g. `MidiInputHub::only_connect_to`), never load modules into PipeWire/PulseAudio/JACK or change
    system audio settings, and never write outside the worktree, scratch dirs and the NVMe build folder. **If a tool
    call is blocked, stop and report it — do not retry the same action another way** (2026-11-15).
15. **New features go into the demo:** every user-visible feature (a device, a routing option, an editing mode …) is also
    shown in the demo project `komponist --demo` (`komponist_app::showcase::showcase_project()`), in a musically
    sensible way, in the same WP. Keep the mix balanced (`showcase::render_levels`, ignored test: master peak ≤ −0.3 dBFS,
    limiter ≤ ~2 dB, no silent track) and update the showcase tests (owner, 2026-11-15).
