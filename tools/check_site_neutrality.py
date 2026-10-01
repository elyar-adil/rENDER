#!/usr/bin/env python3
"""Enforce the site-neutrality law from docs/generic-browser-todo.md.

The project law is that missing capability is fixed generically in the engine and
never by branching on a site. That rule is easy to state and easy to violate by
accident, so it is checked mechanically here.

A site name does not only enter code through a comparison. It enters through
whichever channel the author reaches for first, and a gate that watches only one
channel is defeated by the second one written. The channels checked here, in
descending confidence:

``decision``
    A site identifier compared against something - a comparison, a match arm, a
    substring test, a site-selecting environment variable or cargo feature. The
    identifier can take several shapes, because the obvious evasion is not
    writing the domain literally::

        if host == "www.taobao.com" { ... }              # domain
        if host.eq_ignore_ascii_case("WWW.TAOBAO.COM")    # case evasion
        if url.contains("taobao") { ... }                # brand token, no TLD
        if url == concat!("www.", "taobao", ".com")       # split literal
        if host == "203.107.1.1" { ... }                 # bare address
        if env::var("RENDER_SITE").is_ok() { ... }        # externalised switch
        if cfg!(feature = "site-jd") { ... }             # build-time switch

A *mention* of a switch is not a switch. A comment that explains
``RENDER_SITE``, or a string that spells the name out, is prose and is left to
the advisory rules below; only the shape of a read is a decision position.

``site-identifier``
    A site brand token spelled into a Rust identifier - a function, constant or
    module name. A test called ``fn temp_diag_bilibili()`` names the site in the
    only place a reader looks first, and it survives every rename that leaves
    the body alone.

``site-path``
    A site-named path segment inside a string literal, which is how a test
    binds itself to one site's captured page: ``".diag/bilibili/page.html"``.
    Only a literal that is unambiguously a path on disk is read, and a URL's
    host and path are not: a URL handed to a parser is inert fixture data (see
    below), while a path *segment* naming a site is the test saying "this
    regression is about that site". This is a deliberate narrowing, and the
    cost is recorded under known limitations.

``site-prose`` (advisory)
    A site brand token in a comment or doc comment. Prose naming a site as an
    example of a shape is not the same as code branching on one, so this is
    reported with its own diagnostic and does not fail the gate unless
    ``--strict`` is passed. Silence here is how a site name becomes the
    accepted vocabulary of a file.

``site-filename`` (advisory)
    A site brand token in a source file's own name, for the same reason.

What is deliberately NOT a violation
------------------------------------
- A site identifier used as inert test data, for example a fixture URL handed to a
  parser or a loader. `real_site_acceptance` fixtures and the offline diagnostic
  corpora legitimately contain real URLs; they make no decision based on them.
  A URL *host* inside a string is therefore never a site-path hit; a path
  *segment* naming a site is, because that is a statement about which site the
  code is about rather than a string being parsed.
- Generic host handling. Reading `Url::host_str()`, splitting a host, comparing a
  host against the origin of a request, or validating a cookie/domain scope are all
  correct and necessary. The decision rules only fire when an identifier is
  compared against something.
- Reserved names and addresses: RFC 2606 domains (`example.com`), RFC 5737
  documentation ranges, loopback, and the specification URIs that correct namespace
  handling compares against.
- Anything under third_party/, target/, .diag/, or .artifacts/.

Only ``SITE_TOKEN`` is used outside a decision position, and that is deliberate.
``DOMAIN`` matches `track.info` and `console.info` (`info` is a real TLD), and
``IPV4`` matches every spec section citation in the tree (`HTML 13.2.4.5`,
`ECMA-262 20.1.3.6`). Both are meaningful when compared against and pure noise in
prose, a path, or an identifier, so they stay confined to decision positions.

Suppression
-----------
A line may be suppressed with a comment that states a reason::

    // site-neutral: example.com is a reserved domain from RFC 2606, used to
    // exercise the unsupported-scheme path.

The same suppression applies at every confidence tier.

Exit status
-----------
0 when there are no blocking violations, 1 when there are, 2 on a usage error.
Advisory findings are printed either way and do not change the exit status unless
``--strict`` is passed, which promotes them to blocking.

Known limitations - read this before trusting a clean run
--------------------------------------------------------
This is a lint, not a proof. It cannot see:
- a site identifier assembled at runtime from parsed data rather than literals;
- a site-specific branch keyed on a *class name*, *title* or *path* that contains no
  recognisable brand token (`if class == "J-global-header"`). Detecting those needs a
  corpus of known site class names, and a rule that guesses would produce more noise
  than signal;
- a branch whose condition is a function call with the identifier passed in from
  elsewhere;
- a site whose brand token is not in ``SITE_TOKEN`` at all. The token list is
  partial by design - it is extended only when a token cannot collide with
  legitimate engine vocabulary, so a captured site named `hao123` or `163` is
  invisible to the string, path, identifier and prose rules;
- a test whose name carries only a bare number (`fn temp_read_5073_state()`). No
  token-free rule can tell that from any other numbered test, so such a test is
  caught only if it also reads a site-named path, which this one does;
- a site-named path buried in a larger markup or prose literal.
  ``FILESYSTEM_PATH`` is anchored at both ends of the literal, so
  ``"<source src='/media/bilibili-init.mp4'>"`` is not a path and is not
  reported. That is the cost of not flagging the browser home page, which is a
  large literal full of site names and is a product decision rather than a
  per-site accommodation. It is a deliberate narrowing, not an oversight;
- a site name in free text inside a string literal, as opposed to a path. An
  ``eprintln!("skipped: saved bilibili page not present")`` is neither a path
  nor a comment. Extending the rules to catch it would also flag every inert
  fixture string, so it is left;
- a site token in an unterminated block comment, which swallows the rest of the
  file. Such a file does not compile, so cargo catches it first;
- sources outside `crates/` - `tests/real_site_tasks/` is a separate workspace and is
  not scanned.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import NamedTuple

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
# Note that this also matches `track.info` and `console.info`; see the module
# docstring for why it is confined to decision positions.
DOMAIN = re.compile(
    r"\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+"
    r"(?:com|cn|net|org|io|co|gov|edu|me|dev|app|info|biz|tv|cc|xyz)\b",
    re.IGNORECASE,
)

# Brand tokens with no TLD, which the domain rule above cannot see. Kept as one
# alternation so it is greppable and easy to extend. Adding a token here is cheap;
# adding one that also appears in legitimate engine vocabulary is not, so read the
# allowlist before extending.
#
# This list is deliberately incomplete. It carries the tokens whose absence would
# be conspicuous and whose presence cannot collide with engine vocabulary. It does
# not carry every site this project has ever captured, and the rules that use it
# are correspondingly narrow: a captured site with no token here is only caught if
# a literal domain or address of it is compared against something.

# A token embedded in a longer Rust identifier (`fn temp_diag_bilibili()`) or file
# name (`baidu_diag.rs`) still has to match, so the boundary is "not a word
# character" rather than `\b`. Three things follow:
#   - `\b` does not fire next to `_`, which is exactly the character Rust uses to
#     join identifier parts. `\bbilibili\b` would never match
#     `temp_diag_bilibili`, which is the single case the identifier rule exists
#     for. The first version of this file had that bug.
#   - `_` is a *joiner*, not a word character, so it must be excluded from the
#     test. Excluding it the other way round would stop `bilibili_init` matching.
#   - A token still has to end at a non-alphanumeric, so `taobaogx` does not
#     match, and `qq.company` does not match `qq\.com`.
_TOKEN_BEFORE = r"(?<![A-Za-z0-9])"
_TOKEN_AFTER = r"(?![A-Za-z0-9])"

SITE_TOKEN = re.compile(
    _TOKEN_BEFORE
    + r"(?:taobao|tmall|alibaba|jd\.com|jingdong|163\.com|netease|163cn|"
    r"bilibili|bilivideo|zhihu|douyin|tiktok|weibo|sina|sohu|tencent|qq\.com|"
    r"baidu|youku|iqiyi|kuaishou|dianping|meituan|ctrip|58\.com|lagou|"
    r"xiaohongshu|douban|zhuanlan|csdn|jianshu)"
    + _TOKEN_AFTER,
    re.IGNORECASE,
)

# A bare IPv4 literal. Loopback and the RFC 5737 documentation range are allowlisted.
# Note that this also matches spec section citations (`13.2.4.5`); see the module
# docstring for why it is confined to decision positions.
IPV4 = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")

# A cargo feature that selects a site. The name lives inside a string literal,
# because that is where cargo writes feature names.
FEATURE_SWITCH = re.compile(
    r"""feature\s*=\s*"[^"]*(?:site|tenant)[^"]*\"""",
    re.IGNORECASE,
)

