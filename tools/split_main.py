"""One-shot migration: split render-browser/src/main.rs into app modules.

Same rustfmt-invariant machinery. Binary-root modules: visibility token
pub(super) resolves to the whole binary. Trait impls stay verbatim.
"""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/render-browser/src/main.rs"
OUT = ROOT / "crates/render-browser/src"

VIS = "pub(super)"

FILES = ["app", "page_source", "fetch_handles", "render_worker", "page_state", "diagnostics"]

# key -> destination file (structs, enums, free fns, impl blocks, consts)
ITEM_MAP = {
    "CachedRequestState": "fetch_handles",
    "CachedRequestHandle": "fetch_handles", "impl:CachedRequestHandle": "fetch_handles",
    "CachedBatchHandle": "fetch_handles", "impl:CachedBatchHandle": "fetch_handles",
    "CachedFetchResult": "fetch_handles",
    "PageSource": "page_source", "load_initial_page": "page_source",
    "home_source": "page_source", "settings_source": "page_source",
    "network_start_source": "page_source", "source_from_local_file": "page_source",
    "status_source": "page_source", "error_source": "page_source",
    "source_from_network_response": "page_source", "escape_html": "page_source",
    "PageRenderPayload": "render_worker", "FullPageRenderPayload": "render_worker",
    "PageRenderFrame": "render_worker", "PageRenderWorker": "render_worker",
    "BrowserRasterControl": "render_worker", "impl:BrowserRasterControl": "render_worker",
    "start_render_worker": "render_worker", "process_page_render": "render_worker",
    "PageState": "page_state", "PageNavigation": "page_state",
    "impl:PageNavigation": "page_state", "impl:PageState": "page_state",
    "PendingNavigation": "page_state", "PendingStyleSheets": "page_state",
    "PendingScripts": "page_state", "PendingImages": "page_state",
    "HistoryMode": "app", "BrowserApp": "app", "impl:BrowserApp": "app",
    "impl:ApplicationHandler": "app", "ContentTextEditor": "app",
    "key_character_is": "app", "page_key_name": "app", "HostPlatform": "app",
    "primary_modifier_for": "app", "primary_modifier_active": "app",
    "wheel_document_delta_y": "app", "address_shortcut": "app",
    "finite_f32": "app", "theme_from_winit": "app",
    "log_completed_frame_debug": "diagnostics", "dump_debug_frame": "diagnostics",
    "report_stylesheet_diagnostics": "diagnostics",
    "report_script_diagnostics": "diagnostics",
    "report_script_discovery_diagnostics": "diagnostics",
    "report_image_diagnostics": "diagnostics",
    # root-kept items (visible to every sibling via `crate::`)
    "INITIAL_WIDTH": "root", "INITIAL_HEIGHT": "root", "SCROLL_LINE_PIXELS": "root",
    "ACTIVE_PAGE_TURN_BUDGET": "root", "BACKGROUND_PAGE_TURN_BUDGET": "root",
    "NativeSurface": "root", "UserEvent": "root",
}

ROOT_MODULES = {"frame", "content_interaction"}

# ---------------------------------------------------------------- parse

lines = SRC.read_text(encoding="utf-8").split("\n")

inner_attrs = []
i = 0
if lines[0].startswith("//!"):
    inner_attrs.append(lines[0])
    i = 1

use_lines = []
mod_decls = []
while i < len(lines):
    line = lines[i]
    if line.startswith("use ") or line.startswith("mod "):
        stmt = [line]
        while not stmt[-1].rstrip().endswith(";"):
            stmt.append(lines[i + 1])
            i += 1
        (mod_decls if line.startswith("mod ") else use_lines).extend(stmt)
    elif line.startswith("#!["):
        inner_attrs.append(line)
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

# ---------------------------------------------------------------- assign

assigned_items = {}
for key, text in item_map.items():
    if key in ("main", "browser_main", "tests"):
        continue
    target = ITEM_MAP.get(key)
    if target is None:
        raise SystemExit(f"unassigned item: {key}")
    assigned_items.setdefault(target, []).append(key)

# ---------------------------------------------------------------- emit

def normalize_uses():
    entries = []
    text = "\n".join(use_lines)
    for stmt in re.findall(r"use ([^;]+);", text, re.S):
        stmt = " ".join(stmt.split())
        if "{" in stmt:
            path, names = stmt.split("{", 1)
            path = path.strip().rstrip(",").rstrip(":").strip()
            for name in names.rstrip("}").split(","):
                name = name.strip()
                if not name:
                    continue
                if " as " in name:
                    orig, alias = [x.strip() for x in name.split(" as ")]
                    entries.append((f"{path}::{orig}" if path else orig, alias))
                else:
                    entries.append((f"{path}::{name}" if path else name, name))
        elif " as " in stmt:
            p, alias = stmt.split(" as ")
            entries.append((" ".join(p.split()), alias.strip()))
        else:
            entries.append((stmt, stmt.split("::")[-1]))
    fixed = []
    for p, n in entries:
        first = p.split("::")[0]
        if first in ROOT_MODULES:
            p = f"crate::{p}"
        fixed.append((p, n))
    return fixed

HEADER_USES = normalize_uses()

ALL_NAMES = {}
for file_, keys in assigned_items.items():
    for key in keys:
        ALL_NAMES[key] = file_
for root_name in ["INITIAL_WIDTH", "INITIAL_HEIGHT", "SCROLL_LINE_PIXELS",
                  "ACTIVE_PAGE_TURN_BUDGET", "BACKGROUND_PAGE_TURN_BUDGET",
                  "NativeSurface", "UserEvent"]:
    ALL_NAMES[root_name] = "root"

