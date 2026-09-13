"""Error-driven import fixer for the freshly split runtime/ tree."""
import json
import re
import subprocess
import sys
from pathlib import Path

import argparse

ROOT = Path(__file__).resolve().parent.parent
MODULE_DIR = {
    "runtime": ROOT / "crates/render-core/src/js/runtime",
    "solver": ROOT / "crates/render-core/src/layout/solver",
}
NAMES_FILE = {
    "runtime": ROOT / "tools/names.json",
    "solver": ROOT / "tools/names_solver.json",
}

def run_check():
    proc = subprocess.run(
        ARGS.cargo,
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    msgs = []
    for line in proc.stdout.split("\n"):
        line = line.strip()
        if not line.startswith("{"):
            continue
        data = json.loads(line)
        if data.get("reason") == "compiler-message":
            msgs.append(data["message"])
    return proc.returncode, msgs


def primary_span(msg):
    for span in msg.get("spans", []):
        if span.get("is_primary"):
            return span
    return msg["spans"][0] if msg.get("spans") else None


def insert_import(path, import_line):
    lines = path.read_text(encoding="utf-8").split("\n")
    last_use = max((i for i, l in enumerate(lines) if l.startswith("use ")), default=-1)
    if import_line in lines:
        return
    lines.insert(last_use + 1, import_line)
    path.write_text("\n".join(lines), encoding="utf-8")


def delete_line(path, lineno):
    lines = path.read_text(encoding="utf-8").split("\n")
    if 0 <= lineno - 1 < len(lines) and lines[lineno - 1].lstrip().startswith("use "):
        del lines[lineno - 1]
        path.write_text("\n".join(lines), encoding="utf-8")
        return True
    return False


def find_def(name):
    return NAME_MAP.get(name)


parser = argparse.ArgumentParser()
parser.add_argument("module", choices=sorted(MODULE_DIR))
ARGS = parser.parse_args()
OUT = MODULE_DIR[ARGS.module]
NAME_MAP = json.loads(NAMES_FILE[ARGS.module].read_text(encoding="utf-8"))
ARGS.cargo = ["cargo", "check", "-p", "render-core", "--all-targets", "--message-format=json"]

fixed_total = 0
for iteration in range(20):
    code, msgs = run_check()
    acted = False
    errors = []
    for msg in msgs:
        if msg.get("level") != "error":
            continue
        mcode = (msg.get("code") or {}).get("code") or ""
        text = msg["message"]
        span = primary_span(msg)
        if mcode == "unused_imports" and span:
            path = ROOT / span["file_name"]
            if delete_line(path, span["line_start"]):
                acted = True
                continue
        m = re.search(r"cannot find (?:value|function|type) `(\w+)`", text)
        if m or mcode in ("E0425", "E0432", "E0433"):
            names = re.findall(r"`(\w+)`", text)
            target = span and ROOT / span["file_name"]
            done = False
            if target and target.exists():
                for name in names:
                    base = find_def(name)
                    if base:
                        import_line = f"use {base}::{name};"
                        insert_import(target, import_line)
                        acted = True
                        done = True
                        break
            if not done:
                errors.append(f"{mcode or 'E'} {text.splitlines()[0]}")
            continue
        errors.append(f"{mcode or 'E'} {text.splitlines()[0]}")
    if not acted:
        print(f"iteration {iteration}: no auto-fix available; {len(errors)} error(s) remain")
        for e in errors[:25]:
            print("  ", e)
        sys.exit(1 if errors else 0)
    fixed_total += 1

print("iteration cap reached")
sys.exit(1)