# An environment variable that selects a site.
#
# This was one alternative of a single `EXTERNAL_SWITCH` pattern that also
# carried the feature half, and it was the bare variable name:
# `RENDER_[A-Z0-9_]*(?:SITE|TENANT)`. That matched any mention, so a comment
# explaining the switch, or a string spelling the name out, was reported as a
# site switch. A mention is not a switch, so each alternative now requires the
# shape of a read: the name handed to an environment lookup, or compared
# against something.
ENV_SWITCH = re.compile(
    r"""(?:env::(?:var|var_os)!?|env!|var!|var_os!)\s*\(\s*"?\s*"""
    r"""RENDER_[A-Z0-9_]*(?:SITE|TENANT)"""
    r"""|(?:==|!=|contains|starts_with|ends_with|eq_ignore_ascii_case)"""
    r"""\s*\(*\s*"?\s*RENDER_[A-Z0-9_]*(?:SITE|TENANT)""",
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
    ("external-switch", FEATURE_SWITCH, "a cargo feature that selects a site"),
    ("env-switch", ENV_SWITCH, "an environment variable that selects a site"),
)

BLOCKING = "blocking"
ADVISORY = "advisory"

# The four channels that are not decision positions, in the order they are
# reported. `SCOPES` says which lexical span a rule reads: identifiers read code,
# paths read string literals, prose reads comments.
# (name, scope, confidence, description)
CHANNEL_RULES = (
    ("site-identifier", "code", BLOCKING, "a site brand token in an identifier"),
    ("site-path", "string", BLOCKING, "a site-named path segment in a string literal"),
    ("site-prose", "comment", ADVISORY, "a site brand token in a comment"),
    ("site-filename", "filename", ADVISORY, "a site brand token in the file name"),
)

