#!/usr/bin/env python3
"""Regenerate `crates/render-layout/src/linebreak/tables.rs` from the UCD.

The line breaking algorithm in `crates/render-layout/src/linebreak/mod.rs` is
UAX #14 §6, and §5 is explicit that "the line break property assignments from
the data file are normative" and that "for the complete list always refer to
the data file". So the class table is data, not something to retype, and this
script is the one place that data is derived.

Inputs (Unicode Character Database, `https://www.unicode.org/Public/UCD/latest/`):

* `ucd/extracted/DerivedLineBreak.txt` - the Line_Break property, with the
  derived defaults for the CJK and currency blocks already spelled out. UAX #14
  §5 names this file as "the same data, but with a more explicit listing of code
  point ranges with complex default values".
* `ucd/extracted/DerivedEastAsianWidth.txt` - East_Asian_Width, which UAX #14
  §5 requires for `$EastAsian` in rule LB30, and which CSS Text 3 §5.2 needs for
  the `PO`/`PR` half of its `loose` requirements.

Run it from the repository root:

    python crates/render-layout/tools/generate_linebreak_tables.py

Classes that UAX #14 rule LB1 resolves away, or that this engine does not
implement, are resolved here instead of being emitted, and `mod.rs` documents
which is which. Everything else is emitted verbatim.
"""

from __future__ import annotations

import io
import sys
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
OUT = REPO_ROOT / "crates" / "render-layout" / "src" / "linebreak" / "tables.rs"
CACHE = Path.home() / "AppData" / "Local" / "Temp" / "opencode" / "lb"
BASE = "https://www.unicode.org/Public/UCD/latest/ucd/extracted/"

FILES = {
    "line": "DerivedLineBreak.txt",
    "eaw": "DerivedEastAsianWidth.txt",
}

# The UCD spells a class in capitals; the Rust enum is `CamelCase`, because the
# project's lint set requires it. The mapping is written out rather than derived
# so that a new class in a future UCD fails loudly instead of being guessed at.
VARIANTS = {
    "BK": "Bk", "CR": "Cr", "LF": "Lf", "NL": "Nl", "CM": "Cm", "WJ": "Wj",
    "ZW": "Zw", "GL": "Gl", "SP": "Sp", "ZWJ": "Zwj", "B2": "B2", "BA": "Ba",
    "BB": "Bb", "HY": "Hy", "HH": "Hh", "IN": "In", "CL": "Cl", "CP": "Cp",
    "EX": "Ex", "NS": "Ns", "OP": "Op", "QU": "Qu", "IS": "Is", "NU": "Nu",
    "PO": "Po", "PR": "Pr", "SY": "Sy", "ID": "Id", "CJ": "Cj", "HL": "Hl",
    "H2": "H2", "H3": "H3", "JL": "Jl", "JV": "Jv", "JT": "Jt", "RI": "Ri",
    "EB": "Eb", "EM": "Em",
}

# Emitted verbatim. Everything absent here is resolved to `Al` by `mod.rs`, which
# names each one; the table is the list of classes the algorithm distinguishes.
EMITTED = {
    "BK", "CR", "LF", "NL", "CM", "WJ", "ZW", "GL", "SP", "ZWJ",
    "B2", "BA", "BB", "HY", "HH", "IN",
    "CL", "CP", "EX", "NS", "OP", "QU",
    "IS", "NU", "PO", "PR", "SY",
    "ID", "CJ", "HL", "H2", "H3", "JL", "JV", "JT", "RI", "EB", "EM",
}

# Resolved to `Al`, and therefore not emitted. Each entry is the reason, so the
# table below and the module documentation cannot disagree.
RESOLVED_TO_AL = {
    "AL": "the class itself",
    "AI": "LB1; §5.1 permits resolving every ambiguous character to AL",
    "SA": "LB1 resolves a non-Mn/Mc SA character to AL; dictionary breaking is absent",
    "SG": "LB1",
    "XX": "LB1",
    "AK": "LB28a is not implemented, so a Brahmic base is an ordinary letter",
    "AP": "LB28a is not implemented",
    "AS": "LB28a is not implemented",
    "VI": "LB28a is not implemented",
    "VF": "LB28a is not implemented",
    "CB": "LB20's default for an unresolved CB is break before and after, which is ID",
}


