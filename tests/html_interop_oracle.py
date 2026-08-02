"""Compare Rust HTML tree construction with a current browser oracle.

This is an opt-in interoperability tool rather than a CI gate: specifications
and WPT remain authoritative, while installed Edge/Chromium behavior helps
detect mistakes in reduced real-browser contracts.
"""

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

from render_runtime import parse_html_snapshot


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "interop" / "html_tree_oracle.html"


def detect_browser(explicit: str | None) -> str | None:
    if explicit:
        return explicit
    candidates = [
        shutil.which("msedge"),
        shutil.which("google-chrome"),
        shutil.which("chromium"),
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    return next((candidate for candidate in candidates if candidate and Path(candidate).exists()), None)


def browser_summary(browser: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="render-html-oracle-") as profile:
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
    match = re.search(r'data-oracle="([^"]+)"', output)
    if not match:
        raise RuntimeError("browser dump did not contain the oracle result")
    return json.loads(unescape(match.group(1), {"&quot;": '"', "&#x27;": "'"}))


def descendants(node: dict, local_name: str) -> list[dict]:
    found = [node] if node.get("local_name") == local_name else []
    for child in node["children"]:
        found.extend(descendants(child, local_name))
    return found


def text_content(node: dict) -> str:
    if node["type"] == "text":
        return node["data"]
    return "".join(text_content(child) for child in node["children"])


def rust_summary() -> dict:
    snapshot = parse_html_snapshot(FIXTURE.read_text(encoding="utf-8"))
    document = snapshot["document"]
    html_node = descendants(document, "html")[0]
    body = descendants(html_node, "body")[0]
    table = descendants(body, "table")[0]
    host = next(
        node
        for node in descendants(body, "div")
        if node["attributes"].get("id") == "table-host"
    )
    return {
        "compatMode": "CSS1Compat" if snapshot["quirks_mode"] == "no-quirks" else "BackCompat",
        "title": text_content(descendants(html_node, "title")[0]),
        "htmlLang": html_node["attributes"].get("lang"),
        "bodyId": body["attributes"].get("id"),
        "bodyClass": body["attributes"].get("class"),
        "bodyElements": [
            child["local_name"] for child in body["children"] if child["type"] == "element"
        ],
        "listItems": [text_content(node) for node in descendants(body, "li")],
        "tableChildren": [
            child["local_name"] for child in table["children"] if child["type"] == "element"
        ],
        "rowCells": [text_content(node) for node in descendants(table, "td")],
        "fosteredText": host["children"][0]["data"],
        "textArea": text_content(descendants(body, "textarea")[0]),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--browser", help="Path to Edge/Chrome/Chromium")
    args = parser.parse_args()
    browser = detect_browser(args.browser or os.environ.get("RENDER_BROWSER_CMD"))
    if not browser:
        raise SystemExit("No Edge/Chrome/Chromium executable found")

    expected = browser_summary(browser)
    actual = rust_summary()
    print(json.dumps({"browser": expected, "rust": actual}, ensure_ascii=False, indent=2))
    if actual != expected:
        print("HTML interoperability contract differs")
        return 1
    print("HTML interoperability contract matches")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