# A literal that is *only* path characters is a path on disk. `:` `?` `#` and
# whitespace are deliberately excluded, and that is what separates the two cases
# this rule has to tell apart:
#
#   ".diag/bilibili/page.html"          a path on disk  -> a hit
#   "https://www.bilibili.com/media/x"  a URL          -> inert data
#   "www.baidu.com/s?wd=rust"           a URL          -> inert data
#   "<span class=\"favorite-icon baidu\">"  markup     -> not a path
#
# A URL handed to a parser is inert fixture data. A path on disk whose segment
# names a site is the code binding itself to one site's captured page, which is
# the thing the law forbids. Guessing at markup would flag the browser's home
# page, so the rule only reads literals that are unambiguously a path.
FILESYSTEM_PATH = re.compile(r"\A[.A-Za-z0-9_\\/-]+\Z")

# A run of literal path separators, used to break a path into its segments.
PATH_SEPARATOR = re.compile(r"[/\\]")


# --------------------------------------------------------------------------
# Lexing
# --------------------------------------------------------------------------
#
# A line-oriented regex cannot tell a site token in code from the same token
# inside a string or a comment, and that distinction is the whole point of the
# non-decision rules. So the source is lexed once into per-line spans and each
# rule reads the spans it cares about. The lexer is not a Rust parser: it
# recognises comments, string literals, raw string literals, byte strings and
# char literals, which is everything needed to tell those three apart.

CODE = "code"
STRING = "string"
COMMENT = "comment"

