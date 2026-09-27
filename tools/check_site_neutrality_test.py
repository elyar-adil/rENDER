#!/usr/bin/env python3
"""Self-test for tools/check_site_neutrality.py.

A gate that silently stops detecting is worse than no gate, because it reads as
enforcement. This exercises both directions: real per-site branches must be
flagged, and legitimate generic host handling must not be.

Standalone and dependency-free so it can run in CI and before a commit:

    python tools/check_site_neutrality_test.py
"""

from __future__ import annotations

import importlib.util
import pathlib
import sys
import tempfile

GATE = pathlib.Path(__file__).resolve().parent / "check_site_neutrality.py"

spec = importlib.util.spec_from_file_location("check_site_neutrality", GATE)
assert spec and spec.loader
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

# (source line, must_be_flagged, what the case is about)
CASES: list[tuple[str, bool, str]] = [
    # --- must be flagged: the shape of a per-site branch ---
    ('if host == "www.taobao.com" { special(); }', True, "equality on a host"),
    ('if url.contains("163.com") { skip(); }', True, "substring test on a domain"),
    ('    "www.jd.com" => load_jd(),', True, "match arm keyed by a domain"),
    (
        'if url.starts_with("https://www.bilibili.com/") {}',
        True,
        "prefix test on a site URL",
    ),
    (
        'if !request_url.ends_with("qq.com/favicon.ico") { return; }',
        True,
        "suffix test on a site host",
    ),
    # --- evasions the first version of this gate missed ---
    (
        'if host.eq_ignore_ascii_case("WWW.TAOBAO.COM") { special(); }',
        True,
        "uppercase domain defeats a case-sensitive rule",
    ),
    (
        'if url.contains("taobao") { special(); }',
        True,
        "brand token with no TLD",
    ),
    (
        'if title == "jingdong" { special(); }',
        True,
        "brand token in an equality test",
    ),
    (
        'if host == concat!("www.", "taobao", ".com") { special(); }',
        True,
        "identifier split across a concat",
    ),
    (
        'if host == "203.107.1.1" { special(); }',
        True,
        "bare public address in a comparison",
    ),
    (
        'if env::var("RENDER_SITE").as_deref() == Ok("jd") { special(); }',
        True,
        "environment variable that selects a site",
    ),
    (
        'if cfg!(feature = "site-taobao") { special(); }',
        True,
        "build-time site switch",
    ),
    (
        'if host == "www.taobao.com" || host == "127.0.0.1" { special(); }',
        True,
        "a reserved token must not excuse a site identifier on the same line",
    ),
    # --- must not be flagged: generic, correct host handling ---
    (
        'let u = parse("https://www.163.com/news").unwrap();',
        False,
        "a real URL used as inert fixture data",
    ),
    ("let host = url.host_str().to_owned();", False, "reading a host"),
    (
        "if cookie_domain == request_origin { accept(); }",
        False,
        "comparing a cookie domain to the request origin",
    ),
    (
        "if s.starts_with(\"w3.org\") { return Namespace::Xml; }",
        False,
        "namespace URI comparison is required for correct handling",
    ),
    (
        'if h == "localhost" || h == "127.0.0.1" { direct(); }',
        False,
        "loopback is a local test server",
    ),
    (
        'if h == "example.com" { return; }',
        False,
        "RFC 2606 reserved domain",
    ),
    (
        'if peer == "192.0.2.10" { ignore(); }',
        False,
        "RFC 5737 documentation range",
    ),
    (
        'if rel == "apple-touch-icon" { fetch(); }',
        False,
        "apple-touch-icon is a standard link rel value",
    ),
    # --- must not be flagged: the documented suppression ---
    (
        "// site-neutral: example.com is reserved by RFC 2606\nif h == \"qq.com\" {}",
        False,
        "suppression comment on the preceding line",
    ),
    (
        'if h == "soso.com" {} // site-neutral: retired domain in a fixture',
        False,
        "suppression comment on the same line",
    ),
]


def check_case(source: str, expect_flagged: bool) -> list[str]:
    with tempfile.NamedTemporaryFile(
        "w", suffix=".rs", delete=False, encoding="utf-8"
    ) as handle:
        handle.write(source + "\n")
        path = pathlib.Path(handle.name)
    try:
        hits = gate.scan(path)
    finally:
        path.unlink(missing_ok=True)
    problems: list[str] = []
    flagged = bool(hits)
    if flagged != expect_flagged:
        problems.append(
            f"expected flagged={expect_flagged}, got {flagged} for: {source!r}"
        )
    if flagged and not hits[0][2].strip():
        problems.append(f"violation reported without a reason for: {source!r}")
    return problems


def main() -> int:
    failures: list[str] = []
    for source, expect_flagged, description in CASES:
        for problem in check_case(source, expect_flagged):
            failures.append(f"[{description}] {problem}")
        status = "flag" if expect_flagged else "pass"
        print(f"  ok   {description}")

    if failures:
        print(f"\nsite-neutrality self-test: {len(failures)} failure(s)\n")
        for failure in failures:
            print(f"  {failure}")
        return 1

    flagged = sum(1 for _, expected, _ in CASES if expected)
    print(
        f"\nsite-neutrality self-test: {len(CASES)} cases passed "
        f"({flagged} must-flag, {len(CASES) - flagged} must-pass)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
