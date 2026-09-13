"""One-shot migration: split layout/solver.rs into a solver/ module tree.

Same rustfmt-invariant machinery as split_runtime.py. Types stay in
mod.rs; only impl Solver methods and free functions move, grouped by
formatting context.
"""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/render-core/src/layout/solver.rs"
OUT = ROOT / "crates/render-core/src/layout/solver"

VIS_DIRECT = "pub(super)"

FILES = ["block", "flex", "grid", "inline", "resolve"]

MOD_KEEP = {
    "measure", "source", "set_children", "fragment_mut", "fragment_z_index",
    "allocate_fragment", "remove_fragment_subtree", "finish_box",
}

METHOD_RULES = [
    (r"^(layout_block|layout_anonymous|layout_float|float_side|cleared_y|is_out_of_flow|source_is_out_of_flow)", "block"),
    (r"^(layout_flex|distribute_flex|flex_|margin_is_auto|intrinsic_flex)", "flex"),
    (r"^(layout_grid|expand_grid|resolve_grid|grid_item|report_grid)", "grid"),
    (r"^(layout_inline|text_align|align_inline|collect_inline|measure_inline|intrinsic_text|text_style|layout_atomic|atomic_inline|atomic_outer|html_image|push_character|flush_text_run|max_content_width|replaced_size)", "inline"),
    (r"^(resolve_|apply_min_max|fragment_outer|translate_fragment|stretch_fragment|resize_fragment)", "resolve"),
]

FREE_FNS = {
    "parse_font_size": "inline", "parse_line_height": "inline",
    "parse_text_length": "inline", "align_offset": "inline",
    "justify_offsets": "inline", "inline_segment_end": "inline",
    "is_soft_line_break": "inline", "is_prohibited_line_end": "inline",
    "is_prohibited_line_start": "inline", "is_wide_character": "inline",
    "image_dimension_to_f32": "inline", "count_as_f32": "resolve",
    "establishes_block_formatting_context": "block",
    "float_band": "block", "inline_float_band": "block",
    "float_line_band": "block",
    "grid_template": "grid", "grid_length_depends_on_percentage": "grid",
    "position": "resolve",
}

MOD_ITEMS = {
    "TextStyle", "TextMeasure", "TextMeasurer", "SimpleTextMeasurer",
    "impl:SimpleTextMeasurer", "LayoutLimits", "impl:LayoutLimits",
    "LayoutOptions", "impl:LayoutOptions", "LayoutDiagnosticCode",
    "LayoutDiagnostic", "LayoutOutput", "layout_formatting_tree",
    "layout_formatting_tree_with_images", "Solver", "BlockResult",
    "FlexItem", "GridItem", "FloatArea", "AutoEdge", "InlineAtom",
    "TextRun",
}

# ---------------------------------------------------------------- parse

lines = SRC.read_text(encoding="utf-8").split("\n")

inner_attrs = []
i = 0
while lines[i].startswith("//!") or lines[i] == "":
    if lines[i].startswith("//!"):
        inner_attrs.append(lines[i])
    i += 1

use_lines = []
while i < len(lines):
    line = lines[i]
    if line.startswith("use "):
        stmt = [line]
        while not stmt[-1].rstrip().endswith(";"):
            stmt.append(lines[i + 1])
            i += 1
        use_lines.extend(stmt)
    elif line.strip() == "":
        pass
    else:
        break
    i += 1

items = []
pending = []
while i < len(lines):
    line = lines[i]
    if line.strip() == "":
        i += 1
        continue
    if re.match(r"^(#\[|///|//[^/])", line):
        pending.append(line)
        i += 1
        depth = line.count("[") - line.count("]")
        while depth > 0:
            pending.append(lines[i])
            depth += lines[i].count("[") - lines[i].count("]")
            i += 1
        continue
    m = re.match(r"^(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:struct|enum|fn|mod|trait|type|const|static)\s+(\w+)", line)
    mi = re.match(r"^impl(?:<[^>]*>)?\s+(?:\w+\s+for\s+)?(\w+)", line)
    if m:
        key = m.group(1)
    elif mi:
        key = f"impl:{mi.group(1)}"
    else:
        raise SystemExit(f"unparsed top-level line {i+1}: {line!r}")
    body = [line]
    i += 1
    if not line.rstrip().endswith(";"):
        while not body[-1].rstrip().endswith("{") and not body[-1].rstrip().endswith(";"):
            body.append(lines[i])
            i += 1
        if body[-1].rstrip().endswith("{"):
            while lines[i] != "}":
                body.append(lines[i])
                i += 1
            body.append("}")
            i += 1
    items.append((key, pending + body))
    pending = []

item_map = {key: text for key, text in items}

impl_solver = item_map.pop("impl:Solver")
IMPL_HEADER = impl_solver[0]
methods = {}
method_order = []
body = impl_solver[1:-1]
j = 0
while j < len(body):
    line = body[j]
    if line.strip() == "":
        j += 1
        continue
    if re.match(r"^    #\[", line):
        depth = line.count("[") - line.count("]")
        while depth > 0:
            j += 1
            depth += body[j].count("[") - body[j].count("]")
        j += 1
        continue
    if re.match(r"^    (?:///|//)", line):
        j += 1
        continue
    m = re.match(r"^    (?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?fn (\w+)", line)
    if not m:
        raise SystemExit(f"unexpected line in impl Solver: {line!r}")
    start = j
    k = j - 1
    while k >= 0 and body[k].strip() != "" and body[k] != "    }":
        start = k
        k -= 1
    end = j
    while body[end] != "    }":
        end += 1
    methods[m.group(1)] = body[start:end + 1]
    method_order.append(m.group(1))
    j = end + 1

