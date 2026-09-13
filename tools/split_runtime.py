"""One-shot migration: split js/runtime.rs into a runtime/ module tree.

Pure move: method bodies are copied verbatim; only visibility tokens,
use statements, and the native-dispatch match are reorganized.
Relies on rustfmt invariants: items close at column 0, methods at 4
spaces, match arms at 12 spaces.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/render-core/src/js/runtime.rs"
OUT = ROOT / "crates/render-core/src/js/runtime"
BUILTINS = OUT / "builtins"

VIS = "pub(in crate::js::runtime)"  # items inside builtins/*
VIS_DIRECT = "pub(super)"           # items in direct children of runtime

BUILTIN_FILES = [
    "dom", "events", "observers", "style", "timers", "global_fns", "url",
    "array", "typed_array", "collections", "object", "math", "string",
    "regexp", "promise", "date", "json",
]
CHAIN = ["dom", "events", "array", "typed_array", "collections", "string",
         "regexp", "object", "math", "json", "date", "promise", "url",
         "style", "timers", "observers"]

MOD_KEEP = {
    "new", "with_url", "with_limits", "with_limits_and_url", "debug_call_stack",
    "take_pending_microtasks", "take_pending_timer_requests",
    "take_console_messages", "take_pending_navigations",
    "install_element_geometry", "install_viewport", "has_active_timers",
    "fire_timer", "dispatch_dom_event", "invoke_microtask", "execute",
    "execute_compiled", "consume_step", "call_depth_exceeded", "range_error",
    "request_location_navigation", "realm", "binding_trace_enabled",
    "depth_trace_enabled", "statement_trace_enabled",
    "MAX_TYPED_ARRAY_ELEMENTS",
}

METHOD_RULES = [
    (r"^(typed_array_|typed_index|typed_range|typed_from_index|create_typed_array)", "typed_array"),
    (r"^(array_|set_array_|sort_order|create_array_from_values|array_like_length)", "array"),
    (r"^collection_", "collections"),
    (r"^(object_|error_constructor|error_to_string)", "object"),
    (r"^math_", "math"),
    (r"^(string_|string_wrapper|require_string_)", "string"),
    (r"^(construct_regex|regex_|regexp_|coerce_pattern_argument)", "regexp"),
    (r"^(url_constructor|url_search|url_to_string|percent_encode|percent_decode)", "url"),
    (r"^(create_promise|resolve_promise|reject_promise|settle_promise|enqueue_reaction|perform_promise_then)", "promise"),
    (r"^(json_node_to_value|json_stringify_value)", "json"),
    (r"^(date_|require_date_value|now_ms|monotonic_now_ms|parse_date_string|format_date)", "date"),
    (r"^(event_constructor|event_target_node|event_prevent_default|add_event_listener|remove_event_listener|dispatch_event|dispatch_prepared_event|add_window_listener|remove_window_listener)", "events"),
    (r"^(intersection_|mutation_|queue_mutation|notify_|queue_intersection|has_ancestor)", "observers"),
    (r"^(inline_declarations|write_inline_declarations|style_|require_style_declaration)", "style"),
    (r"^(register_timer|cancel_timer)", "timers"),
    (r"^console_write", "global_fns"),
    (r"^(require_document|require_node|require_object|value_as_node|query_root|find_element_by|class_list_|require_class_list|wrap_node|text_content|set_text_content|set_inner_html|set_outer_html|clone_node|import_dom_subtree|element_rect_value|rect_value|image_constructor)", "dom"),
    (r"^(ensure_heap_capacity|collect_garbage)", "gc"),
    (r"^(optional_callable|require_callable_object|is_callable_object)", "eval"),
    (r"^(evaluate_|evaluate$|instantiate_|call$|call_native$|create_user_function|create_arrow_function|create_function|create_object_rest|resolve_assignment_reference|assign_destructuring_target|read_assignment_reference|write_assignment_reference|numeric_primitive|to_numeric_primitive|binding_exists|evaluate_binary_values|property_in|instanceof|create_binding|initialize_binding|lookup_binding|assign_binding|get_member|set_member|construct|construct_dispatch|call_with_this|call_user|function_call|function_bind|coerce_member_base)", "eval"),
]

FREE_FNS = {
    "gc_trace_enabled": "gc", "listener_values": "gc", "mark_value": "gc",
    "mark_object": "gc", "mark_environment": "gc", "mark_function": "gc",
    "mark_promise": "gc", "mark_host": "gc",
    "json_quote": "json",
    "days_from_civil": "date",
    "optional_index": "convert", "valid_position": "string",
    "char_at_value": "string", "slice_range": "convert",
    "collect_global_matches": "string", "split_by_regex": "string",
    "expand_replacement": "string", "string_method_native": "string",
    "is_string_native": "string", "js_math_round": "math",
    "to_number": "convert", "format_number_precision": "convert",
    "array_index": "array", "to_uint32": "convert", "to_int32": "convert",
    "bitwise_binary": "convert", "shift_count": "convert",
    "shift_left": "convert", "shift_right": "convert",
    "unsigned_shift_right": "convert", "compare": "convert",
    "strict_equal": "convert", "number_equal": "convert",
    "same_value_zero": "convert", "dom_contains": "dom",
    "find_body_node": "dom", "optional_timer_id": "timers",
    "is_valid_property_name": "dom", "css_prop_from_member": "dom",
    "node_attribute_property": "dom", "node_boolean_property": "dom",
    "abstract_equal": "convert", "required_argument": "convert",
    "valid_html_local_name": "dom",
}

ITEM_MAP = {
    "Environment": "types", "MAX_BUFFERED_CONSOLE_MESSAGES": "types",
    "CollectionView": "collections", "PromiseState": "promise",
    "Completion": "eval", "AssignmentReference": "eval",
    "collect_var_names": "eval", "JsonNode": "json",
    "STYLE_METHOD_PROPERTIES": "style",
    "DOCUMENT_POSITION_DISCONNECTED": "dom", "DOCUMENT_POSITION_PRECEDING": "dom",
    "DOCUMENT_POSITION_FOLLOWING": "dom", "DOCUMENT_POSITION_CONTAINS": "dom",
    "DOCUMENT_POSITION_CONTAINED_BY": "dom",
    "DATE_WEEKDAYS": "date", "DATE_MONTHS": "date", "CloneSource": "dom",
    "Binding": "types", "GlobalBinding": "types", "ObjectEntryKind": "types",
    "impl:ObjectEntryKind": "types", "EnvironmentRecord": "types",
    "UserFunction": "types", "PromiseReaction": "types",
    "PromiseRecord": "types", "JsMicrotask": "types", "TimerKind": "types",
    "TimerEntry": "types", "TimerRequest": "types",
    "NavigationRequest": "types", "RegexRecord": "types",
    "ConsoleLevel": "types", "impl:ConsoleLevel": "types",
    "ConsoleMessage": "types", "ElementRect": "types",
    "JsRuntime": "mod",  # the struct definition
    "JsonParser": "json", "impl:JsonParser": "json",
    "impl:JsValue": "convert", "impl:From": "mod",  # impl From<DomError> for JsError
}

VARIANT_RULES = [
    (r"^(Url)", "url"),
    (r"^(Global)", "global_fns"),
    (r"^(Num|Bool|Symbol|Error|Object)", "object"),
    (r"^(Css)", "global_fns"),
    (r"^(String)", "string"),
    (r"^(Array)", "array"),
    (r"^(Math|Number)", "math"),
    (r"^(Json)", "json"),
    (r"^(Date)", "date"),
    (r"^(Promise)", "promise"),
    (r"^(Map|Set|WeakMap|WeakSet|Collection)", "collections"),
    (r"^(TypedArray|Int8|Uint8|Int16|Uint16|Int32|Uint32|Float32|Float64|BigInt64|BigUint64)", "typed_array"),
    (r"^(Regex|RegExp)", "regexp"),
    (r"^(Function)", "global_fns"),
    (r"^(Event|Window|Animation)", "events"),
    (r"^(Intersection|Mutation)", "observers"),
    (r"^(Style)", "style"),
    (r"^(Timer|Timeout|Interval)", "timers"),
    (r"^(GetElementById|QuerySelector|QuerySelectorAll|GetElementsByTagName|GetElementsByClassName|CloneNode|NamedMap|Attr|CreateTextNode|CreateDocumentFragment|GetComputedStyle|CompareDocumentPosition|CreateElement|SetAttribute|GetAttribute|HasAttribute|RemoveAttribute|AppendChild|RemoveChild|InsertBefore|RemoveNode|Contains|Matches|Click|Node|Element|Document|Form|ClassList|TextContent|InnerHtml|OuterHtml|Rect|Offset|Client|Scroll|Image)", "dom"),
    (r"^(Console)", "global_fns"),
]

# ---------------------------------------------------------------- parse

lines = SRC.read_text(encoding="utf-8").split("\n")

inner_attrs = []
i = 0
if lines[0].startswith("#!["):
    inner_attrs.append(lines[0])
    i = 1
    while lines[i - 1] != ")]":
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

items = []  # (key, [text lines])
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
    m = re.match(r"^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|fn|mod|trait|type|const|static)\s+(\w+)", line)
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
        # consume a multi-line signature until the opening brace line
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

impl_jsruntime = item_map.pop("impl:JsRuntime")
methods = {}
method_order = []
body = impl_jsruntime[1:-1]
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
        j += 1  # attributes are attached by the next method's backward walk
        continue
    mc = re.match(r"^    (?:pub(?:\([^)]*\))?\s+)?const (\w+)", line)
    if mc and mc.group(1) != "fn":
        start = j
        k = j - 1
        while k >= 0 and body[k].strip() != "" and body[k] != "    }":
            start = k
            k -= 1
        end = j
        while not body[end].rstrip().endswith(";"):
            end += 1
        methods[mc.group(1)] = body[start:end + 1]
        method_order.append(mc.group(1))
        j = end + 1
        continue
    m = re.match(r"^    (?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?fn (\w+)", line)
    if not m:
        raise SystemExit(f"unexpected line in impl JsRuntime: {line!r}")
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

# explode call_native_dispatch into arms
cnd = methods.pop("call_native_dispatch")
cb = cnd[1:-1]
mstart = next(idx for idx, l in enumerate(cb) if l.strip() == "match function {")
match_close = next(idx for idx in range(mstart + 1, len(cb)) if cb[idx] == "        }")
arms = []
k = mstart + 1
while k < match_close:
    if not re.match(r"^            (NativeFunction::|_)", cb[k]):
        k += 1
        continue
    arm_start = k
    while "=>" not in cb[k]:
        k += 1
    pattern = cb[arm_start:k + 1]
    variants = re.findall(r"NativeFunction::(\w+)", "\n".join(pattern)) or ["_WILDCARD"]
    k += 1
    arm_body = []
    while k < match_close and cb[k] != "        }" and not re.match(r"^            (NativeFunction::|_)", cb[k]):
        arm_body.append(cb[k])
        k += 1
    while arm_body and arm_body[-1].strip() == "":
        arm_body.pop()
    arms.append((variants, pattern, arm_body))

# moved code lives deeper in the tree: original `super::` (the js module)
# becomes an absolute path
arms = [
    (v,
     [l.replace("super::", "crate::js::") for l in p],
     [l.replace("super::", "crate::js::") for l in b])
    for v, p, b in arms
]

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
    if name == "call_native_dispatch":
        continue
    assigned_methods.setdefault(method_file(name), []).append(name)

assigned_items = {}
for key in item_map:
    if key in ("impl:JsRuntime", "tests"):
        continue
    target = ITEM_MAP.get(key) or FREE_FNS.get(key)
    if target is None:
        raise SystemExit(f"unassigned item: {key}")
    assigned_items.setdefault(target, []).append(key)

def variant_file(variant):
    for rule, target in VARIANT_RULES:
        if re.match(rule, variant):
            return target
    return "global_fns"

arm_assignment = {}
for variants, pattern, arm_body in arms:
    if "_WILDCARD" in variants:
        raise SystemExit("wildcard arm found; map it manually")
    targets = {variant_file(v) for v in variants}
    target = targets.pop() if len(targets) == 1 else "global_fns"
    if len(targets) > 0:
        print(f"  note: mixed arm {variants} -> {target}")
    arm_assignment.setdefault(target, []).append((variants, pattern, arm_body))

for f in BUILTIN_FILES:
    n = len(arm_assignment.get(f, []))
    if n:
        print(f"  builtins/{f}.rs: {n} arms")

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
    return [(p.replace("super::", "crate::js::"), n) for p, n in entries]

HEADER_USES = normalize_uses()

ALL_NAMES = {}
for file_, keys in assigned_items.items():
    for key in keys:
        ALL_NAMES[key] = file_

REEXPORTED = {"ConsoleLevel", "ConsoleMessage", "ElementRect", "JsMicrotask",
              "NavigationRequest", "TimerEntry", "TimerKind", "TimerRequest"}

def imports_for(file_, body_text):
    needed = []
    for path, name in HEADER_USES:
        if name == "_":
            if re.search(r"\bwrite!\(|\bwriteln!\(", body_text):
                needed.append("use std::fmt::Write as _;")
            continue
        if re.search(rf"\b{re.escape(name)}\b", body_text):
            needed.append(f"use {path};")
    for name, def_file in ALL_NAMES.items():
        if def_file != file_ and re.search(rf"\b{re.escape(name)}\b", body_text):
            if file_ == "mod" and name in REEXPORTED:
                continue  # brought into scope by the pub use below
            if def_file == "mod":
                base = "crate::js::runtime"
            elif def_file in BUILTIN_FILES:
                base = f"crate::js::runtime::builtins::{def_file}"
            else:
                base = f"crate::js::runtime::{def_file}"
            needed.append(f"use {base}::{name};")
    def key(line):
        if line.startswith("use std"):
            return (0, line)
        if line.startswith("use crate"):
            return (2, line)
        return (1, line)
    return sorted(set(needed), key=key)

def add_visibility(text_lines, vis):
    text_lines = [l.replace("super::", "crate::js::") for l in text_lines]
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

def vis_of(file_):
    return VIS if file_ in BUILTIN_FILES else VIS_DIRECT

# types.rs
emit_file("types", [add_visibility(item_map[k], VIS_DIRECT) for k in assigned_items.get("types", [])])

# dispatch chain (inside mod.rs's impl block). Each domain match delegates
# unknown variants to the next link; `global_fns` closes the chain and is
# exhaustive, so a missed arm is a compile error.
chain = [f for f in CHAIN if arm_assignment.get(f)] + ["global_fns"]
chain_lines = ["    fn call_native_dispatch("]
chain_lines += [
    "        &mut self,",
    "        dom: &mut Dom,",
    "        function: NativeFunction,",
    "        receiver: ObjectId,",
    "        arguments: &[JsValue],",
    "    ) -> Result<JsValue, JsError> {",
    f"        self.dispatch_{chain[0]}_native(dom, function, receiver, arguments)",
    "    }",
]

def wrap_methods(method_names, file_):
    if not method_names:
        return None
    block = ["impl JsRuntime {"]
    for name in method_names:
        block += methods[name] if file_ == "mod" else add_visibility(methods[name], vis_of(file_))
        block.append("")
    if file_ == "mod":
        block += chain_lines
    block.append("}")
    return block

# direct children with items and/or methods
for file_ in ["convert", "gc", "eval", "mod"]:
    chunks = []
    for k in assigned_items.get(file_, []):
        # `impl Trait for Type` items must stay verbatim: trait-impl items
        # cannot carry visibility tokens.
        if file_ == "mod" and k == "impl:From":
            chunks.append(item_map[k])
        elif file_ != "mod":
            chunks.append(add_visibility(item_map[k], vis_of(file_)))
        else:
            chunks.append(item_map[k])
    wrapped = wrap_methods(assigned_methods.get(file_, []), file_)
    if wrapped:
        chunks.append(wrapped)
    emit_file(file_, chunks)

# builtins
for file_ in BUILTIN_FILES:
    chunks = []
    arms_here = arm_assignment.get(file_, [])
    if arms_here and file_ == "global_fns":
        block = ["impl JsRuntime {"]
        block.append(f"    {VIS} fn dispatch_residual_native(")
        block += [
            "        &mut self,",
            "        dom: &mut Dom,",
            "        function: NativeFunction,",
            "        receiver: ObjectId,",
            "        arguments: &[JsValue],",
            "    ) -> Result<JsValue, JsError> {",
            "        match function {",
        ]
        for variants, pattern, arm_body in arms_here:
            block += pattern + arm_body
        foreign = {}
        for owner, owner_arms in arm_assignment.items():
            if owner == "global_fns":
                continue
            for fvariants, _, _ in owner_arms:
                for v in fvariants:
                    foreign[v] = owner
        for v, owner in sorted(foreign.items()):
            block.append(
                f"            NativeFunction::{v} => self.dispatch_{owner}_native(dom, NativeFunction::{v}, receiver, arguments),"
            )
        block += ["        }", "    }", "}"]
        chunks.append(block)
    elif arms_here:
        next_link = chain[chain.index(file_) + 1]
        next_fn = "residual" if next_link == "global_fns" else next_link
        block = ["impl JsRuntime {"]
        block.append(f"    {VIS} fn dispatch_{file_}_native(")
        block += [
            "        &mut self,",
            "        dom: &mut Dom,",
            "        function: NativeFunction,",
            "        receiver: ObjectId,",
            "        arguments: &[JsValue],",
            "    ) -> Result<JsValue, JsError> {",
            "        match function {",
        ]
        for variants, pattern, arm_body in arms_here:
            block += pattern + arm_body
        block += [
            f"            other => self.dispatch_{next_fn}_native(dom, other, receiver, arguments),",
            "        }",
            "    }",
            "}",
        ]
        chunks.append(block)
    for k in assigned_items.get(file_, []):
        chunks.append(add_visibility(item_map[k], vis_of(file_)))
    wrapped = wrap_methods(assigned_methods.get(file_, []), file_)
    if wrapped:
        chunks.append(wrapped)
    emit_file(file_, chunks)

# mod.rs extras: module decls and re-exports
mod_lines = generated["mod"]
insert_at = max(idx for idx, l in enumerate(mod_lines) if l.startswith("use ")) + 1
decls = ["", "mod builtins;", "mod convert;", "mod eval;", "mod gc;", "mod types;",
         "", "#[cfg(test)]", "mod tests;", "",
         "pub use types::{ConsoleLevel, ConsoleMessage, ElementRect, JsMicrotask,",
         "    NavigationRequest, TimerEntry, TimerKind, TimerRequest};"]
mod_lines[insert_at:insert_at] = decls


# write everything
OUT.mkdir(parents=True, exist_ok=True)
BUILTINS.mkdir(parents=True, exist_ok=True)
for file_, text in generated.items():
    target = BUILTINS / f"{file_}.rs" if file_ in BUILTIN_FILES else OUT / f"{file_}.rs"
    target.write_text("\n".join(text) + "\n", encoding="utf-8")

bmod = [
    "//! ECMAScript and Web API built-ins, grouped by specification area.",
    "//!",
    "//! Each file owns one API domain: its `NativeFunction` dispatch arms, its",
    "//! `impl JsRuntime` methods, and its private helpers. Adding a new Web API",
    "//! means adding a variant to `crate::js::value::NativeFunction`, creating",
    "//! the value where the host object is built, and handling it in this",
    "//! domain's `dispatch_*_native` match. `global_fns.rs` holds the residual",
    "//! global-object functions and stays exhaustive over the enum so that a",
    "//! missed arm is a compile error, not a runtime gap.",
    "",
]
for file_ in BUILTIN_FILES:
    bmod.append(f"pub(super) mod {file_};")
(BUILTINS / "mod.rs").write_text("\n".join(bmod) + "\n", encoding="utf-8")

# tests.rs: inner content of the original `mod tests`
tests_text = item_map["tests"]
mod_line = next(i for i, l in enumerate(tests_text) if l.startswith("mod tests"))
inner = [l.replace("super::", "crate::js::") for l in tests_text[mod_line + 1:-1]]
(OUT / "tests.rs").write_text("\n".join(inner) + "\n", encoding="utf-8")

name_map = {}
for name, def_file in ALL_NAMES.items():
    if def_file == "mod":
        name_map[name] = "crate::js::runtime"
    elif def_file in BUILTIN_FILES:
        name_map[name] = f"crate::js::runtime::builtins::{def_file}"
    else:
        name_map[name] = f"crate::js::runtime::{def_file}"
(ROOT / "tools" / "names.json").write_text(json.dumps(name_map, indent=1), encoding="utf-8")

SRC.unlink()
print(f"wrote {len(generated) + 2} files; methods: {len(methods)}")

# probe marker 12345
