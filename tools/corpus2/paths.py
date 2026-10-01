#!/usr/bin/env python3
"""Where the corpus2 tools, their data and the corpus itself live.

The tool *code* is committed here, under `tools/corpus2/`, because a test that
lives only in a gitignored directory is a test that rots. The tool *data*
(`matrix.json`, `capture_index.json`, the candidate URL lists) and the captures
themselves stay in `.diag/corpus2/`, which is gitignored: 22 MB of captured
markup does not belong in history.

    tools/corpus2/          committed: the instruments and their tests
    .diag/corpus2/          gitignored: the captures, and _tools/ data + shims
    .diag/                  gitignored: the 9 pre-existing hand-picked portals

`.diag/corpus2/_tools/*.py` are one-line shims that load the module from here,
so every command in the README - and every command an earlier round of this
project published - keeps working.
"""

from __future__ import annotations

import os

TOOLS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(TOOLS, "..", ".."))
DIAG = os.path.join(REPO, ".diag")
CORPUS = os.path.join(DIAG, "corpus2")
LEGACY = os.path.join(CORPUS, "_tools")

# Data lives beside the old tools. Fall back to the committed directory so the
# tools still run on a clean checkout that has no `.diag/` at all.
DATA = LEGACY if os.path.isdir(LEGACY) else TOOLS

SCRATCH = os.environ.get(
    "CORPUS_SCRATCH",
    os.path.join(os.environ.get("TEMP", os.path.join(REPO, ".tmp")),
                 "corpus2_raw"),
)
PROXY = os.environ.get("CORPUS_PROXY", "http://127.0.0.1:17890")

# The browser-shaped agent every capture is requested with first.
UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36")


def data(name: str) -> str:
    """Path to a data file, preferring `.diag/corpus2/_tools/`."""
    p = os.path.join(DATA, name)
    if os.path.exists(p):
        return p
    return os.path.join(TOOLS, name)


def capture_dirs() -> list:
    """(shape, slug, path) for every capture on disk, sorted."""
    out = []
    if not os.path.isdir(CORPUS):
        return out
    for shape in sorted(os.listdir(CORPUS)):
        sd = os.path.join(CORPUS, shape)
        if not os.path.isdir(sd) or shape.startswith("_"):
            continue
        for slug in sorted(os.listdir(sd)):
            cd = os.path.join(sd, slug)
            if os.path.isdir(cd) and os.path.exists(os.path.join(cd, "page.html")):
                out.append((shape, slug, cd))
    return out
