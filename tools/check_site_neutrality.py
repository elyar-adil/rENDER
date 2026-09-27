#!/usr/bin/env python3
"""Enforce the site-neutrality law from docs/generic-browser-todo.md.

The project law is that missing capability is fixed generically in the engine and
never by branching on a site. That rule is easy to state and easy to violate by
accident, so it is checked mechanically here.

A violation is a *site identifier in a decision position* in engine source - a
comparison, a match arm, or a substring test. The identifier can take several
shapes, because the obvious evasion is not writing the domain literally:

    if host == "www.taobao.com" { ... }              # domain
    if host.eq_ignore_ascii_case("WWW.TAOBAO.COM")    # case evasion
    if url.contains("taobao") { ... }                # brand token, no TLD
    if url == concat!("www.", "taobao", ".com")       # split literal
    if host == "203.107.1.1" { ... }                 # bare address
    if env::var("RENDER_SITE").is_ok() { ... }        # externalised switch
    if cfg!(feature = "site-jd") { ... }             # build-time switch

What is deliberately NOT a violation
------------------------------------
- A site identifier used as inert test data, for example a fixture URL handed to a
  parser or a loader. `real_site_acceptance` fixtures and the offline diagnostic
  corpora legitimately contain real URLs; they make no decision based on them.
- Generic host handling. Reading `Url::host_str()`, splitting a host, comparing a
  host against the origin of a request, or validating a cookie/domain scope are all
  correct and necessary. This script only fires when an identifier is compared
  against something.
- Reserved names and addresses: RFC 2606 domains (`example.com`), RFC 5737
  documentation ranges, loopback, and the specification URIs that correct namespace
  handling compares against.
- Anything under third_party/, target/, .diag/, or .artifacts/.

Suppression
-----------
A line may be suppressed with a comment that states a reason::

    // site-neutral: example.com is a reserved domain from RFC 2606, used to
    // exercise the unsupported-scheme path.

Known limitations - read this before trusting a clean run
--------------------------------------------------------
This is a lint, not a proof. It cannot see:
- a site identifier assembled at runtime from parsed data rather than literals;
- a site-specific branch keyed on a *class name*, *title* or *path* that contains no
  recognisable brand token (`if class == "J-global-header"`). Detecting those needs a
  corpus of known site class names, and a rule that guesses would produce more noise
  than signal;
- a branch whose condition is a function call with the identifier passed in from
  elsewhere.

Exit status is 0 when clean, 1 when violations are found, 2 on a usage error.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

SKIP_DIRS = {
    "third_party",
    "target",
    ".diag",
    ".artifacts",
    ".git",
    ".agent-workspace",
    "artifacts",
}

# The positions in which an identifier stops being data and becomes a decision.
DECISION_PREFIXES = (
    r"==",
    r"!=",
    r"starts_with",
    r"ends_with",
    r"contains",
    r"eq_ignore_ascii_case",
    r"strip_prefix",
    r"strip_suffix",
    r"trim_start_matches",
    r"trim_end_matches",
    r"concat!",
)

# A domain-shaped literal. Requires a label plus a plausible public suffix.
DOMAIN = re.compile(
    r"\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+"
    r"(?:com|cn|net|org|io|co|gov|edu|me|dev|app|info|biz|tv|cc|xyz)\b",
    re.IGNORECASE,
)

# Brand tokens with no TLD, which the domain rule above cannot see. Kept as one
# alternation so it is greppable and easy to extend. Adding a token here is cheap;
# adding one that also appears in legitimate engine vocabulary is not, so read the
# allowlist before extending.
SITE_TOKEN = re.compile(
    r"\b(?:taobao|tmall|alibaba|jd\.com|jingdong|163\.com|netease|163cn|"
    r"bilibili|bilivideo|zhihu|douyin|tiktok|weibo|sina|sohu|tencent|qq\.com|"
    r"baidu|youku|iqiyi|kuaishou|dianping|meituan|ctrip|58\.com|lagou|"
    r"xiaohongshu|douban|zhuanlan|csdn|jianshu)\b",
    re.IGNORECASE,
)

# A bare IPv4 literal. Loopback and the RFC 5737 documentation range are allowlisted.
IPV4 = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")

# An environment variable or cargo feature that selects a site.
EXTERNAL_SWITCH = re.compile(
    r"""(?:RENDER_[A-Z0-9_]*(?:SITE|TENANT)|feature\s*=\s*"[^"]*(?:site|tenant)[^"]*")""",
    re.IGNORECASE,
)

# `match` arms and `=>` arms bind the literal without a comparison operator.
DECISION_ARMED = re.compile(r"""(?:^|[\s{,])"[^"]*"\s*(?:\|[^|]*)*=>""")

SUPPRESSION = re.compile(r"site-neutral\s*:", re.IGNORECASE)

