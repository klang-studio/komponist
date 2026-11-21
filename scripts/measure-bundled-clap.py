#!/usr/bin/env python3
"""Run benchmarks or historical project checks against a verified unpacked app archive."""

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


def run_staged(app, runner, digest, historical):
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

        modes = ("direct", "bridge") if historical else ("legacy", "direct", "bridge")
        test_name = (
            "legacy_bundled::historical_projects_migrate_and_play"
            if historical else "bundled::bundled_clap_workloads"
        )
        for mode in modes:
            environment = dict(os.environ)
            if historical:
                environment["KOMPONIST_HISTORICAL_BUNDLED_APP"] = str(app)
            else:
                environment["KOMPONIST_PERF_BUNDLED_APP"] = str(app)
                environment["KOMPONIST_PERF_BUNDLED_MODE"] = mode
            environment.pop("KOMPONIST_EXPERIMENTAL_DIRECT_CLAP", None)
            environment.pop("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE", None)
            if mode != "legacy":
                environment["KOMPONIST_EXPERIMENTAL_DIRECT_CLAP"] = "1"
            if mode == "bridge":
                environment["KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE"] = "1"
            subprocess.run(
                [
                    str(staged_runner), "--ignored", "--nocapture", "--exact",
                    test_name,
                ],
                env=environment,
                check=True,
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="packaged app executable from the unpacked archive")
    parser.add_argument(
        "--historical-projects", action="store_true",
        help="validate migration, persistence and playback of both historical project fixtures",
    )
    options = parser.parse_args()
    app = options.app.resolve(strict=True)
    digest = verified_digest(app)
    test = "it" if options.historical_projects else "engine_perf"
    runner = build_runner(digest, test, release=not options.historical_projects)
    run_staged(app, runner, digest, options.historical_projects)


if __name__ == "__main__":
    main()
