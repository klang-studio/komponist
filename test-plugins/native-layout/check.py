#!/usr/bin/env python3
"""Exercise the C ABI fixture and the v1 descriptor rules with the standalone host."""

import argparse
from copy import deepcopy
from pathlib import Path
import subprocess

from validate import InvalidLayout, MAX_BYTES, UnsupportedLayout, read_json, require, validate

FIXTURE_ID = "org.klang-studio.komponist.layout-fixture"


def run_probe(probe, plugin, plugin_id):
    report = subprocess.run(
        [str(probe), str(plugin), plugin_id],
        check=True,
        stdout=subprocess.PIPE,
    )
    return read_json(report.stdout)


def expect_rejection(error_type, callback, description):
    try:
        callback()
    except error_type:
        return
    raise AssertionError(f"accepted {description}")


def check_descriptors(layout, params, size):
    validate(layout, params, size)
    validate(layout, params, MAX_BYTES)
    for bad_size in (0, MAX_BYTES + 1):
        expect_rejection(InvalidLayout, lambda: validate(layout, params, bad_size),
                         f"invalid byte count {bad_size}")

    first_page = layout["pages"][0]
    bad_pages = [
        {**first_page, "labels": {"de": "Übersicht"}},
        {**first_page, "columns": 1},
        {**first_page, "cells": first_page["cells"] + first_page["cells"]},
        {**first_page, "labels": {"en": "é" * 65}},
        {**first_page, "labels": {"en": "control\tcharacter"}},
    ]
    for page in bad_pages:
        expect_rejection(InvalidLayout, lambda: validate({**layout, "pages": [page]}, params, size),
                         "malformed page")
    ascii_locale = {**layout, "locales": [*layout["locales"], "kk"]}
    for invalid_locale in (
        {**ascii_locale, "default_locale": "KK"},
        {**ascii_locale, "pages": [{**first_page, "labels": {"en": "Main", "KK": "Main"}}]},
    ):
        expect_rejection(InvalidLayout, lambda: validate(invalid_locale, params, size),
                         "non-ASCII locale alias")
    for flags in (1 << 2, 1 << 3, 1 << 4):
        mutated = [{**param, "flags": flags} if param["id"] == 42 else param for param in params]
        expect_rejection(InvalidLayout, lambda: validate(layout, mutated, size),
                         f"parameter flags {flags}")

    bad_enum = deepcopy(layout)
    bad_enum["pages"][0]["cells"][1]["choices"][2]["value"] = 3
    expect_rejection(InvalidLayout, lambda: validate(bad_enum, params, size), "incorrect enum range")
    for override in ({"schema_version": 2}, {"required_capabilities": ["custom-widget"]}):
        expect_rejection(UnsupportedLayout, lambda: validate({**layout, **override}, params, size),
                         "unsupported v1 descriptor")

    for raw in (b'{"pages":[],"pages":[]}', b'{"labels":"\xff"}', b'{"param_id":NaN}'):
        expect_rejection((InvalidLayout, UnicodeDecodeError), lambda: read_json(raw), "invalid JSON")


def check_nice_plug(probe, artifacts):
    plugin_id = "org.klang-studio.komponist.nice-layout-prototype"
    layouts = []
    for variant in ("baseline", "rebuild", "reordered"):
        plugin = artifacts / variant / "nice-layout-prototype.clap"
        report = run_probe(probe, plugin, plugin_id)
        validate(report["layout"], report["params"], report["size"])
        ids = {param["name"]: param["id"] for param in report["params"]}
        require(ids == {"Gain": 3165055, "Mix": 108124}, f"unexpected IDs in {variant}")
        layouts.append(report["layout"])
    require(all(layout == layouts[0] for layout in layouts), "layout changed across builds")
    plugin = artifacts / "collision" / "nice-layout-prototype.clap"
    report = run_probe(probe, plugin, plugin_id)
    require(report["layout"] is None and report["status"] == 1 and report["size"] == 0,
            "independent C host did not observe ABSENT for colliding IDs")
    print("C host verified stable nice-plug IDs and collision fallback")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("probe", type=Path)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("example", type=Path)
    parser.add_argument("--nice-plug-artifacts", type=Path)
    args = parser.parse_args()
    probe, fixture = args.probe.resolve(), args.fixture.resolve()
    subprocess.run([str(probe), "--self-test", str(fixture)], check=True)

    def variant(name):
        return run_probe(probe, fixture, f"{FIXTURE_ID}.{name}")

    valid = variant("valid")
    require(valid["layout"] == read_json(args.example.read_bytes()),
            "wire payload differs from the normative example")
    check_descriptors(valid["layout"], valid["params"], valid["size"])

    optional = variant("optional-field")
    require(optional["layout"] != valid["layout"], "optional-field fixture has no new data")
    validate(optional["layout"], optional["params"], optional["size"])
    for name, error_type in [("unknown-param", InvalidLayout),
                             ("required-capability", UnsupportedLayout)]:
        report = variant(name)
        require(isinstance(report["layout"], dict), f"{name} failed before semantic validation")
        expect_rejection(error_type,
                         lambda: validate(report["layout"], report["params"], report["size"]),
                         name)
    print("Layout v1 semantic checks passed")
    if args.nice_plug_artifacts:
        check_nice_plug(probe, args.nice_plug_artifacts.resolve())


if __name__ == "__main__":
    main()