# Reserved names and addresses, checked against the *matched identifier* rather
# than the whole line. Checking the line would let one reserved token excuse an
# unrelated site identifier on the same line, which is itself an evasion.
# Each entry is (regex, why).
ALLOWLIST = (
    (
        re.compile(r"^example\.(com|org|net)$", re.IGNORECASE),
        "example.com/org/net are reserved by RFC 2606 precisely so documentation "
        "and tests can use them without touching a real site",
    ),
    (
        re.compile(r"\.invalid$|invalid\.", re.IGNORECASE),
        ".invalid is reserved by RFC 2606 for testing name resolution",
    ),
    (
        re.compile(
            r"^(?:127\.\d{1,3}\.\d{1,3}\.\d{1,3}"
            r"|0\.0\.0\.0|255\.255\.255\.255|::1|localhost)$",
            re.IGNORECASE,
        ),
        "loopback and unspecified addresses are local, not sites",
    ),
    (
        re.compile(r"^(?:192\.0\.2|198\.51\.100|203\.0\.113)\.\d{1,3}$"),
        "RFC 5737 documentation ranges are reserved for examples",
    ),
    (
        re.compile(
            r"(?:w3\.org|xml\.org|schemas\.|purl\.org|unicode\.org|rust-lang\.org)$",
            re.IGNORECASE,
        ),
        "specification and namespace URIs; comparing these is required for correct "
        "namespace handling",
    ),
)

RUST_SUFFIXES = (".rs",)

# (name, pattern, human description). A line is flagged when the pattern matches
# AND the line also sits in a decision position.
RULES = (
    ("domain", DOMAIN, "a domain literal"),
    ("brand-token", SITE_TOKEN, "a site brand token"),
    ("address", IPV4, "a bare IPv4 literal"),
    ("external-switch", EXTERNAL_SWITCH, "an environment variable or cargo feature that selects a site"),
)


def iter_source_files() -> list[Path]:
    files: list[Path] = []
    crates = REPO_ROOT / "crates"
    if not crates.is_dir():
        return files
    for path in sorted(crates.rglob("*")):
        if not path.is_file() or path.suffix not in RUST_SUFFIXES:
            continue
        if SKIP_DIRS.intersection(path.relative_to(REPO_ROOT).parts):
            continue
        files.append(path)
    return files


def allowlist_reason(identifier: str) -> str | None:
    """Why this specific matched identifier is reserved, if it is.

    Takes the matched text, not the line. A reserved token on a line must not
    excuse an unrelated site identifier on that same line.
    """
    for pattern, why in ALLOWLIST:
        if pattern.search(identifier):
            return why
    return None


def suppressed(lines: list[str], index: int) -> bool:
    start = max(0, index - 2)
    return any(SUPPRESSION.search(line) for line in lines[start : index + 1])


def in_decision_position(line: str) -> str | None:
    """Which decision construct, if any, governs this line."""
    for prefix in DECISION_PREFIXES:
        pattern = re.compile(re.escape(prefix) + r"""\s*\(*\s*(?:b|r)?"[^"]*""")
        if pattern.search(line):
            return prefix
    if DECISION_ARMED.search(line):
        return "match arm"
    if EXTERNAL_SWITCH.search(line):
        # A bare `RENDER_*SITE*` lookup is a site switch whatever it is compared to.
        return "environment lookup"
    return None


def scan(path: Path) -> list[tuple[int, str, str]]:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    lines = text.splitlines()
    found: list[tuple[int, str, str]] = []
    try:
        relative = path.relative_to(REPO_ROOT)
    except ValueError:
        relative = path

    for index, line in enumerate(lines):
        decision = in_decision_position(line)
        if decision is None:
            continue
        if suppressed(lines, index):
            continue
        for name, pattern, description in RULES:
            for match in pattern.finditer(line):
                if allowlist_reason(match.group(0)) is not None:
                    continue
                found.append(
                    (
                        index + 1,
                        line.strip(),
                        f"{relative}: {description} in a {decision} position - this is "
                        f"the shape of a per-site branch, which "
                        f"docs/generic-browser-todo.md forbids. Fix the generic capability "
                        f"instead, or suppress with a '// site-neutral: <reason>' comment if "
                        f"this is genuinely not a site branch.",
                    )
                )
                break
            else:
                continue
            break
    return found


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] in {"-h", "--help"}:
        print(__doc__)
        return 0

    files = iter_source_files()
    if not files:
        print("site-neutrality: no Rust sources found; is this run from the repo root?")
        return 2

    violations: list[tuple[int, str, str]] = []
    for path in files:
        violations.extend(scan(path))

    if not violations:
        print(
            f"site-neutrality: clean ({len(files)} Rust source files scanned, "
            f"{len(RULES)} identifier rules)"
        )
        return 0

    print(f"site-neutrality: {len(violations)} violation(s)\n")
    for line_number, line, reason in violations:
        print(f"  {line_number}: {line}")
        print(f"      {reason}\n")
    print(
        "Per-site branches are forbidden. The capability gap belongs in the engine "
        "as a generic fix with a reduced test; see docs/generic-browser-todo.md."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