# ---------------------------------------------------------------- assign

def method_file(name):
    if name in MOD_KEEP:
        return "mod"
    for rule, target in METHOD_RULES:
        if re.match(rule, name):
            return target
    raise SystemExit(f"unassigned method: {name}")

assigned_methods = {}
for name in method_order:
    assigned_methods.setdefault(method_file(name), []).append(name)

assigned_items = {}
for key in item_map:
    if key in ("impl:Solver", "tests"):
        continue
    if key in MOD_ITEMS:
        assigned_items.setdefault("mod", []).append(key)
        continue
    target = FREE_FNS.get(key)
    if target is None:
        raise SystemExit(f"unassigned item: {key}")
    assigned_items.setdefault(target, []).append(key)

# ---------------------------------------------------------------- emit

def normalize_uses():
    entries = []
    text = "\n".join(use_lines)
    for stmt in re.findall(r"use ([^;]+);", text, re.S):
        stmt = " ".join(stmt.split())
        if " as " in stmt:
            p, alias = stmt.split(" as ")
            entries.append((" ".join(p.split()), alias.strip()))
        elif "{" in stmt:
            path, names = stmt.split("{", 1)
            path = path.strip().rstrip(",").rstrip(":").strip()
            for name in names.rstrip("}").split(","):
                name = name.strip()
                if name:
                    entries.append((f"{path}::{name}" if path else name, name))
        else:
            entries.append((stmt, stmt.split("::")[-1]))
    return [(p.replace("super::", "crate::layout::"), n) for p, n in entries]

HEADER_USES = normalize_uses()

ALL_NAMES = {}
for file_, keys in assigned_items.items():
    for key in keys:
        ALL_NAMES[key] = file_

def imports_for(file_, body_text):
    needed = []
    for path, name in HEADER_USES:
        if re.search(rf"\b{re.escape(name)}\b", body_text):
            needed.append(f"use {path};")
    for name, def_file in ALL_NAMES.items():
        if def_file != file_ and re.search(rf"\b{re.escape(name)}\b", body_text):
            base = "crate::layout::solver" if def_file == "mod" else f"crate::layout::solver::{def_file}"
            needed.append(f"use {base}::{name};")
    def key(line):
        if line.startswith("use std"):
            return (0, line)
        if line.startswith("use crate"):
            return (2, line)
        return (1, line)
    return sorted(set(needed), key=key)

def add_visibility(text_lines, vis):
    text_lines = [l.replace("super::", "crate::layout::") for l in text_lines]
    is_struct = any(re.match(r"^(?:pub(?:\([^)]*\))?\s+)?struct ", l) for l in text_lines)
    out = []
    for line in text_lines:
        if re.match(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(fn |struct |enum |trait |type |const |static )", line) and "pub" not in line:
            line = re.sub(r"^(\s*)", f"\\1{vis} ", line)
        elif is_struct and re.match(r"^\s{4}\w+\s*:", line) and "pub" not in line:
            line = re.sub(r"^(\s{4})", f"\\1{vis} ", line)
        out.append(line)
    return out

generated = {}

def emit_file(file_, chunks):
    body_text = "\n".join("\n".join(c) for c in chunks)
    out_lines = list(inner_attrs)
    if inner_attrs:
        out_lines.append("")
    out_lines += imports_for(file_, body_text)
    out_lines.append("")
    for c in chunks:
        out_lines += c
        out_lines.append("")
    generated[file_] = out_lines

# mod.rs: types + Solver struct + kept methods (all verbatim)
chunks = []
for k in assigned_items.get("mod", []):
    chunks.append(item_map[k])
mod_methods = assigned_methods.get("mod", [])
if mod_methods:
    block = [IMPL_HEADER]
    for name in mod_methods:
        block += methods[name]
        block.append("")
    block.append("}")
    chunks.append(block)
emit_file("mod", chunks)

# domain files
for file_ in FILES:
    chunks = []
    for k in assigned_items.get(file_, []):
        chunks.append(add_visibility(item_map[k], VIS_DIRECT))
    method_block = []
    for name in assigned_methods.get(file_, []):
        method_block += add_visibility(methods[name], VIS_DIRECT)
        method_block.append("")
    if method_block:
        chunks.append([IMPL_HEADER] + method_block + ["}"])
    emit_file(file_, chunks)

# mod.rs extras: module decls (types stay here, no re-exports needed)
mod_lines = generated["mod"]
insert_at = max(idx for idx, l in enumerate(mod_lines) if l.startswith("use ")) + 1
decls = ["", "mod block;", "mod flex;", "mod grid;", "mod inline;", "mod resolve;", "",
         "#[cfg(test)]", "mod tests;"]
mod_lines[insert_at:insert_at] = decls

OUT.mkdir(parents=True, exist_ok=True)
for file_, text in generated.items():
    (OUT / f"{file_}.rs").write_text("\n".join(text) + "\n", encoding="utf-8")

# tests.rs: inner content of the original `mod tests`
tests_text = item_map["tests"]
mod_line = next(i for i, l in enumerate(tests_text) if l.startswith("mod tests"))
inner = [l.replace("super::", "crate::layout::solver::") for l in tests_text[mod_line + 1:-1]]
(OUT / "tests.rs").write_text("\n".join(inner) + "\n", encoding="utf-8")

name_map = {}
for name, def_file in ALL_NAMES.items():
    base = "crate::layout::solver" if def_file == "mod" else f"crate::layout::solver::{def_file}"
    name_map[name] = base
(ROOT / "tools" / "names_solver.json").write_text(json.dumps(name_map, indent=1), encoding="utf-8")

SRC.unlink()
print(f"wrote {len(generated) + 1} files; methods: {len(methods)}")