_RAW_STRING = re.compile(r'(?:b?r)(#*)"')
_CHAR_LITERAL = re.compile(r"""'(?:\\.|[^\\'])'""")


class _Lexer:
    """Split Rust source into per-line (kind, text) spans.

    `kind` is one of CODE, STRING or COMMENT. A span that runs across a newline
    (a raw string, a block comment) is recorded against the line it starts on,
    which is the line a reader would point at.
    """

    def __init__(self, text: str) -> None:
        self.text = text
        self.length = len(text)
        self.pos = 0
        self.kind = CODE
        self.start = 0
        self.line = 0
        self.spans: list[list[tuple[str, str]]] = [[]]
        self.block_depth = 0
        self.raw_hashes = ""

    # -- span bookkeeping -------------------------------------------------
    def _record(self, chunk: str) -> None:
        if not chunk:
            return
        parts = chunk.split("\n")
        for index, part in enumerate(parts):
            if index:
                self.line += 1
                if self.line >= len(self.spans):
                    self.spans.append([])
            if part:
                self.spans[self.line].append((self.kind, part))

    def _close(self) -> None:
        """Record everything consumed since the last close, under the current kind."""
        if self.pos > self.start:
            self._record(self.text[self.start : self.pos])
        self.start = self.pos

    def _end_span(self, kind: str, new_kind: str) -> None:
        """Close the open span, which is of `kind`, and resume as `new_kind`."""
        self.kind = kind
        self._close()
        self.kind = new_kind
        self.start = self.pos

    # -- main loop --------------------------------------------------------
    def run(self) -> list[list[tuple[str, str]]]:
        text, length = self.text, self.length
        while self.pos < length:
            if self.kind == CODE:
                self._code(text, length)
            elif self.kind == COMMENT:
                self._comment(text, length)
            else:
                self._string(text, length)
        self._close()
        return self.spans

    def _code(self, text: str, length: int) -> None:
        start = self.pos
        # Every branch that leaves the CODE state closes the pending code span at
        # `start` first. `_end_span` records text[self.start:self.pos], so leaving
        # `self.pos` where it is makes it record exactly the code before the token.
        if text.startswith("//", start):
            self._end_span(CODE, COMMENT)
            newline = text.find("\n", start)
            self.pos = length if newline == -1 else newline + 1
            # A line comment ends at the newline; the newline itself is consumed
            # as part of the comment so the next line index stays correct.
            self._end_span(COMMENT, CODE)
            return
        if text.startswith("/*", start):
            self._end_span(CODE, COMMENT)
            self.block_depth = 1
            self.pos = start + 2
            return
        raw = _RAW_STRING.match(text, start)
        if raw is not None:
            self._open_string(raw.end(), raw.group(1))
            return
        if text[start] == '"':
            self._open_string(start + 1, "")
            return
        if text[start] == "'" and _CHAR_LITERAL.match(text, start) is not None:
            self._open_string(start + 1, "")
            self.pos = start + 2  # the content is the one character
            self._end_span(STRING, CODE)
            self.pos = start + 3  # now past the closing quote
            self.start = self.pos
            return
        self.pos = start + 1

    def _open_string(self, content_start: int, hashes: str) -> None:
        """Leave CODE and begin a STRING span whose first character is content.

        The opening delimiter stays in the code span and is not repeated at the
        head of the string span, so a rule reading a string reads the literal's
        characters. That matters: a path rule anchored at the start and end of
        the span would otherwise never match, because the span would still carry
        its surrounding quotes.
        """
        self.pos = content_start
        self._end_span(CODE, STRING)
        self.raw_hashes = hashes
        self.start = content_start

    def _comment(self, text: str, length: int) -> None:
        if self.block_depth == 0:
            newline = text.find("\n", self.pos)
            self.pos = length if newline == -1 else newline + 1
            self._end_span(COMMENT, CODE)
            return
        if text.startswith("/*", self.pos):
            # Rust block comments nest.
            self.block_depth += 1
            self.pos += 2
            return
        if text.startswith("*/", self.pos):
            self.block_depth -= 1
            self.pos += 2
            if self.block_depth == 0:
                self._end_span(COMMENT, CODE)
            return
        self.pos += 1

    def _string(self, text: str, length: int) -> None:
        if self.raw_hashes:
            closer = '"' + self.raw_hashes
            index = text.find(closer, self.pos)
            if index == -1:
                self.pos = length
                self._end_span(STRING, CODE)
                return
            self.pos = index  # the content ends before the delimiter
            self._end_span(STRING, CODE)
            self.pos = index + len(closer)
            self.start = self.pos
            return
        if text[self.pos] == "\\":
            self.pos = min(self.pos + 2, length)
            return
        if text[self.pos] == '"':
            self._end_span(STRING, CODE)  # content ends before the delimiter
            self.pos += 1
            self.start = self.pos
            return
        if text[self.pos] == "\n":
            # An unterminated string literal does not compile. Do not let one
            # swallow the rest of the file.
            self._end_span(STRING, CODE)
            return
        self.pos += 1