def base_of(def_file):
    return "crate" if def_file == "root" else f"crate::{def_file}"

def imports_for(file_, body_text):
    needed = []
    for path, name in HEADER_USES:
        if name in ROOT_MODULES:
            continue  # module names are used via crate::<mod> paths below
        if re.search(rf"\b{re.escape(name)}\b", body_text):
            last = path.split("::")[-1]
            if last == name:
                needed.append(f"use {path};")
            else:
                needed.append(f"use {path} as {name};")
    for name, def_file in ALL_NAMES.items():
        if def_file != file_ and re.search(rf"\b{re.escape(name)}\b", body_text):
            if file_ == "main" and def_file == "root":
                continue  # defined right here
            needed.append(f"use {base_of(def_file)}::{name};")
    for mod_name in sorted(ROOT_MODULES):
        if re.search(rf"\b{mod_name}::", body_text):
            needed.append(f"use crate::{mod_name};")
    def key(line):
        if line.startswith("use std"):
            return (0, line)
        if line.startswith("use render") or line.startswith("use softbuffer") or line.startswith("use winit"):
            return (1, line)
        if line.startswith("use crate"):
            return (3, line)
        return (2, line)
    return sorted(set(needed), key=key)

def add_visibility(text_lines, vis):
    text_lines = [l.replace("super::", "crate::") for l in text_lines]
    is_struct = any(re.match(r"^(?:pub(?:\([^)]*\))?\s+)?struct ", l) for l in text_lines)
    out = []
    for line in text_lines:
        if re.match(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(fn |struct |enum |trait |type |const |static )", line) and "pub" not in line:
            line = re.sub(r"^(\s*)", f"\\1{vis} ", line)
        elif is_struct and re.match(r"^\s{4}\w+\s*:", line) and "pub" not in line:
            line = re.sub(r"^(\s{4})", f"\\1{vis} ", line)
        out.append(line)
    return out

def verbatim(key):
    """trait impls and generic wrappers cannot take visibility tokens"""
    text = item_map[key]
    return " for " in text[0]

generated = {}

def emit_file(file_, chunks, extra_header=None):
    body_text = "\n".join("\n".join(c) for c in chunks)
    out_lines = list(inner_attrs)
    if extra_header:
        out_lines += extra_header
    out_lines += imports_for(file_, body_text)
    out_lines.append("")
    for c in chunks:
        out_lines += c
        out_lines.append("")
    generated[file_] = out_lines

for file_ in FILES:
    chunks = []
    for k in assigned_items.get(file_, []):
        if verbatim(k):
            chunks.append(item_map[k])
        else:
            chunks.append(add_visibility(item_map[k], VIS))
    emit_file(file_, chunks)

# main.rs: header, mod decls, root consts, main, browser_main
main_chunks = []
for k in ["INITIAL_WIDTH", "INITIAL_HEIGHT", "SCROLL_LINE_PIXELS",
          "ACTIVE_PAGE_TURN_BUDGET", "BACKGROUND_PAGE_TURN_BUDGET", "NativeSurface",
          "UserEvent", "main", "browser_main"]:
    main_chunks.append(item_map[k])
new_mods = [f"mod {f_};" for f_ in FILES]
new_mods += ["", "#[cfg(test)]", "mod app_tests;"]
emit_file("main", main_chunks,
          extra_header=[l for l in mod_decls if l.startswith("mod ")] + new_mods + [""])

OUT.mkdir(parents=True, exist_ok=True)
SRC.unlink()
if "app" in generated:
    app_text = generated["app"]
    anchor = next(i2 for i2, l2 in enumerate(app_text)
                  if l2.startswith("use render_browser::editor::NativeClipboard;"))
    app_text.insert(anchor + 1, "use render_browser::editor::Clipboard;")
for file_, text in generated.items():
    (OUT / f"{file_}.rs").write_text("\n".join(text) + "\n", encoding="utf-8")

# tests.rs: inner content of the original `mod tests`
tests_text = item_map["tests"]
mod_line = next(i for i, l in enumerate(tests_text) if l.startswith("mod tests"))
inner = [l.replace("super::", "crate::") for l in tests_text[mod_line + 1:-1]]
# Expand grouped `use crate::{...}; imports into per-name paths.
EXTERN_REMAP = {
    "ElementRect": "render_core::js",
    "HOME_TITLE": "render_browser::home",
    "HOME_HTML": "render_browser::home",
    "SETTINGS_TITLE": "render_browser::settings",
    "FrameDamage": "crate::frame", "FrameRect": "crate::frame",
    "blit_page": "crate::frame", "copy_frame_regions": "crate::frame",
    "geometry_from_layout": "crate::frame",
    "surface_to_softbuffer": "crate::frame",
    "viewport_dimension": "crate::frame",
}
name_map = {}
for name, def_file in ALL_NAMES.items():
    name_map[name] = base_of(def_file)
name_map.update(EXTERN_REMAP)

def expand_group(mo):
    parts = []
    for name in mo.group(1).split(","):
        name = name.strip()
        if not name:
            continue
        base = name_map.get(name, "crate")
        parts.append(f"use {base}::{name};")
    parts_join = "\n".join(parts)
    return parts_join


group_re = re.compile(r"use crate::\{([^;]+)\};")
inner_text = "\n".join(inner)
text = group_re.sub(expand_group, inner_text)
expanded = text.split("\n")
(OUT / "app_tests.rs").write_text("\n".join(expanded) + "\n", encoding="utf-8")

name_map = {}
for name, def_file in ALL_NAMES.items():
    name_map[name] = base_of(def_file)
(ROOT / "tools" / "names_main.json").write_text(json.dumps(name_map, indent=1), encoding="utf-8")

print(f"wrote {len(generated) + 1} files")
