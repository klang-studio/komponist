#!/usr/bin/env python3
"""Run benchmarks or project acceptance checks against a verified unpacked app archive."""

import argparse
import hashlib
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
    subprocess.run(
        [str(runner), "--ignored", "--nocapture", "--exact", *names],
        env=environment,
        check=True,
    )


def reject_unavailable_bundle(runner, payload, environment, scratch):
    manifest = json.loads((payload / "manifest.json").read_text(encoding="utf-8"))
    eq = next(entry for entry in manifest["bundles"] if entry["id"].endswith(".eq"))
    bundle = payload / eq["path"]
    removed = Path(scratch) / "removed.clap"
    bundle.rename(removed)
    test = "bundled_current::unavailable_bundle_keeps_saved_project_intact"
    run_test(runner, environment, test)
    removed.rename(bundle)
    with (payload / eq["files"][0]["path"]).open("ab") as file:
        file.write(b"modified project test payload")
    run_test(runner, environment, test)


def run_staged(app, runner, digest, projects):
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

        modes = ("direct", "bridge") if projects else ("legacy", "direct", "bridge")
        test_names = {
            None: ("bundled::bundled_clap_workloads",),
            "historical": (
                "legacy_bundled::historical_projects_migrate_and_play",
                "bundled_sampler::both_samplers_play_assets_after_the_project_moves",
            ),
            "current": (
                "bundled_current::current_project_round_trip_and_offline_render",
                "bundled_current::saves_manual_eq_edit_during_automation_and_renders_the_reopened_curve",
                "bundled_synth_ui::packaged_synth_editors_and_factory_presets_follow_clap_state",
                "bundled_sampler::bundled_sampler_panels_edit_and_restore_current_state",
                "bundled_current::packaged_effect_cards_and_presets",
            ),
        }[projects]
        environment = os.environ.copy()
        environment.pop("KOMPONIST_EXPERIMENTAL_DIRECT_CLAP", None)
        environment.pop("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE", None)
        if projects == "historical":
            environment["KOMPONIST_HISTORICAL_BUNDLED_APP"] = str(app)
        elif projects == "current":
            environment["KOMPONIST_CURRENT_BUNDLED_APP"] = str(app)
            environment["KOMPONIST_CURRENT_BUNDLED_RESULTS"] = scratch
        else:
            environment["KOMPONIST_PERF_BUNDLED_APP"] = str(app)

        for mode in modes:
            route = environment.copy()
            if not projects:
                route["KOMPONIST_PERF_BUNDLED_MODE"] = mode
            if mode != "legacy":
                route["KOMPONIST_EXPERIMENTAL_DIRECT_CLAP"] = "1"
            if mode == "bridge":
                route["KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE"] = "1"
            if projects == "current":
                route.setdefault("KOMPONIST_SYNTH_UI_LANG", "en" if mode == "bridge" else "de")
                for test in test_names:
                    run_test(staged_runner, route, test)
            else:
                run_test(staged_runner, route, *test_names)

        if projects == "current":
            environment["KOMPONIST_EXPERIMENTAL_DIRECT_CLAP"] = "1"
            reject_unavailable_bundle(staged_runner, destination, environment, scratch)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="packaged app executable from the unpacked archive")
    projects = parser.add_mutually_exclusive_group()
    projects.add_argument(
        "--historical-projects", dest="projects", action="store_const", const="historical",
        help="validate historical project migration and relocated sampler asset playback",
    )
    projects.add_argument(
        "--current-projects", dest="projects", action="store_const", const="current",
        help="validate current-format project state and offline audio against both host paths",
    )
    options = parser.parse_args()
    app = options.app.resolve(strict=True)
    digest = verified_digest(app)
    test = "it" if options.projects else "engine_perf"
    runner = build_runner(digest, test, release=not options.projects)
    run_staged(app, runner, digest, options.projects)


if __name__ == "__main__":
    main()