def lex(text: str) -> list[list[tuple[str, str]]]:
    """Per-line (kind, text) spans for a Rust source file."""
    return _Lexer(text).run()


# --------------------------------------------------------------------------
# Findings
# --------------------------------------------------------------------------


class Finding(NamedTuple):
    """One reported site identifier.

    Indexed positionally for backwards compatibility: `[0]` line, `[1]` the
    source line, `[2]` the reason.
    """

    line: int
    text: str
    reason: str
    rule: str
    confidence: str


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
    if FEATURE_SWITCH.search(line):
        # A build-time site switch is a site switch whatever it is compared to.
        return "build-time switch"
    if ENV_SWITCH.search(line):
        # A `RENDER_*SITE*` *read* is a site switch whatever it is compared to.
        return "environment lookup"
    return None


def site_named_path(literal: str) -> str | None:
    """The site-named path segment in a string literal, if there is one.

    Only a literal that is unambiguously a multi-segment path on disk is read. A
    URL is inert fixture data, markup is not a path, and a bare brand token with
    no separator is not a *path* segment either; see FILESYSTEM_PATH.
    """
    if not FILESYSTEM_PATH.match(literal):
        return None
    if not PATH_SEPARATOR.search(literal):
        return None
    for segment in PATH_SEPARATOR.split(literal):
        if DOMAIN.search(segment):
            # `www.bilibili.com` is a host that reached this position, not a
            # directory named after a site.
            continue
        match = SITE_TOKEN.search(segment)
        if match is not None:
            return match.group(0)
    return None


def rule_count() -> int:
    """How many rules this gate runs. Reported in both the clean and dirty paths."""
    return len(RULES) + len(CHANNEL_RULES)


def _scan_decisions(
    lines: list[str], relative: Path
) -> list[Finding]:
    """The original rules: a site identifier compared against something."""
    found: list[Finding] = []
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
                    Finding(
                        index + 1,
                        line.strip(),
                        f"{relative}: {description} in a {decision} position - this is "
                        f"the shape of a per-site branch, which "
                        f"docs/generic-browser-todo.md forbids. Fix the generic capability "
                        f"instead, or suppress with a '// site-neutral: <reason>' comment if "
                        f"this is genuinely not a site branch.",
                        f"decision/{name}",
                        BLOCKING,
                    )
                )
                break
            else:
                continue
            break
    return found


