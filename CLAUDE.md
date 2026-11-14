# komponist

A MIDI DAW for Linux in Rust (egui/eframe, cpal, CLAP and VST3 hosting in bridge processes).
Architecture: `docs/ARCHITECTURE.md`. Work packages and orchestration: `docs/PLAN.md`.

**Naming:** the DAW is called **komponist**. *klang-studio* is the umbrella name of the workspace
`<workspace>/` (komponist, pisynth, klopfer, klang-dsp, the design handoff). The
design handoff still says "klang-studio" where it means the DAW; read it as "komponist" (rule 1).

## Worktrees

The workspace depends on siblings by relative path (`../klang-dsp`, `../pisynth`, `../klopfer`).
A worktree must therefore sit **exactly one level below `<workspace>/`**. Do **not**
use the Agent tool's `isolation: "worktree"` (it nests the worktree deeper and breaks the paths).
Instead:

```bash
cd <workspace>/komponist
git worktree add ../kp-wt-WP-23 -b wp/WP-23 main     # → <workspace>/kp-wt-WP-23
```

Each worktree builds into its own `target/`. The first build of egui/wgpu takes a few minutes.

## Commands

| Command | What it does |
|---|---|
| `scripts/run.sh [args]` | Builds `komponist` and `komponist-bridge` (release) and starts komponist with the arguments (`--demo`, `--null-audio`, `--open-plugin NAME`, `--scale F`). Plain `cargo run -p komponist-app` does not build the bridge. |
| `scripts/check.sh` | The gate before every report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. |
| `scripts/render-reference.sh` | Renders the handoff `.dc.html` files with headless Chromium into `design/reference/*.png` (needs network for the Google fonts). |
| `scripts/build-klopfer.sh` | Bundles klopfer in `../klopfer` (`cargo xtask bundle klopfer --release`) and prints the paths of `Klopfer.clap` and `Klopfer.vst3` in `../klopfer/target/bundled/`. Does not touch klopfer's submodules. |

The `egui` MCP server (`egui-mcp` from `egui_mcp` 0.3.0, matching egui 0.37) is configured in
`.mcp.json`. It attaches to a running app or gallery started with `EGUI_INSPECTION=1`
(127.0.0.1:5719).

## Design references

- `design/tokens.json`: all design tokens, the single source of truth (copy of the handoff file).
- `design/reference/*.png`, rendered by `scripts/render-reference.sh`:
  - `Screens-01.png` … `Screens-10.png`: screen S01 … S10, each with its notes. The 1920 × 1080
    frame starts at x = 56 (1 px border around it).
  - `Designsystem-01.png` … `-04.png`: tokens and components, cut between sections.
  - `Layout-01.png`: layout 2a with dock (variants 2b/2c are rejected).
  - `Prototyp.png`: start screen of the clickable prototype, 1920 × 1080.
- Sources: `../klang-studio-handoff/` (`HANDOFF.md`, `reference/*.dc.html`, `ks-engine.js`,
  `ks-view.js`). The dc runtime `support.js` lies in `../pisynth/design_handoff/`.

## Rules for all agents

From `docs/PLAN.md` §2. `HS/` = `../klang-studio-handoff/`.

1. **Language:** code, comments, docs, commits in English. UI texts are German and are taken
   **verbatim** from the screens (`HS/reference/Screens.dc.html`), including typographic characters
   (U+2212 minus, „…“, ·). One exception: the DAW is called **komponist**. Where the screens show
   "klang-studio" for the DAW (wordmark, window title, "Rahmen von klang-studio"), write "komponist".
   klang-studio is the umbrella name of the workspace.
2. **Tokens:** no hard-coded colours, sizes, radii or font sizes. Always use `komponist-tokens`. Missing
   colour or type token → contract change request. Component metrics the handoff defines only in the
   design data (e.g. caret size, menu paddings, swatch size) go into `komponist_ui::widgets::metrics`,
   one constant each with its source in the `.dc.html` data — never inline in widget code. Layout metrics
   of the app shell or of one view live in that crate's or view's own `metrics` module, with sources too.
3. **Design sources:** the `.dc.html` files render only with their runtime. Values are in the
   `<script>` blocks (`renderVals()`, `ix`, `notes:ixs([...])`, `headers`, `clips`, `notes`, …), so read
   those, not the markup. Visual reference: `design/reference/*.png`. If a screen and the design
   system disagree, the **design system wins**; report the contradiction.
4. **Realtime:** inside `Engine::process`, `Processor::process` and the bridge RT loop: no allocation
   or freeing, no locks, no logging, no IO, no blocking syscalls except the specified futex wait
   with a deadline. Tests wrap `process` in `assert_no_alloc`.
5. **Scope:** change only the paths your WP owns. Never touch `../pisynth`, `../klopfer` or
   `../klang-dsp` unless your WP says so.
6. **Tests first-class:** every WP ships tests. Before reporting, `scripts/check.sh` must pass
   (fmt, clippy `-D warnings`, tests).
7. **UI work:** render your component/view to PNG with `egui_kittest`, **look at the PNG** (Read
   tool) and compare it with the reference before you report. Once a binary shows your work (gallery
   from WP-14, app from WP-30), also check it live: start it with `EGUI_INSPECTION=1` (under Xvfb if
   needed), attach with the `egui` MCP server, read the AccessKit tree, click and type through it and
   take a screenshot. Parallel agents: use your own port, e.g. `EGUI_INSPECTION=127.0.0.1:57xyz`, not
   the default 5719. Foreign plugin GUIs are invisible to egui: use `xdotool` + screenshots
   (`import -window`) under Xvfb for those.
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
    a German accessible name (icon-only buttons too, usually the tooltip text). Canvases (arrangement,
    piano roll, step grid, mixer) expose AccessKit nodes for their **visible** objects (track, clip,
    note, step, strip) with names such as "Clip Beat A, Takt 1–4" or "Note C4, 1.1.000". Without them
    neither kittest queries nor the egui MCP server can find anything.

13. **Waiting for background work:** never poll with `pgrep -f "<pattern>"` — the polling shell's own
    command line contains the pattern, so the loop never ends (six such loops ran forever on 2026-11-13).
    Start long commands with `run_in_background` and wait for their completion notification; if you
    must poll, wait on a PID (`while kill -0 $PID; do sleep 3; done`) or use `pgrep -x`.