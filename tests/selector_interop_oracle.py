"""Compare Rust selector matching with current Edge/Chromium."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from xml.sax.saxutils import unescape

from render_runtime import query_html_snapshot


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "interop" / "selector_oracle.html"


def detect_browser(explicit):
    candidates = [
        explicit,
        shutil.which("msedge"),
        shutil.which("google-chrome"),
        shutil.which("chromium"),
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    return next((candidate for candidate in candidates if candidate and Path(candidate).exists()), None)


def browser_contract(browser):
    with tempfile.TemporaryDirectory(prefix="render-selector-oracle-") as profile:
        command = [
            browser,
            "--headless=new",
            "--disable-gpu",
            "--disable-background-networking",
            "--no-first-run",
            f"--user-data-dir={profile}",
            "--dump-dom",
            FIXTURE.resolve().as_uri(),
        ]
        try:
            completed = subprocess.run(command, capture_output=True, text=True, timeout=20)
            output = completed.stdout
        except subprocess.TimeoutExpired as error:
            output = error.stdout or ""
            if isinstance(output, bytes):
                output = output.decode("utf-8", errors="replace")
    match = re.search(r'data-selector-oracle="([^"]+)"', output)
    if not match:
        raise RuntimeError("browser dump did not contain selector oracle data")
    return json.loads(unescape(match.group(1), {"&quot;": '"', "&#x27;": "'"}))


def flatten(node):
    result = {node["id"]: node}
    for child in node["children"]:
        result.update(flatten(child))
    return result


def rust_contract(expected):
    source = FIXTURE.read_text(encoding="utf-8")
    matches = {}
    for selector in expected["matches"]:
        result = query_html_snapshot(source, selector)
        nodes = flatten(result["document"])
        matches[selector] = [
            nodes[node_id].get("attributes", {}).get("id", "")
            for node_id in result["match_ids"]
        ]
    invalid = {}
    for selector in expected["invalid"]:
        try:
            query_html_snapshot(source, selector)
            invalid[selector] = False
        except ValueError:
            invalid[selector] = True
    quirks_source = "<div id='MixedId' class='MixedClass'></div>"
    quirks = {
        "compatMode": "BackCompat",
        "classLower": len(query_html_snapshot(quirks_source, ".mixedclass")["match_ids"]),
        "idLower": len(query_html_snapshot(quirks_source, "#mixedid")["match_ids"]),
    }
    return {"matches": matches, "invalid": invalid, "quirks": quirks}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser")
    args = parser.parse_args()
    browser = detect_browser(args.browser or os.environ.get("RENDER_BROWSER_CMD"))
    if not browser:
        raise SystemExit("No Edge/Chrome/Chromium executable found")
    expected = browser_contract(browser)
    actual = rust_contract(expected)
    print(json.dumps({"browser": expected, "rust": actual}, indent=2, ensure_ascii=False))
    if actual != expected:
        print("Selector interoperability contract differs")
        return 1
    print("Selector interoperability contract matches")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
