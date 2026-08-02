"""Compare Rust token-level computed values with current Edge/Chromium."""

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

from render_runtime import computed_html_snapshot


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "interop" / "computed_oracle.html"


def detect_browser(explicit):
    candidates = [
        explicit,
        shutil.which("msedge"),
        shutil.which("google-chrome"),
        shutil.which("chromium"),
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    return next((item for item in candidates if item and Path(item).exists()), None)


def browser_contract(browser):
    with tempfile.TemporaryDirectory(prefix="render-computed-oracle-") as profile:
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
            completed = subprocess.run(command, capture_output=True, text=True, timeout=30)
            output = completed.stdout
        except subprocess.TimeoutExpired as error:
            output = error.stdout or ""
            if isinstance(output, bytes):
                output = output.decode("utf-8", errors="replace")
    match = re.search(r'data-computed-oracle="([^"]+)"', output)
    if not match:
        raise RuntimeError("browser dump did not contain computed-value oracle data")
    return json.loads(unescape(match.group(1), {"&quot;": '"', "&#x27;": "'"}))


def rust_contract(property_names):
    source = FIXTURE.read_text(encoding="utf-8")
    style_match = re.search(r'<style id="oracle-style">(.*?)</style>', source, re.DOTALL)
    if not style_match:
        raise RuntimeError("fixture did not contain oracle stylesheet")
    result = computed_html_snapshot(source, style_match.group(1), "#child")
    if result["stylesheet_diagnostics"]:
        raise RuntimeError(f"Rust stylesheet diagnostics: {result['stylesheet_diagnostics']}")
    properties = result["styles"][0]["properties"]
    return {name: properties.get(name, "").strip() for name in property_names}


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
        print("Computed-value interoperability contract differs")
        return 1
    print("Computed-value interoperability contract matches")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
