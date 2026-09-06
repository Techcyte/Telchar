#!/usr/bin/env python3
"""Set Telchar's release version to a SemVer-compatible calendar version."""

from __future__ import annotations

import argparse
import datetime as dt
import re
from pathlib import Path

CALVER = re.compile(r"^[1-9][0-9]{3}\.([1-9]|1[0-2])\.[0-9]+$")
PACKAGE_MANIFESTS = (
    Path("crates/nix-worker-protocol/Cargo.toml"),
    Path("crates/telchar/Cargo.toml"),
    Path("crates/telchar-nomad-worker/Cargo.toml"),
    Path("crates/telchar-telemetry/Cargo.toml"),
)
WORKSPACE_PACKAGES = ("nix-worker-protocol", "telchar", "telchar-nomad-worker", "telchar-telemetry")
LOCKFILE_PACKAGES = (
    (Path("Cargo.lock"), WORKSPACE_PACKAGES),
    (Path("crates/nix-worker-protocol/fuzz/Cargo.lock"), ("nix-worker-protocol",)),
)


def replace_exact(
    content: str, old: str, new: str, expected_count: int, path: Path
) -> str:
    actual_count = content.count(old)
    if actual_count != expected_count:
        raise SystemExit(
            f"{path}: expected {expected_count} occurrence(s) of {old!r}, found {actual_count}"
        )
    return content.replace(old, new)


def current_version(root: Path) -> str:
    content = (root / "nix/packages.nix").read_text()
    match = re.search(r'^  version = "([^"]+)";$', content, re.MULTILINE)
    if match is None:
        raise SystemExit("nix/packages.nix: package version is missing")
    return match.group(1)


def update_manifest(path: Path, old_version: str, version: str) -> None:
    content = path.read_text()
    content = replace_exact(
        content,
        f'version = "{old_version}"',
        f'version = "{version}"',
        1,
        path,
    )
    path.write_text(content)


def update_lockfile(
    path: Path, old_version: str, version: str, packages: tuple[str, ...]
) -> None:
    content = path.read_text()
    for package in packages:
        package_pattern = re.compile(
            rf'(name = "{re.escape(package)}"\nversion = "){re.escape(old_version)}("\n)'
        )
        content, count = package_pattern.subn(rf"\g<1>{version}\g<2>", content)
        if count != 1:
            raise SystemExit(
                f"{path}: expected one locked package named {package}, found {count}"
            )
    path.write_text(content)


def prepare_release(root: Path, version: str) -> None:
    old_version = current_version(root)
    for relative_path in PACKAGE_MANIFESTS:
        update_manifest(root / relative_path, old_version, version)
    for relative_path, packages in LOCKFILE_PACKAGES:
        update_lockfile(root / relative_path, old_version, version, packages)

    packages_path = root / "nix/packages.nix"
    packages = replace_exact(
        packages_path.read_text(),
        f'  version = "{old_version}";',
        f'  version = "{version}";',
        1,
        packages_path,
    )
    packages_path.write_text(packages)


def next_version(root: Path, today: dt.date) -> str:
    match = CALVER.fullmatch(current_version(root))
    if match is None:
        return f"{today.year}.{today.month}.0"

    try:
        year, month, patch = (int(part) for part in match.group(0).split("."))
    except ValueError as error:
        raise SystemExit(
            "current package version contains an invalid CalVer number"
        ) from error
    if (year, month) == (today.year, today.month):
        return f"{year}.{month}.{patch + 1}"
    return f"{today.year}.{today.month}.0"


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    parser.add_argument("--version")
    return parser.parse_args()


def main() -> None:
    arguments = parse_arguments()
    root = arguments.root.resolve()
    version = arguments.version or next_version(root, dt.date.today())
    if CALVER.fullmatch(version) is None:
        raise SystemExit(
            "release version must match YYYY.M.PATCH with a month from 1 through 12"
        )
    prepare_release(root, version)
    print(version)


if __name__ == "__main__":
    main()
