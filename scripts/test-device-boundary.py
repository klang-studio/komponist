"""Regression tests for check-device-boundary.py."""

from collections import Counter
import importlib.util
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("device_boundary", Path(__file__).with_name("check-device-boundary.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def dependency(package, *, alias=None, kind=None, target=None):
    return {"pkg": package, "name": alias or package, "dep_kinds": [{"kind": kind, "target": target}]}


class DeviceBoundaryTests(unittest.TestCase):
    def test_records_direct_and_transitive_routes_to_same_device(self):
        nodes = {
            "app": {"deps": [dependency("device"), dependency("facade"), dependency("app", kind="dev")]},
            "facade": {"deps": [dependency("device", alias="renamed", kind="build")]},
            "device": {"deps": [dependency("app")]},
        }
        self.assertCountEqual(
            check.forbidden_edges("app", nodes, {"device"}),
            [
                ("app", "device", "device", "normal", "*"),
                ("app", "facade", "facade", "normal", "*"),
                ("facade", "device", "renamed", "build", "*"),
            ],
        )

    def test_tracks_target_specific_dev_edges_only_for_the_protected_root(self):
        windows = "cfg(windows)"
        nodes = {
            "engine": {"deps": [dependency("fixture", kind="dev", target=windows), dependency("model")]},
            "fixture": {"deps": [dependency("device")]},
            "model": {"deps": [dependency("unused_device", kind="dev")]},
            "device": {"deps": []},
            "unused_device": {"deps": []},
        }
        devices = {"device", "unused_device"}
        self.assertCountEqual(
            check.forbidden_edges("engine", nodes, devices),
            [
                ("engine", "fixture", "fixture", "dev", windows),
                ("fixture", "device", "device", "normal", "*"),
            ],
        )
        self.assertCountEqual(
            check.forbidden_edges("model", nodes, devices),
            [("model", "unused_device", "unused_device", "dev", "*")],
        )

    def test_counts_rust_references_by_file_and_marker(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src.rs").write_text(
                'use komponist_hexa::Macro;\n// Internal :: Foo; Internal::Bar; BuiltinKind\n'
                'const PATH: &str = "../builtin-devices/hexa/patch";\n'
                '// UnrelatedBuiltinKind and komponist_hexadecimal\n',
                encoding="utf-8",
            )
            (root / "test.rs").write_text("// Internal\n ::Test;", encoding="utf-8")
            with patch.object(check, "ROOT", root):
                self.assertEqual(
                    Counter(check.rust_references(root, {"komponist_hexa"})),
                    {
                        ("source", "src.rs", "komponist_hexa"): 1,
                        ("source", "src.rs", "Internal::"): 2,
                        ("source", "src.rs", "BuiltinKind"): 1,
                        ("source", "src.rs", "builtin-devices"): 1,
                        ("source", "test.rs", "Internal::"): 1,
                    },
                )

    def test_baseline_rejects_duplicate_or_nonpositive_rows(self):
        row = "1\tsource\tsrc.rs\tInternal::"
        with self.assertRaisesRegex(ValueError, "duplicate"):
            check.read_baseline(f"{row}\n{row}\n")
        with self.assertRaisesRegex(ValueError, "invalid"):
            check.read_baseline("0" + row[1:])


if __name__ == "__main__":
    unittest.main()