def _scan_channels(
    lines: list[str], spans: list[list[tuple[str, str]]], relative: Path
) -> list[Finding]:
    """The non-decision channels: identifiers, string paths, and prose."""
    found: list[Finding] = []
    for name, scope, confidence, description in CHANNEL_RULES:
        if scope == "filename":
            continue
        for index, line_spans in enumerate(spans):
            if index >= len(lines):
                break
            if suppressed(lines, index):
                continue
            line = lines[index]
            for kind, text in line_spans:
                if kind != scope:
                    continue
                if scope == "string":
                    token = site_named_path(text)
                else:
                    match = SITE_TOKEN.search(text)
                    token = match.group(0) if match else None
                if token is None:
                    continue
                if allowlist_reason(token) is not None:
                    continue
                found.append(
                    Finding(
                        index + 1,
                        line.strip(),
                        f"{relative}: {description} ({token!r}). A site name in "
                        + (
                            "a path segment binds this code to one site's captured "
                            "page; the capability it needs belongs in the engine, and "
                            "the regression test should be named for the shape it "
                            "covers, not the site it was found on. See "
                            "docs/generic-browser-todo.md."
                            if scope == "string"
                            else (
                                "an identifier names the site in the first place a "
                                "reader looks. Rename it for the shape it covers. See "
                                "docs/generic-browser-todo.md."
                                if scope == "code"
                                else "a comment is not a decision, so this does not "
                                "block the gate, but prose that names a site is how a "
                                "site name becomes accepted vocabulary. Prefer the "
                                "page shape. Suppress with '// site-neutral: <reason>' "
                                "if the mention is genuinely unavoidable."
                            )
                        ),
                        name,
                        confidence,
                    )
                )
    return found


def _scan_filename(relative: Path) -> list[Finding]:
    match = SITE_TOKEN.search(relative.name)
    if match is None or allowlist_reason(match.group(0)) is not None:
        return []
    return [
        Finding(
            1,
            str(relative),
            f"{relative}: a site brand token in the file name ({match.group(0)!r}). "
            f"A file named for a site is a per-site tool, which "
            f"docs/generic-browser-todo.md forbids. Advisory: this does not block the "
            f"gate, because a diagnostic tool named after what it diagnoses is a "
            f"weaker signal than one named after a site inside the engine. Rename it "
            f"for the page shape.",
            "site-filename",
            ADVISORY,
        )
    ]


def scan(path: Path) -> list[Finding]:
    """Every finding in one file, blocking first then advisory, in file order."""
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    lines = text.splitlines()
    try:
        relative = path.relative_to(REPO_ROOT)
    except ValueError:
        relative = path

    findings = _scan_decisions(lines, relative)
    findings.extend(_scan_channels(lines, lex(text), relative))
    findings.extend(_scan_filename(relative))

    # One line can trip more than one rule; report each rule once per line so a
    # decision-position site name is not also reported as prose on the same line.
    seen: set[tuple[str, int]] = set()
    unique: list[Finding] = []
    for finding in findings:
        key = (finding.rule, finding.line)
        if key in seen:
            continue
        seen.add(key)
        unique.append(finding)
    unique.sort(key=lambda f: (f.line, f.rule))
    return unique


def main(argv: list[str]) -> int:
    args = list(argv[1:])
    strict = False
    for arg in args:
        if arg in {"-h", "--help"}:
            print(__doc__)
            return 0
        if arg == "--strict":
            strict = True
        else:
            print(f"site-neutrality: unknown argument {arg!r}; try --help", file=sys.stderr)
            return 2

    files = iter_source_files()
    if not files:
        print("site-neutrality: no Rust sources found; is this run from the repo root?")
        return 2

    findings: list[Finding] = []
    for path in files:
        findings.extend(scan(path))

    blocking = [f for f in findings if f.confidence == BLOCKING]
    advisory = [f for f in findings if f.confidence == ADVISORY]
    counts = f"{len(files)} Rust source files scanned, {rule_count()} rules"

    if not findings:
        print(f"site-neutrality: clean ({counts})")
        return 0

    print(
        f"site-neutrality: {len(blocking)} blocking violation(s), "
        f"{len(advisory)} advisory finding(s) ({counts})\n"
    )
    for finding in findings:
        marker = "VIOLATION" if finding.confidence == BLOCKING else "advisory"
        print(f"  [{marker}] {finding.line}: {finding.text}")
        print(f"      {finding.reason}\n")

    if advisory and not strict:
        print(
            f"{len(advisory)} advisory finding(s) did not fail the gate; re-run with "
            "--strict to treat prose and file-name mentions as violations.\n"
        )

    print(
        "Per-site branches are forbidden. The capability gap belongs in the engine "
        "as a generic fix with a reduced test; see docs/generic-browser-todo.md."
    )
    if blocking or (advisory and strict):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
