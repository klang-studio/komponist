#!/usr/bin/env python3
"""Run engine_perf against the verified CLAP payload of an unpacked app archive."""

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


def build_runner(digest):
    environment = dict(os.environ, KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256=digest)
    with subprocess.Popen(
        [
            "cargo", "test", "--locked", "--release", "-p", "komponist-app",
            "--test", "engine_perf", "--no-run", "--message-format=json-render-diagnostics",
        ],
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
                and message["target"]["name"] == "engine_perf"
                and message.get("executable")
            ):
                executable = Path(message["executable"])
        if cargo.wait() or executable is None:
            raise SystemExit("Could not build the engine_perf runner")
    return executable


def measure(app, runner, digest):
    # Keep the archived app untouched. Stage a separate runner with the same verified payload.
    with tempfile.TemporaryDirectory(prefix="bundled-clap-perf-", dir=runner.parent) as scratch:
        directory = Path(scratch)
        if sys.platform == "darwin":
            directory /= "Perf.app/Contents/MacOS"
            directory.mkdir(parents=True)
        staged_runner = directory / runner.name
        shutil.copy2(runner, staged_runner)
        source = payload_directory(app)
        destination = payload_directory(staged_runner)
        shutil.copytree(source, destination, symlinks=True)
        if hashlib.sha256((destination / "manifest.json").read_bytes()).hexdigest() != digest:
            raise SystemExit("Staged manifest differs from the digest reported by the packaged app")

        for mode in ("legacy", "direct", "bridge"):
            environment = dict(
                os.environ,
                KOMPONIST_PERF_BUNDLED_APP=str(app),
                KOMPONIST_PERF_BUNDLED_MODE=mode,
            )
            environment.pop("KOMPONIST_EXPERIMENTAL_DIRECT_CLAP", None)
            environment.pop("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE", None)
            if mode != "legacy":
                environment["KOMPONIST_EXPERIMENTAL_DIRECT_CLAP"] = "1"
            if mode == "bridge":
                environment["KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE"] = "1"
            subprocess.run(
                [
                    str(staged_runner), "--ignored", "--nocapture", "--exact",
                    "bundled::bundled_clap_workloads",
                ],
                env=environment,
                check=True,
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="packaged app executable from the unpacked archive")
    app = parser.parse_args().app.resolve(strict=True)
    digest = verified_digest(app)
    measure(app, build_runner(digest), digest)


if __name__ == "__main__":
    main()