def fetch(name: str) -> str:
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / name
    if not path.exists():
        with urllib.request.urlopen(BASE + name) as response:  # noqa: S310
            path.write_bytes(response.read())
    return path.read_text(encoding="utf-8")


def ranges(text: str) -> list[tuple[int, int, str]]:
    out = []
    for line in text.splitlines():
        body = line.split("#", 1)[0].strip()
        if not body:
            continue
        fields = [field.strip() for field in body.split(";")]
        span = fields[0]
        if ".." in span:
            first, last = (int(part, 16) for part in span.split(".."))
        else:
            first = last = int(span, 16)
        out.append((first, last, fields[1]))
    out.sort()
    return out


def merge(entries: list[tuple[int, int, str]]) -> list[tuple[int, int, str]]:
    merged: list[tuple[int, int, str]] = []
    for first, last, value in entries:
        if merged and merged[-1][2] == value and merged[-1][1] + 1 == first:
            merged[-1] = (merged[-1][0], last, value)
        else:
            merged.append((first, last, value))
    return merged


def pick(entries, wanted: set[str]) -> list[tuple[int, int, str]]:
    return merge([entry for entry in entries if entry[2] in wanted])


def render_ranges(name: str, doc: str, entries) -> str:
    lines = [f"/// {doc}", f"pub(super) static {name}: &[(u32, u32)] = &["]
    width = max((len(f"{first:04X}") for first, _, _ in entries), default=4)
    for first, last, _ in entries:
        lines.append(f"    (0x{first:0{width}X}, 0x{last:0{width}X}),")
    lines.append("];")
    return "\n".join(lines)


def render_classes(entries) -> str:
    lines = ["pub(super) static LINE_BREAK: &[(u32, u32, LineBreakClass)] = &["]
    width = max((len(f"{first:04X}") for first, _, _ in entries), default=4)
    for first, last, value in entries:
        lines.append(
            f"    (0x{first:0{width}X}, 0x{last:0{width}X}, "
            f"LineBreakClass::{VARIANTS[value]}),"
        )
    lines.append("];")
    return "\n".join(lines)


HEADER = '''//! Character data for the UAX #14 line breaking algorithm.
//!
//! Generated by `crates/render-layout/tools/generate_linebreak_tables.py` from
//! the Unicode Character Database. Do not edit by hand: regenerate.
//!
//! Each table is a sorted, non-overlapping list of inclusive code point ranges
//! read with a binary search. A code point in no range takes the class named in
//! `mod.rs` as its default, which is why `Al` is not in [`LINE_BREAK`].
//!
//! The `Line_Break` assignments are normative per UAX #14 §5 ("The line break
//! property assignments from the data file are normative"); the derivation here
//! is the UCD's own `DerivedLineBreak.txt`, which §5 also names as "the same
//! data, but with a more explicit listing of code point ranges with complex
//! default values".

#![allow(clippy::unreadable_literal, reason = "generated code point ranges")]

use super::LineBreakClass;
'''


def main() -> int:
    line = ranges(fetch(FILES["line"]))
    eaw = ranges(fetch(FILES["eaw"]))

    unknown = {value for _, _, value in line} - EMITTED - set(RESOLVED_TO_AL)
    if unknown:
        print(f"unclassified line break classes: {sorted(unknown)}", file=sys.stderr)
        return 1

    classes = pick(line, EMITTED)
    # `$EastAsian` is `[\\p{ea=F}\\p{ea=W}\\p{ea=H}]` (UAX #14 §6).
    wide = pick(eaw, {"F", "W", "H"})
    # CSS Text 3 §5.2 distinguishes `A`/F/W from F/W for its `PO`/`PR` rows.
    wide_or_ambiguous = pick(eaw, {"A", "F", "W"})
    parts = [
        HEADER,
        render_classes(classes),
        "",
        render_ranges(
            "EAST_ASIAN_WIDE",
            "UAX #14 §6's `$EastAsian`: East_Asian_Width F, W or H.",
            wide,
        ),
        "",
        render_ranges(
            "EAST_ASIAN_WIDE_OR_AMBIGUOUS",
            "CSS Text 3 §5.2's `PO`/`PR` rows: East_Asian_Width Ambiguous, "
            "Fullwidth or Wide.",
            wide_or_ambiguous,
        ),
        "",
    ]
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with io.open(OUT, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("\n".join(parts))
    print(f"wrote {OUT} ({len(classes)} line break ranges)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
