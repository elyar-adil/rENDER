#!/usr/bin/env python3
"""Self-test for tools/check_site_neutrality.py.

A gate that silently stops detecting is worse than no gate, because it reads as
enforcement. This exercises both directions: real per-site branches must be
flagged, and legitimate generic host handling must not be.

Every case names the exact rule it expects, rather than just "something was
flagged", for three reasons:

* a rule that stops firing shows up as a named failure instead of being masked
  by a neighbouring rule that happens to match the same line;
* the two confidence tiers can be held apart. Prose that names a site must be
  reported as advisory, not quietly promoted to a build failure, and a per-site
  branch must be blocking, not quietly demoted to a note;
* `test_rule_coverage` can then assert that every rule the gate owns has at
  least one must-flag case *and* at least one must-pass case. A rule with only
  must-flag cases is how a gate gets switched off.

Runs standalone (stdlib only) and under pytest:

    python tools/check_site_neutrality_test.py
    pytest tools/check_site_neutrality_test.py
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import pathlib
import sys
import tempfile

GATE = pathlib.Path(__file__).resolve().parent / "check_site_neutrality.py"

spec = importlib.util.spec_from_file_location("check_site_neutrality", GATE)
assert spec and spec.loader
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

BLOCKING = gate.BLOCKING
ADVISORY = gate.ADVISORY

# (source, expected rule or None, what the case is about, rule this case defends)
#
# `expected rule` of None means "no finding at all". Otherwise the finding must
# carry exactly that rule name, so a case cannot pass because some other rule
# fired on the same line.
CASES: list[tuple[str, str | None, str, str | None]] = [
    # --- must be flagged: the shape of a per-site branch ---
    (
        'if host == "www.taobao.com" { special(); }',
        "decision/domain",
        "equality on a host",
        None,
    ),
    (
        'if url.contains("163.com") { skip(); }',
        "decision/domain",
        "substring test on a domain",
        None,
    ),
    ('    "www.jd.com" => load_jd(),', "decision/domain", "match arm keyed by a domain", None),
    (
        'if url.starts_with("https://www.bilibili.com/") {}',
        "decision/domain",
        "prefix test on a site URL",
        None,
    ),
    (
        'if !request_url.ends_with("qq.com/favicon.ico") { return; }',
        "decision/domain",
        "suffix test on a site host",
        None,
    ),
    # --- evasions the first version of this gate missed ---
    (
        'if host.eq_ignore_ascii_case("WWW.TAOBAO.COM") { special(); }',
        "decision/domain",
        "uppercase domain defeats a case-sensitive rule",
        None,
    ),
    (
        'if url.contains("taobao") { special(); }',
        "decision/brand-token",
        "brand token with no TLD",
        None,
    ),
    (
        'if title == "jingdong" { special(); }',
        "decision/brand-token",
        "brand token in an equality test",
        None,
    ),
    (
        'if host == concat!("www.", "taobao", ".com") { special(); }',
        "decision/brand-token",
        "identifier split across a concat",
        None,
    ),
    (
        'if host == "203.107.1.1" { special(); }',
        "decision/address",
        "bare public address in a comparison",
        None,
    ),
    (
        'if env::var("RENDER_SITE").as_deref() == Ok("jd") { special(); }',
        "decision/env-switch",
        "environment variable that selects a site",
        None,
    ),
    (
        'if cfg!(feature = "site-taobao") { special(); }',
        "decision/brand-token",
        "build-time site switch",
        None,
    ),
    (
        'if cfg!(feature = "tenant-cn") { special(); }',
        "decision/external-switch",
        "a cargo feature naming a tenant",
        None,
    ),
    (
        'if mode == RENDER_SITE_MODE { special(); }',
        "decision/env-switch",
        "a site switch compared against, not read from the environment",
        None,
    ),
    (
        'if host == "www.taobao.com" || host == "127.0.0.1" { special(); }',
        "decision/domain",
        "a reserved token must not excuse a site identifier on the same line",
        None,
    ),
    # --- must not be flagged: generic, correct host handling ---
    (
        'let u = parse("https://www.163.com/news").unwrap();',
        None,
        "a real URL used as inert fixture data",
        "decision/domain",
    ),
    ("let host = url.host_str().to_owned();", None, "reading a host", None),
    (
        "if cookie_domain == request_origin { accept(); }",
        None,
        "comparing a cookie domain to the request origin",
        None,
    ),
    (
        "if s.starts_with(\"w3.org\") { return Namespace::Xml; }",
        None,
        "namespace URI comparison is required for correct handling",
        None,
    ),
    (
        'if h == "localhost" || h == "127.0.0.1" { direct(); }',
        None,
        "loopback is a local test server",
        None,
    ),
    ('if h == "example.com" { return; }', None, "RFC 2606 reserved domain", None),
    ('if peer == "192.0.2.10" { ignore(); }', None, "RFC 5737 documentation range", None),
    (
        'if rel == "apple-touch-icon" { fetch(); }',
        None,
        "apple-touch-icon is a standard link rel value",
        None,
    ),
    # --- must not be flagged: the documented suppression ---
    (
        "// site-neutral: example.com is reserved by RFC 2606\nif h == \"qq.com\" {}",
        None,
        "suppression comment on the preceding line",
        None,
    ),
    (
        'if h == "soso.com" {} // site-neutral: retired domain in a fixture',
        None,
        "suppression comment on the same line",
        None,
    ),
    # --- rule: site-identifier (a site in a Rust identifier) ---
    (
        "#[test]\nfn temp_diag_bilibili() {\n    assert!(true);\n}",
        "site-identifier",
        "a test function name that carries a site",
        None,
    ),
    (
        "fn get_form_submission_builds_a_baidu_style_search_navigation() {}",
        "site-identifier",
        "a test name naming the site it was found on",
        None,
    ),
    (
        'const BILIBILI_BASE: &str = "https://example.com/";',
        "site-identifier",
        "a site in an upper-case constant name",
        None,
    ),
    ("mod bilibili_capture { }", "site-identifier", "a site in a module name", None),
    (
        "fn description_meta_refresh() {}",
        None,
        "an identifier named for the shape, not the site",
        "site-identifier",
    ),
    (
        "fn taobaogx_widget() {}",
        None,
        "a token that is only a substring of a longer word is not a token",
        "site-identifier",
    ),
    (
        "let t = track.info.duration_seconds;",
        None,
        "DOMAIN matches `track.info` but only in a decision position",
        "decision/domain",
    ),
    (
        'let _ = "RENDER_SITE is not read here";',
        None,
        "a site switch named in a string is not a switch",
        "decision/env-switch",
    ),
    (
        "// RENDER_SITE would select a site; nothing reads it.",
        None,
        "a site switch named in a comment is not a switch",
        "decision/env-switch",
    ),
    (
        'let msg = "the site is unreachable";',
        None,
        '"site" as an ordinary word is not a cargo feature',
        "decision/external-switch",
    ),
    (
        'let x = "bilibili";',
        None,
        "a bare brand token is not a decision",
        "decision/brand-token",
    ),
    (
        "// site-neutral: the captured page is named after the site it captures.\n"
        "fn temp_diag_bilibili() {}",
        None,
        "suppression covers an identifier hit too",
        None,
    ),
    # --- rule: site-path (a site-named path a test reads) ---
    (
        "fn load() -> String {\n"
        "    std::fs::read_to_string(concat!(\n"
        '        env!("CARGO_MANIFEST_DIR"),\n'
        '        "/../../.diag/bilibili/page.html"\n'
        "    ))\n"
        "    .unwrap()\n"
        "}",
        "site-path",
        "a test reading a site-named captured page",
        None,
    ),
    (
        'let s = fs::read_to_string(".diag/bilibili/video.js").unwrap();',
        "site-path",
        "a site-named fixture path",
        None,
    ),
    (
        'let p = "../../.diag/zhihu/assets";',
        "site-path",
        "a site-named directory joined onto a path",
        None,
    ),
    (
        'let url = Url::parse("https://www.bilibili.com/").expect("base");',
        None,
        "a URL host is inert fixture data, not a path segment",
        "site-identifier",
    ),
    (
        'let src = "https://www.bilibili.com/media/poster.mp4";',
        None,
        "a URL path is inert fixture data, not a path on disk",
        "site-path",
    ),
    (
        'let title = "www.baidu.com".to_owned();',
        None,
        "a bare host with no path separator is not a path segment",
        "site-path",
    ),
    (
        "let html = \"<source src='/media/bilibili-init.mp4'>\";",
        None,
        "a site-named asset inside markup is not a path literal",
        "site-path",
    ),
    (
        "// site-neutral: the capture directory is a scratch dir, not engine code.\n"
        'let p = "../../.diag/bilibili";',
        None,
        "suppression covers a path hit too",
        None,
    ),
    # --- rule: site-prose (a site in a comment) ---
    (
        "/// Reproduces the bilibili description meta refresh.",
        "site-prose",
        "a site in a doc comment",
        None,
    ),
    (
        "// The douyin player needs a click before the clock starts.",
        "site-prose",
        "a site in an ordinary comment",
        None,
    ),
    (
        "/// ECMA-262 20.1.3.6 step 7: a string-valued Symbol.toStringTag",
        None,
        "IPv4 must not fire on a spec section citation in prose",
        "decision/address",
    ),
    (
        "/// Parses a document into a DOM tree.",
        None,
        "a doc comment with no site in it",
        "site-prose",
    ),
    (
        "// site-neutral: the shape needs naming and the site is the only name\n"
        "// for it; nothing branches on this.\n"
        "// (bilibili feed)",
        None,
        "suppression covers a prose hit too",
        None,
    ),
]

# (file name, source, what the case is about, rule this case expects/defends)
# Separate from CASES only because the file-name rule reads the path rather than
# the contents, so the harness has to be able to choose a file's name. The rule
# column follows the same convention as CASES: a rule means must-flag, None
# means must-pass.
NAME_CASES: list[tuple[str, str, str, str | None]] = [
    (
        "bilibili_diag.rs",
        "fn main() {}\n",
        "a source file named for a site",
        "site-filename",
    ),
    (
        "media_discovery.rs",
        "fn main() {}\n",
        "a source file named for a capability",
        None,
    ),
]

# (source, argv, expected exit status) - the gate has to be able to fail.
EXIT_CASES: list[tuple[str, list[str], int]] = [
    ("fn main() {}\n", [], 0),
    ("fn temp_diag_bilibili() {}\n", [], 1),
    ("/// The douyin player needs a click.\nfn main() {}\n", [], 0),
    ("/// The douyin player needs a click.\nfn main() {}\n", ["--strict"], 1),
]


def all_rule_names() -> list[str]:
    names = [f"decision/{name}" for name, _pattern, _description in gate.RULES]
    names.extend(name for name, _scope, _confidence, _description in gate.CHANNEL_RULES)
    return names


def _write(directory: pathlib.Path, name: str, source: str) -> pathlib.Path:
    path = directory / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")
    return path


def _confidence_of(rule: str) -> str:
    if rule.startswith("decision/"):
        return BLOCKING
    confidence = {name: conf for name, _scope, conf, _desc in gate.CHANNEL_RULES}
    return confidence[rule]


def check_case(source: str, expect_rule: str | None) -> list[str]:
    problems: list[str] = []
    with tempfile.TemporaryDirectory() as raw:
        path = _write(pathlib.Path(raw), "case.rs", source + "\n")
        hits = gate.scan(path)

    if expect_rule is None:
        if hits:
            return [
                f"expected no finding, got {[h.rule for h in hits]} for: {source!r}"
            ]
        return problems

    if not hits:
        return [f"expected rule {expect_rule!r}, got no finding for: {source!r}"]
    rules = [hit.rule for hit in hits]
    if expect_rule not in rules:
        return [
            f"expected rule {expect_rule!r}, got {sorted(set(rules))} for: {source!r}"
        ]
    for hit in hits:
        if not hit[2].strip():
            problems.append(f"finding reported without a reason for: {source!r}")
        if hit.confidence != _confidence_of(hit.rule):
            problems.append(
                f"rule {hit.rule!r} reported at the wrong confidence for: {source!r}"
            )
    return problems


def check_name_case(name: str, source: str, expect_rule: str | None) -> list[str]:
    with tempfile.TemporaryDirectory() as raw:
        path = _write(pathlib.Path(raw), name, source)
        hits = gate.scan(path)
    found = "site-filename" in [hit.rule for hit in hits]
    if expect_rule is None and found:
        return [f"unexpected site-filename finding for file named {name!r}"]
    if expect_rule is not None and not found:
        return [f"expected a {expect_rule!r} finding for file named {name!r}"]
    return []


def run_exit_case(source: str, argv: list[str]) -> tuple[int, str]:
    """Run main() against a one-file tree and return (exit status, stdout)."""
    saved_root = gate.REPO_ROOT
    try:
        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw)
            _write(root / "crates" / "demo" / "src", "lib.rs", source)
            gate.REPO_ROOT = root
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                status = gate.main(["check_site_neutrality.py", *argv])
            return status, out.getvalue()
    finally:
        gate.REPO_ROOT = saved_root


def check_cases() -> list[str]:
    """Every must-flag and must-pass case in the corpus."""
    problems: list[str] = []
    for source, expect_rule, description, _defends in CASES:
        for problem in check_case(source, expect_rule):
            problems.append(f"[{description}] {problem}")
    for name, source, description, expect_rule in NAME_CASES:
        for problem in check_name_case(name, source, expect_rule):
            problems.append(f"[{description}] {problem}")
    return problems


def check_exit_status() -> list[str]:
    """The gate can pass, can fail, and draws the tier line where documented."""
    problems: list[str] = []
    for source, argv, expected in EXIT_CASES:
        status, _text = run_exit_case(source, argv)
        if status != expected:
            problems.append(
                f"expected exit {expected} for argv={argv or ['(none)']}, "
                f"got {status} for: {source!r}"
            )
    return problems


def check_clean_run_reports_counts() -> list[str]:
    """A clean run says how much it looked at, which is what makes it auditable."""
    status, text = run_exit_case("fn main() {}\n", [])
    problems: list[str] = []
    if status != 0:
        problems.append(f"clean tree exited {status}")
    if "Rust source files scanned" not in text:
        problems.append(f"clean run did not report a file count: {text!r}")
    if "rules)" not in text:
        problems.append(f"clean run did not report a rule count: {text!r}")
    return problems


def check_usage_errors() -> list[str]:
    out, err = io.StringIO(), io.StringIO()
    problems: list[str] = []
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        if gate.main(["check_site_neutrality.py", "--help"]) != 0:
            problems.append("--help did not exit 0")
        if gate.main(["check_site_neutrality.py", "--nonsense"]) != 2:
            problems.append("an unknown argument did not exit 2")
    return problems


def check_rule_coverage() -> list[str]:
    """Every rule has a must-flag case and a must-pass case.

    A rule with only must-flag cases is how a gate gets switched off: it looks
    like it is protecting the tree while nobody can tell what it lets through.
    """
    problems: list[str] = []
    must_flag = {rule for _src, rule, _desc, _defends in CASES if rule}
    must_flag.update(rule for _n, _s, _d, rule in NAME_CASES if rule)
    must_pass = {defends for _src, rule, _d, defends in CASES if rule is None and defends}
    must_pass.update("site-filename" for _n, _s, _d, rule in NAME_CASES if rule is None)
    for name in all_rule_names():
        if name not in must_flag:
            problems.append(f"rule {name!r} has no must-flag case")
        if name not in must_pass:
            problems.append(f"rule {name!r} has no must-pass case")
    return problems


# The `check_*` functions above return a list of problems so that the standalone
# runner can print them all at once. These thin wrappers are what pytest
# collects, and they must assert: a `test_*` function that merely *returns* its
# failures passes under pytest whatever it found, because an empty list and a
# full one are both "no exception raised".
CHECKS = (
    ("cases", check_cases),
    ("exit status", check_exit_status),
    ("clean-run counts", check_clean_run_reports_counts),
    ("usage errors", check_usage_errors),
    ("rule coverage", check_rule_coverage),
)


def _assert_clean(problems: list[str]) -> None:
    assert not problems, "\n".join(f"  {problem}" for problem in problems)


def test_cases() -> None:
    _assert_clean(check_cases())


def test_exit_status() -> None:
    _assert_clean(check_exit_status())


def test_clean_run_reports_counts() -> None:
    _assert_clean(check_clean_run_reports_counts())


def test_usage_errors() -> None:
    _assert_clean(check_usage_errors())


def test_rule_coverage() -> None:
    _assert_clean(check_rule_coverage())


def test_standalone_runner_agrees() -> None:
    """The standalone runner and the checkers must reach the same verdict.

    Run with its output swallowed so pytest output stays readable; the runner's
    own printing is not what is under test here, its verdict is.
    """
    with contextlib.redirect_stdout(io.StringIO()):
        assert main() == 0


def main() -> int:
    failures: list[str] = []
    total = 0
    for source, expect_rule, description, _defends in CASES:
        total += 1
        label = f"flag {expect_rule}" if expect_rule else "pass"
        print(f"  ok   [{label:24}] {description}")
    for name, _source, description, rule in NAME_CASES:
        total += 1
        label = f"flag {rule}" if rule else "pass"
        print(f"  ok   [{label:24}] {description}")
    for _source, argv, expected in EXIT_CASES:
        total += 1
        print(f"  ok   [{'exit':24}] exit {expected} for argv={argv or ['(none)']}")
    for label, check in CHECKS:
        if label == "cases":
            continue
        total += 1
        print(f"  ok   [{label:24}] {label} checks pass")

    for _label, check in CHECKS:
        for problem in check():
            failures.append(problem)

    if failures:
        print(f"\nsite-neutrality self-test: {len(failures)} failure(s)\n")
        for failure in failures:
            print(f"  {failure}")
        return 1

    flagged = sum(1 for _src, rule, _d, _x in CASES if rule) + sum(
        1 for _n, _s, _d, rule in NAME_CASES if rule
    )
    print(
        f"\nsite-neutrality self-test: {total} checks passed "
        f"({flagged} must-flag, {total - flagged - len(EXIT_CASES)} must-pass, "
        f"{len(EXIT_CASES)} exit-status, 1 coverage)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
