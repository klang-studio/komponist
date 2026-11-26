#!/usr/bin/env python3
"""Run benchmarks or project acceptance checks against a verified unpacked app archive."""

import argparse
import hashlib
from itertools import product
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def payload_directory(app):
    resources = app.parent.parent / "Resources" if sys.platform == "darwin" else app.parent
    return resources / "bundled-clap"


def verified_digest(app):
    result = subprocess.run(
        [str(app), "--verify-bundled-clap"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        encoding="utf-8",
        check=False,
    )
    if result.returncode:
        raise SystemExit(f"Package verification failed:\n{result.stderr}{result.stdout}")
    matches = re.findall(r"^manifest sha256 ([0-9a-f]{64})$", result.stdout, re.MULTILINE)
    if len(matches) != 1:
        raise SystemExit("The packaged app did not report its embedded manifest digest")
    return matches[0]


def build_runner(digest, test, release):
    environment = dict(os.environ, KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256=digest)
    command = [
        "cargo", "test", "--locked", "-p", "komponist-app",
        "--test", test, "--no-run", "--message-format=json-render-diagnostics",
    ]
    if release:
        command.append("--release")
        if sys.platform == "win32" and environment.get("KOMPONIST_PERF_AUDIO_HOST") == "asio":
            command.extend(["--features", "asio"])
    with subprocess.Popen(
        command,
        cwd=ROOT,
        env=environment,
        stdout=subprocess.PIPE,
        encoding="utf-8",
    ) as cargo:
        executable = None
        for line in cargo.stdout:
            message = json.loads(line)
            if message["reason"] == "compiler-message":
                sys.stderr.write(message["message"].get("rendered") or "")
            if (
                message["reason"] == "compiler-artifact"
                and message["target"]["name"] == test
                and message.get("executable")
            ):
                executable = Path(message["executable"])
        if cargo.wait() or executable is None:
            raise SystemExit(f"Could not build the {test} runner")
    return executable


def run_test(runner, environment, *names):
    listing = subprocess.run(
        [str(runner), "--ignored", "--exact", "--list", *names],
        env=environment, capture_output=True, encoding="utf-8", check=True,
    )
    listed = {line.removesuffix(": test") for line in listing.stdout.splitlines()
              if line.endswith(": test")}
    if missing := set(names) - listed:
        raise SystemExit(f"Missing ignored packaged tests: {', '.join(sorted(missing))}")
    subprocess.run(
        [str(runner), "--ignored", "--nocapture", "--exact", *names],
        env=environment,
        check=True,
    )


def run_benchmarks(runner, environment, test):
    workloads = environment.get("KOMPONIST_PERF_WORKLOAD", "instruments,effects,midi").split(",")
    buffers = environment.get("KOMPONIST_PERF_BUFFERS", "64,128,256").split(",")
    workers = environment.get("KOMPONIST_PERF_WORKERS", "0,4").split(",")
    failures = []
    # A previous case can leave allocator pages and loaded libraries resident, so each case runs in a fresh process.
    for workload, frames, threads in product(workloads, buffers, workers):
        workload = workload.strip()
        case = dict(
            environment,
            KOMPONIST_PERF_WORKLOAD=workload,
            KOMPONIST_PERF_BUFFERS=frames,
            KOMPONIST_PERF_WORKERS=threads,
        )
        try:
            run_test(runner, case, test)
        except subprocess.CalledProcessError as error:
            label = f"{workload}/{frames}/{threads}"
            print(f"Benchmark failed: {label}: {error}", file=sys.stderr)
            failures.append(label)
    return failures


def reject_unavailable_bundle(runner, payload, environment, scratch):
    manifest = json.loads((payload / "manifest.json").read_text(encoding="utf-8"))
    eq = next(entry for entry in manifest["bundles"] if entry["id"].endswith(".eq"))
    bundle = payload / eq["path"]
    removed = Path(scratch) / "removed.clap"
    bundle.rename(removed)
    test = "bundled_current::unavailable_bundle_keeps_saved_project_intact"
    run_test(runner, dict(environment, KOMPONIST_CURRENT_EQ_CASE="missing"), test)
    removed.rename(bundle)
    with (payload / eq["files"][0]["path"]).open("ab") as file:
        file.write(b"modified project test payload")
    run_test(runner, dict(environment, KOMPONIST_CURRENT_EQ_CASE="altered"), test)


UI_TESTS = (
    "bundled_current::packaged_effect_cards_and_presets",
    "bundled_synth_ui::packaged_synth_editors_and_factory_presets_follow_clap_state",
)


SLUGS = ("hall", "delay", "eq", "presse", "limiter", "hexa", "volt", "surf",
         "sampler", "general-sampler", "arpeggiator", "scale")


def median(values):
    values = sorted(values)
    middle = len(values) // 2
    return values[middle] if len(values) % 2 else (values[middle - 1] + values[middle]) / 2


def run_startup(runner, environment, repeats):
    """Start-up time and resident memory of the bundled devices, each case in a fresh process."""
    test = "bundled::bundled_clap_startup_and_memory"
    cases = [("all", "all")] + [(slug, slug) for slug in SLUGS]
    table = {}
    for label, selection in cases:
        runs = []
        for _ in range(repeats):
            case = dict(environment, KOMPONIST_PERF_STARTUP_DEVICES=selection)
            result = subprocess.run(
                [str(runner), "--ignored", "--nocapture", "--exact", test],
                env=case, stderr=subprocess.PIPE, stdout=subprocess.DEVNULL,
                encoding="utf-8", check=True,
            )
            fields = {}
            for line in result.stderr.splitlines():
                if line.startswith(("startup:", "device:")):
                    fields.update(re.findall(r"(\w+)=(\S+)", line))
            runs.append({key: float(value) for key, value in fields.items() if value != "n/a"})
        table[label] = {key: median([run[key] for run in runs if key in run])
                        for key in runs[0]}
    everything = table["all"]
    print(f"scan cold {everything['scan_cold_ms']:.0f} ms, warm {everything['scan_warm_ms']:.0f} ms "
          f"(median of {repeats} fresh processes)")
    print(f"all {len(SLUGS)} devices: load {everything['load_total_ms']:.1f} ms, "
          f"resident +{everything.get('rss_delta_mib', float('nan')):.1f} MiB "
          f"(runner before loading: {everything.get('rss_base_mib', float('nan')):.1f} MiB)")
    print("device | load ms (alone, prepare + create) | prepare ms | create ms | resident MiB added (alone)")
    for slug in SLUGS:
        row = table[slug]
        print(f"{slug} | {row['load_ms']:.1f} | {row['prepare_ms']:.1f} | {row['create_ms']:.1f} | "
              f"{row.get('rss_delta_mib', float('nan')):.1f}")


def run_staged(app, runner, digest, projects, repeats=5):
    # Keep the archived app untouched. Stage a separate runner with the same verified payload.
    with tempfile.TemporaryDirectory(prefix="bundled-clap-runner-", dir=runner.parent) as scratch:
        directory = Path(scratch)
        if sys.platform == "darwin":
            directory /= "Runner.app/Contents/MacOS"
            directory.mkdir(parents=True)
        staged_runner = directory / runner.name
        shutil.copy2(runner, staged_runner)
        source = payload_directory(app)
        destination = payload_directory(staged_runner)
        shutil.copytree(source, destination, symlinks=True)
        if hashlib.sha256((destination / "manifest.json").read_bytes()).hexdigest() != digest:
            raise SystemExit("Staged manifest differs from the digest reported by the packaged app")

        test_names = {
            None: ("bundled::bundled_clap_workloads",),
            "current": (
                "bundled_current::demo_and_new_tracks_use_verified_clap_only",
                "bundled_current::current_project_round_trip_and_offline_render",
                "bundled_current::packaged_midi_effects_preserve_instrument_notes",
                "flatten::flatten_processes_verified_clap_scale_and_restores_its_state",
                "bundled_current::bundled_presse_lookahead_updates_delay_compensation_during_playback",
                "bundled_current::saves_manual_eq_edit_during_automation_and_renders_the_reopened_curve",
                "bundled_synth_ui::packaged_synth_editors_and_factory_presets_follow_clap_state",
                "bundled_sampler::both_samplers_play_assets_after_the_project_moves",
                "bundled_sampler::bundled_sampler_restores_saved_parameter_bases",
                "bundled_sampler::general_sampler_file_edits_preserve_slice_history",
                "bundled_sampler::bundled_sampler_panels_edit_and_restore_current_state",
                "bundled_current::packaged_effect_cards_and_presets",
                "param_undo::packaged_limiter_drag_is_one_step_and_undo_redo_restore_the_sound",
            ),
        }.get(projects)
        environment = os.environ.copy()
        if projects == "startup":
            environment["KOMPONIST_PERF_BUNDLED_APP"] = str(app)
            return run_startup(staged_runner, environment, repeats)
        if projects == "current":
            environment["KOMPONIST_CURRENT_BUNDLED_APP"] = str(app)
            environment["KOMPONIST_CURRENT_BUNDLED_RESULTS"] = scratch
        else:
            environment["KOMPONIST_PERF_BUNDLED_APP"] = str(app)

        benchmark_failures = []
        if not projects:
            benchmark_failures.extend(run_benchmarks(staged_runner, environment, test_names[0]))
        else:
            for lang in ("de", "en"):
                # The two UI checks run in both languages; everything else once.
                route = dict(environment, KOMPONIST_SYNTH_UI_LANG=lang)
                for test in test_names:
                    if lang == "en" and test not in UI_TESTS:
                        continue
                    run_test(staged_runner, route, test)

        if projects == "current":
            reject_unavailable_bundle(staged_runner, destination, environment, scratch)
        if benchmark_failures:
            raise SystemExit("Failed benchmark cases: " + ", ".join(benchmark_failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="packaged app executable from the unpacked archive")
    parser.add_argument(
        "--current-projects", dest="projects", action="store_const", const="current",
        help="validate current-format project state and offline audio with the bundled devices in-process",
    )
    parser.add_argument(
        "--startup-memory", dest="projects", action="store_const", const="startup",
        help="measure catalog scan, device load time and resident memory with the bundles in-process",
    )
    parser.add_argument("--repeats", type=int, default=5, help="fresh processes per start-up case")
    options = parser.parse_args()
    app = options.app.resolve(strict=True)
    digest = verified_digest(app)
    test = "it" if options.projects == "current" else "engine_perf"
    runner = build_runner(digest, test, release=options.projects != "current")
    run_staged(app, runner, digest, options.projects, options.repeats)


if __name__ == "__main__":
    main()
