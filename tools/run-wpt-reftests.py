#!/usr/bin/env python3
"""Discover and execute official WPT static reftests through render-core."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


WPT_REVISION = "c7fdee80f3f17b4e9813964916afdfd57ace863f"


class MatchLinkParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.matches: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag.lower() != "link":
            return
        values = {name.lower(): value or "" for name, value in attrs}
        rel = {token.lower() for token in values.get("rel", "").split()}
        href = values.get("href", "")
        if "match" in rel and href:
            self.matches.append(href)


def repository_root() -> Path:
    return Path(__file__).resolve().parents[1]


def default_wpt_root(repo: Path) -> Path:
    return repo.parent / "wpt"


def verify_checkout(root: Path) -> None:
    marker = root / ".render-revision"
    if not marker.is_file():
        raise SystemExit(
            f"WPT checkout is missing at {root}; run `pwsh -File tools/fetch-wpt.ps1` first"
        )
    actual_marker = marker.read_text(encoding="ascii").strip().lower()
    if actual_marker != WPT_REVISION:
        raise SystemExit(
            f"WPT marker is {actual_marker}, expected pinned revision {WPT_REVISION}"
        )
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"WPT checkout is not a Git repository: {root}")
    actual_head = result.stdout.strip().lower()
    if actual_head != WPT_REVISION:
        raise SystemExit(
            f"WPT checkout is at {actual_head}, expected pinned revision {WPT_REVISION}"
        )


def selected_roots(root: Path, suite: str, prefixes: list[str]) -> list[Path]:
    if prefixes:
        paths = [root / Path(prefix) for prefix in prefixes]
    elif suite == "css":
        paths = [root / "css"]
    elif suite == "html":
        paths = [root / "html"]
    else:
        paths = [root]
    missing = [path for path in paths if not path.is_dir()]
    if missing:
        names = ", ".join(str(path) for path in missing)
        raise SystemExit(f"WPT checkout is missing suite directory: {names}")
    return paths


def resolve_reference(root: Path, test_path: Path, href: str) -> Path | None:
    parsed = urlsplit(href)
    if parsed.scheme or parsed.netloc or parsed.query or parsed.fragment:
        return None
    path_text = unquote(parsed.path)
    if not path_text:
        return None
    if path_text.startswith("/"):
        candidate = root / Path(path_text.lstrip("/"))
    else:
        candidate = test_path.parent / Path(path_text)
    try:
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(root.resolve())
    except (FileNotFoundError, OSError, ValueError):
        return None
    return resolved if resolved.is_file() else None


def relative_path(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def discover_cases(roots: list[Path], root: Path) -> tuple[list[tuple[str, str]], int]:
    cases: list[tuple[str, str]] = []
    rejected = 0
    seen: set[tuple[str, str]] = set()
    for suite_root in roots:
        for test_path in sorted(suite_root.rglob("*.html")):
            try:
                source = test_path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                rejected += 1
                continue
            parser = MatchLinkParser()
            try:
                parser.feed(source)
                parser.close()
            except Exception:
                rejected += 1
                continue
            for href in parser.matches:
                reference_path = resolve_reference(root, test_path, href)
                if reference_path is None:
                    rejected += 1
                    continue
                case = (
                    relative_path(root, test_path),
                    relative_path(root, reference_path),
                )
                if case not in seen:
                    seen.add(case)
                    cases.append(case)
    return cases, rejected


def write_manifest(path: Path, cases: list[tuple[str, str]]) -> None:
    path.write_text(
        "# Official WPT rel=match pairs\n"
        + "".join(f"{test}\t{reference}\n" for test, reference in cases),
        encoding="utf-8",
        newline="\n",
    )


def parse_args(repo: Path) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run all discovered official WPT static reftests through render-core."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=default_wpt_root(repo),
        help="external WPT checkout (default: sibling directory ../wpt)",
    )
    parser.add_argument(
        "--suite",
        choices=("all", "css", "html"),
        default="all",
        help="official WPT top-level suite to scan (default: all)",
    )
    parser.add_argument(
        "--path-prefix",
        action="append",
        default=[],
        help="scan a specific checkout-relative directory; may be repeated",
    )
    parser.add_argument(
        "--max-cases",
        type=int,
        help="explicitly cap cases for a smoke run; omitted means all discovered cases",
    )
    parser.add_argument(
        "--cargo",
        default="cargo",
        help="cargo executable used to run the Rust integration test",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="run the Rust integration test in release mode",
    )
    parser.add_argument(
        "--list-only",
        action="store_true",
        help="discover and print cases without invoking cargo",
    )
    return parser.parse_args()


def main() -> int:
    repo = repository_root()
    args = parse_args(repo)
    root = args.root.resolve()
    verify_checkout(root)
    roots = selected_roots(root, args.suite, args.path_prefix)
    cases, rejected = discover_cases(roots, root)
    if args.max_cases is not None:
        if args.max_cases <= 0:
            raise SystemExit("--max-cases must be positive")
        cases = cases[: args.max_cases]
    if not cases:
        raise SystemExit("official WPT discovery found no local rel=match cases")

    print(
        f"WPT_DISCOVERY\trevision={WPT_REVISION}\tcases={len(cases)}\t"
        f"rejected_references={rejected}\tsuites={','.join(str(path) for path in roots)}"
    )
    if args.list_only:
        for test, reference in cases:
            print(f"{test}\t{reference}")
        return 0

    with tempfile.TemporaryDirectory(prefix="render-wpt-") as temporary:
        manifest = Path(temporary) / "manifest.tsv"
        write_manifest(manifest, cases)
        environment = os.environ.copy()
        environment["RENDER_WPT_ROOT"] = str(root)
        environment["RENDER_WPT_MANIFEST"] = str(manifest)
        command = [
            args.cargo,
            "test",
            "-p",
            "render-core",
            "--test",
            "wpt_reftests",
            "official_wpt_reftests",
            "--",
            "--exact",
            "--ignored",
            "--nocapture",
        ]
        if args.release:
            command.insert(2, "--release")
        print("WPT_COMMAND\t" + " ".join(command))
        completed = subprocess.run(command, cwd=repo, env=environment, check=False)
        return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
