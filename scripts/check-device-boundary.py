"""Ratchet built-in device dependencies and references out of generic host crates."""

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
PROTECTED = {"komponist-app", "komponist-ui", "komponist-plugins", "komponist-engine", "komponist-model"}
DEVICE_DIR = ROOT / "crates" / "builtin-devices"
BASELINE = "scripts/device-boundary-baseline.tsv"


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, encoding="utf-8")


def device_crates(packages):
    """Derive names from every package under builtin-devices, including its CLAP adapters."""
    local_names = {
        package["name"]
        for package in packages.values()
        if Path(package["manifest_path"]).resolve().is_relative_to(DEVICE_DIR)
    }
    return {
        package_id
        for package_id, package in packages.items()
        if package["name"] in local_names or package["name"].startswith("komponist-bundled-")
    }


def forbidden_edges(root, nodes, devices):
    # Include every feature and platform's edges. A crate's dev-dependencies participate
    # when testing that crate, never when using it as a dependency of another crate.
    reachable = {root}
    pending = [root]
    edges = []
    parents = defaultdict(set)
    while pending:
        source = pending.pop()
        for dependency in nodes[source]["deps"]:
            target = dependency["pkg"]
            # A dependency on the root itself cannot create another route to a device.
            if target == root:
                continue
            for detail in dependency["dep_kinds"]:
                kind = detail["kind"] or "normal"
                if kind == "dev" and source != root:
                    continue
                edges.append((source, target, dependency["name"], kind, detail["target"] or "*"))
                parents[target].add(source)
                if target not in reachable:
                    reachable.add(target)
                    pending.append(target)

    # Keep all edges on paths to a device, even if a different path is already allowed.
    reaches_device = reachable & devices
    pending = list(reaches_device)
    while pending:
        target = pending.pop()
        for source in parents[target] - reaches_device:
            reaches_device.add(source)
            pending.append(source)
    return (edge for edge in edges if edge[1] in reaches_device)


RUST_TRIVIA = re.compile(r"\s+|//[^\n]*|/\*")
BLOCK_COMMENT_EDGE = re.compile(r"/\*|\*/")


def followed_by_path_separator(text, position):
    """Check the next Rust token without hiding references inside skipped comments."""
    while match := RUST_TRIVIA.match(text, position):
        position = match.end()
        if match.group() == "/*":
            depth = 1
            for edge in BLOCK_COMMENT_EDGE.finditer(text, position):
                depth += 1 if edge.group() == "/*" else -1
                if depth == 0:
                    position = edge.end()
                    break
            else:
                return False
    return text.startswith("::", position)


def rust_references(crate_dir, names):
    # Match whole identifiers, including crate names in comments and string paths.
    # This is a textual boundary, so it also covers tests, build scripts, and cfg-disabled source.
    names = "|".join(re.escape(name) for name in sorted(names, key=lambda name: (-len(name), name)))
    references = re.compile(
        r"(?P<internal>\bInternal\b)|(?P<kind>\bBuiltinKind\b)|"
        r"(?P<crate>\b(?:builtin[-_]devices|komponist[-_]bundled[-_][\w-]+"
        + (f"|{names}" if names else "")
        + r")\b)"
    )
    for path in crate_dir.rglob("*.rs"):
        relative = path.relative_to(ROOT).as_posix()
        text = path.read_text(encoding="utf-8")
        for match in references.finditer(text):
            if match.lastgroup == "internal" and not followed_by_path_separator(text, match.end()):
                continue
            marker = {"internal": "Internal::", "kind": "BuiltinKind"}.get(match.lastgroup)
            yield "source", relative, marker or match.group()


def inventory(metadata):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = {
        packages[package_id]["name"]: package_id
        for package_id in metadata["workspace_members"]
        if packages[package_id]["name"] in PROTECTED
    }
    if missing := PROTECTED - roots.keys():
        raise ValueError(f"missing protected workspace crates: {', '.join(sorted(missing))}")
    devices = device_crates(packages)
    names = set()
    for package_id in devices:
        package = packages[package_id]
        names.update([package["name"], package["name"].replace("-", "_")])
        names.update(
            target["name"] for target in package["targets"]
            if any(kind in {"lib", "rlib", "dylib", "cdylib", "proc-macro"} for kind in target["kind"])
        )

    violations = Counter()
    for name, root in roots.items():
        for source, target, alias, kind, platform in forbidden_edges(root, nodes, devices):
            violations["dependency", name, packages[source]["name"], packages[target]["name"], alias, kind, platform] += 1
        crate_dir = Path(packages[root]["manifest_path"]).resolve().parent
        violations.update(rust_references(crate_dir, names))
    return violations


def read_baseline(content):
    rows = Counter()
    for line in content.splitlines():
        if not line or line.startswith("#"):
            continue
        count, category, *fields = line.split("\t")
        count = int(count)
        key = (category, *fields)
        if count <= 0 or len(fields) != {"dependency": 6, "source": 2}.get(category):
            raise ValueError(f"invalid boundary baseline row: {line}")
        if key in rows:
            raise ValueError(f"duplicate boundary baseline row: {line}")
        rows[key] = count
    return rows


def previous_baseline(revision):
    commit = run("git", "rev-parse", "--verify", "--end-of-options", revision + "^{commit}").strip()
    # The first PR introducing this gate has no baseline in its base commit.
    if run("git", "ls-tree", "--name-only", commit, "--", BASELINE).strip():
        return read_baseline(run("git", "show", f"{commit}:{BASELINE}"))
    return None


def format_rows(rows):
    for fields, count in sorted(rows.items()):
        yield "\t".join((str(count), *fields))


def report(message, rows):
    if rows:
        print(message, *format_rows(rows), sep="\n", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--print-current", action="store_true", help="print the current inventory as TSV")
    parser.add_argument("--against", metavar="REV", help="require the baseline to be a subset of this revision's")
    args = parser.parse_args()

    violations = inventory(json.loads(run("cargo", "metadata", "--locked", "--all-features", "--format-version", "1")))
    if args.print_current:
        print(
            "# Legacy built-in device boundary violations. Remove rows or reduce counts as code is retired.",
            "# Dependencies: count, category, protected crate, declaring crate, dependency crate, alias, scope, platform",
            "# Source: count, kind, repository-relative Rust file, marker",
            *format_rows(violations),
            sep="\n",
        )
        return 0

    baseline = read_baseline((ROOT / BASELINE).read_text(encoding="utf-8"))
    added = violations - baseline
    retired = baseline - violations
    grown = Counter()
    if args.against and args.against.strip("0"):
        previous = previous_baseline(args.against)
        if previous is not None:
            grown = baseline - previous
    report("New built-in device boundary violations:", added)
    report("Remove these retired violations from the boundary baseline:", retired)
    report("The built-in device boundary baseline can only shrink:", grown)
    if added or retired or grown:
        return 1
    print("Built-in device boundary OK")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"Built-in device boundary check failed: {error}")
