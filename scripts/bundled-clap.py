#!/usr/bin/env python3
"""Stage bundled CLAP payloads and smoke-test the exact files in an app archive."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
from typing import NamedTuple

REPO = Path(__file__).resolve().parent.parent
SDK = REPO / "crates" / "plugin-sdk"
MANIFEST = "manifest.json"
LICENSES = {
    "LICENSE": REPO / "LICENSE",
    "LICENSE-nice-plug": SDK / "licenses" / "nice-plug.txt",
}
TARGET_LIB_SUFFIX = {
    "x86_64-unknown-linux-gnu": ".so",
    "aarch64-unknown-linux-gnu": ".so",
    "aarch64-apple-darwin": ".dylib",
    "x86_64-pc-windows-msvc": ".dll",
}


class Device(NamedTuple):
    package: str
    library: str
    plugin_id: str
    # Cargo's cdylib name when it differs from `library` (the CLAP bundle and macOS executable name).
    # Device crates that reuse their device crate's lib name carry a `_clap` suffix there (#234).
    cdylib: str = ""

    @property
    def artifact(self):
        return self.cdylib or self.library


# The fixture can exercise the package workflow before the first device port.
# Use the Hall inventory for the first real-device package on all targets.
INVENTORIES = {
    "fixture": (
        Device(
            "native-clap-example",
            "native_clap_example",
            "org.klang-studio.komponist.native-clap-example",
        ),
    ),
    "hall": (
        Device(
            "komponist-bundled-hall",
            "komponist_hall",
            "org.klang-studio.komponist.hall",
        ),
    ),
}

LIMITER = Device(
    "komponist-bundled-limiter",
    "komponist_limiter",
    "org.klang-studio.komponist.limiter",
    "komponist_limiter_clap",
)
INVENTORIES["effects"] = INVENTORIES["hall"] + tuple(
    Device(
        f"komponist-bundled-{name}",
        f"komponist_{name}",
        f"org.klang-studio.komponist.{name}",
        f"komponist_{name}_clap",
    )
    for name in ("delay", "eq", "presse")
) + (LIMITER,)

KLOPFER = Device(
    "komponist-bundled-sampler",
    "komponist_sampler",
    "org.klang-studio.komponist.sampler",
    "komponist_sampler_clap",
)
GENERAL_SAMPLER = Device(
    "komponist-bundled-general-sampler",
    "komponist_general_sampler",
    "org.klang-studio.komponist.general-sampler",
    "komponist_general_sampler_clap",
)

INVENTORIES["all"] = INVENTORIES["effects"] + (
    Device("komponist-bundled-hexa", "komponist_hexa_clap", "org.klang-studio.komponist.hexa"),
    Device("komponist-bundled-volt", "komponist_volt", "org.klang-studio.komponist.volt", "komponist_volt_clap"),
    Device("komponist-bundled-surf", "komponist_surf_clap", "org.klang-studio.komponist.surf"),
    KLOPFER,
    GENERAL_SAMPLER,
    Device("komponist-bundled-arpeggiator", "komponist_arpeggiator_clap", "org.klang-studio.komponist.arpeggiator"),
    Device("komponist-bundled-scale", "komponist_scale_clap", "org.klang-studio.komponist.scale"),
)
INVENTORIES["native-fixtures"] = (KLOPFER, GENERAL_SAMPLER, LIMITER)


def bundle_name(device):
    return f"{device.library}.clap"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def build(target, devices):
    workspace = SDK if devices == INVENTORIES["fixture"] else REPO
    manifest = workspace / "Cargo.toml"
    command = [
        os.environ.get("CARGO", "cargo"),
        "build",
        "--locked",
        "--manifest-path",
        manifest,
        "--release",
        "--target",
        target,
        "--message-format=json-render-diagnostics",
    ]
    for device in devices:
        command += ["--package", device.package]

    if devices == INVENTORIES["native-fixtures"]:
        command += ["--features", ",".join(
            f"{device.package}/fixture-without-parameter-display"
            for device in (KLOPFER, GENERAL_SAMPLER)
        )]

    suffix = TARGET_LIB_SUFFIX[target]
    names = {device.artifact for device in devices}
    artifacts = {}
    with subprocess.Popen(command, cwd=workspace, stdout=subprocess.PIPE, text=True, encoding="utf-8") as child:
        for line in child.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") == "compiler-message":
                sys.stderr.write(message["message"].get("rendered") or "")
            if message.get("reason") != "compiler-artifact":
                continue
            artifact = message["target"]
            stem = artifact["name"]
            if stem not in names or "cdylib" not in artifact["crate_types"]:
                continue
            libraries = [
                Path(path) for path in message["filenames"]
                if Path(path).suffix == suffix
            ]
            if len(libraries) == 1:
                artifacts[stem] = libraries[0]
                # Cargo package IDs are opaque. Read the version from Cargo metadata below.
        if child.wait() != 0:
            raise ValueError("bundled CLAP build failed")

    for device in devices:
        if device.artifact not in artifacts:
            raise ValueError(f"Cargo did not report a unique dynamic library for {device.package}")

    if target != "aarch64-apple-darwin":
        return artifacts, {}
    result = subprocess.run(
        [os.environ.get("CARGO", "cargo"), "metadata", "--locked", "--no-deps",
         "--format-version", "1", "--manifest-path", manifest],
        cwd=workspace,
        check=True,
        capture_output=True,
    )
    return artifacts, {
        package["name"]: package["version"]
        for package in json.loads(result.stdout)["packages"]
    }


def stage(target, root, devices):
    if root.exists() or root.is_symlink():
        raise ValueError(f"staging directory already exists: {root}")
    artifacts, versions = build(target, devices)
    root.mkdir(parents=True)
    for name, source in LICENSES.items():
        shutil.copyfile(source, root / name)

    for device in devices:
        bundle = root / bundle_name(device)
        artifact = artifacts[device.artifact]
        if target == "aarch64-apple-darwin":
            contents = bundle / "Contents"
            executable = contents / "MacOS" / device.library
            executable.parent.mkdir(parents=True)
            shutil.copyfile(artifact, executable)
            executable.chmod(0o755)
            version = versions[device.package]
            with (contents / "Info.plist").open("wb") as file:
                plistlib.dump(
                    {
                        "CFBundleExecutable": device.library,
                        "CFBundleIdentifier": device.plugin_id,
                        "CFBundleInfoDictionaryVersion": "6.0",
                        "CFBundleName": device.library,
                        "CFBundlePackageType": "BNDL",
                        "CFBundleShortVersionString": version,
                        "CFBundleVersion": version,
                    },
                    file,
                )
            identity = os.environ.get("KOMPONIST_SIGN_IDENTITY")
            timestamp = "--timestamp" if identity else "--timestamp=none"
            subprocess.run(
                ["codesign", "--force", "--options", "runtime", timestamp, "--sign",
                 identity or "-", bundle],
                check=True,
                stdout=sys.stderr,
            )
        else:
            shutil.copyfile(artifact, bundle)
    # Hash the final nested bundles, after any code signing, before building the app.
    (root / MANIFEST).write_bytes(manifest_bytes(target, root, devices))
    print(sha256(root / MANIFEST))


def file_mode(path):
    info = path.lstat()
    if (
        stat.S_ISLNK(info.st_mode)
        or getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
    ):
        raise ValueError(f"unexpected symlink or reparse point in package: {path}")
    return info.st_mode


def files_in(path):
    mode = file_mode(path)
    if stat.S_ISREG(mode):
        return [path]
    if stat.S_ISDIR(mode):
        result = []
        for entry in sorted(path.iterdir()):
            result.extend(files_in(entry))
        return result
    raise ValueError(f"unexpected file type in package: {path}")


def manifest_bytes(target, root, devices):
    if not stat.S_ISDIR(file_mode(root)):
        raise ValueError(f"expected payload directory: {root}")
    bundle_names = {bundle_name(device) for device in devices}
    present = {entry.name for entry in root.iterdir()}
    if present - {MANIFEST} != bundle_names | LICENSES.keys():
        raise ValueError(f"unexpected payload inventory in {root}: {sorted(present)}")
    for name, license_path in LICENSES.items():
        license_file = root / name
        if not stat.S_ISREG(file_mode(license_file)) or sha256(license_file) != sha256(license_path):
            raise ValueError(f"license differs from source: {license_file}")
    bundles = []
    for device in devices:
        bundle = root / bundle_name(device)
        mode = file_mode(bundle)
        if target == "aarch64-apple-darwin":
            if not stat.S_ISDIR(mode):
                raise ValueError(f"expected macOS CLAP bundle directory: {bundle}")
            for payload in (
                bundle / "Contents" / "Info.plist",
                bundle / "Contents" / "MacOS" / device.library,
            ):
                if not stat.S_ISREG(file_mode(payload)):
                    raise ValueError(f"missing required bundle file: {payload}")
        elif not stat.S_ISREG(mode):
            raise ValueError(f"expected flat CLAP bundle: {bundle}")
        bundles.append({
            "id": device.plugin_id,
            "path": bundle.name,
            "files": [
                {"path": path.relative_to(root).as_posix(), "sha256": sha256(path)}
                for path in files_in(bundle)
            ],
        })

    manifest = {"schema_version": 1, "target": target, "bundles": bundles}
    return (json.dumps(manifest, indent=2) + "\n").encode("utf-8")


def payload_directory(target, app):
    directory = app.parent
    if target == "aarch64-apple-darwin":
        directory = directory.parent / "Resources"
    return directory / "bundled-clap"


def check_rejects_modified_package(target, app, device):
    # Run the same executable from a disposable package. On macOS ditto keeps
    # the app and nested bundle signatures as they were archived.
    with tempfile.TemporaryDirectory(prefix="komponist integrity ") as temporary:
        directory = Path(temporary)
        if target == "aarch64-apple-darwin":
            source_app = app.parents[2]
            copied_app = directory / source_app.name
            subprocess.run(["ditto", source_app, copied_app], check=True)
            executable = copied_app / app.relative_to(source_app)
        else:
            executable = directory / app.name
            shutil.copy2(app, executable)
            shutil.copytree(payload_directory(target, app), payload_directory(target, executable))

        bundle = payload_directory(target, executable) / bundle_name(device)
        library = bundle
        if target == "aarch64-apple-darwin":
            library = library / "Contents" / "MacOS" / device.library
        with library.open("ab") as file:
            file.write(b"altered bundled CLAP smoke payload")
        check_integrity_refusal(executable, device, "altered", "bundle file hash mismatch")

        bundle.rename(directory / "removed.clap")
        check_integrity_refusal(executable, device, "missing", bundle.name)


def check_integrity_refusal(executable, device, problem, expected_detail):
    # Capture both handles to preserve GUI-subsystem stdout/stderr on Windows.
    refusal = subprocess.run(
        [executable, "--verify-bundled-clap"],
        capture_output=True,
        timeout=60,
    )
    output = refusal.stdout.decode("utf-8", errors="replace")
    diagnostic = refusal.stderr.decode("utf-8", errors="replace")
    if (
        refusal.returncode == 0
        or f"verified {device.plugin_id} (in-process)" in output.splitlines()
        or "bundled CLAP verification:" not in diagnostic
        or expected_detail not in diagnostic
        or device.plugin_id not in diagnostic
    ):
        raise ValueError(
            f"expected app integrity refusal for {problem} bundle ({refusal.returncode}): "
            f"{output}{diagnostic}"
        )
    print(f"rejected {problem} bundle {device.plugin_id}")


def check_archive(target, app, expected_sha256, devices):
    app = app.resolve(strict=True)
    if not app.is_file():
        raise ValueError(f"expected unpacked app executable: {app}")
    root = payload_directory(target, app)
    manifest = root / MANIFEST
    if not stat.S_ISREG(file_mode(manifest)) or sha256(manifest) != expected_sha256:
        raise ValueError("unpacked bundle manifest digest differs from the build")
    if manifest.read_bytes() != manifest_bytes(target, root, devices):
        raise ValueError("unpacked bundle inventory or payload differs from the manifest")
    for device in devices:
        bundle = root / bundle_name(device)
        if target == "aarch64-apple-darwin":
            subprocess.run(["codesign", "--verify", "--strict", bundle], check=True)
        scan = subprocess.run(
            [app, "--bridge", "--scan", bundle],
            check=True,
            stdout=subprocess.PIPE,
            timeout=30,
        )
        ids = [descriptor["id"] for descriptor in json.loads(scan.stdout)]
        if ids != [device.plugin_id]:
            raise ValueError(f"unexpected IDs for {bundle.name}: {ids}")

    # Keep stdout attached to a pipe on Windows so a GUI-subsystem binary
    # finishes before the parent checks its exit code.
    for route, mode in (("in-process", []), ("bridge", ["--bridge"])):
        smoke = subprocess.run(
            [app, "--verify-bundled-clap", *mode],
            check=True,
            stdout=subprocess.PIPE,
            timeout=60,
        )
        stdout = smoke.stdout.decode("utf-8")
        print(stdout, end="")
        reports = [line for line in stdout.splitlines() if line.startswith("verified ")]
        expected = [f"verified {device.plugin_id} ({route})" for device in devices]
        hall_id = INVENTORIES["hall"][0].plugin_id
        if any(device.plugin_id == hall_id for device in devices):
            lifecycle = "direct lifecycle" if route == "in-process" else "forced bridge"
            expected.append(f"verified {hall_id} (normal {lifecycle})")
        if sorted(reports) != sorted(expected):
            raise ValueError(f"unexpected {route} verification reports: {reports}")

    check_rejects_modified_package(target, app, devices[0])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGET_LIB_SUFFIX, required=True)
    parser.add_argument("--inventory", choices=INVENTORIES, required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    stage_parser = commands.add_parser("stage")
    stage_parser.add_argument("directory", type=Path)
    verify_parser = commands.add_parser("check-archive")
    verify_parser.add_argument("app", type=Path)
    verify_parser.add_argument("--manifest-sha256", required=True)
    options = parser.parse_args()
    devices = INVENTORIES[options.inventory]
    if options.command == "stage":
        stage(options.target, options.directory, devices)
    else:
        check_archive(options.target, options.app, options.manifest_sha256, devices)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f"bundled CLAP: {error}")
