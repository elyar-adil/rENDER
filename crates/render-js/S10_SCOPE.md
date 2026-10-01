# S10 scope for the two items that must not be started yet

Written while landing `TextEncoder`/`TextDecoder`, `ArrayBuffer`/`DataView` and
`structuredClone`. These two are **scope only** — nothing here is implemented,
and nothing here should be implemented until the ordering below is satisfied.
Both fail in the same way if attempted from `render-js` alone: the JavaScript
API would exist, scripts would use it, and the output would be wrong.

## `ResizeObserver`

`IntersectionObserver` already works and is the template. It reads
`JsRuntime::element_geometry` (`runtime/mod.rs:114`), a
`BTreeMap<NodeId, ElementRect>` the embedding installs per frame through
`install_element_geometry` (`runtime/mod.rs:410`). `render-browser` fills it in
`frame.rs:206 geometry_from_layout`, which walks the fragment tree and records
**`box_geometry.border_rect()`** per source node.

`ResizeObserver` needs three things that do not exist yet, in dependency order:

1. **`render-layout` must report a content box, not only a border box.**
   `ElementRect` (`runtime/types.rs:318`) is a bare `{x, y, width, height}` and
   `geometry_from_layout` explicitly takes the border rect. The default
   `ResizeObserver` box is `content-box`, so the content size is
   `border - padding - border-width` on both axes, and that arithmetic needs
   the resolved padding and border widths from the cascade. Until layout
   publishes those, a JS-side `ResizeObserver` can only report the border box,
   which is wrong for any element with padding — and silently wrong, which is
   the outcome to avoid.
2. **The embedding must re-install geometry after every layout pass, not once.**
   The observer has to *diff* successive sizes per target to decide what
   changed, so the runtime needs a per-observer last-reported size and the
   browser worker has to push a fresh geometry map each frame. Today
   `install_element_geometry` replaces the whole map, which is fine for
   intersection (a full recompute every frame is always correct) but is not
   enough to express "only these targets changed".
3. **The delivery timing is a layout-loop question, not a JS one.** Entries must
   be delivered after layout and before paint, in one batch per observer, and
   `ResizeObserver` must also fire an initial callback on `observe()` before any
   layout has run. `MutationObserver` already has the batching machinery
   (`queue_mutation_deliveries`, `queue_intersection_observers`), so the
   plumbing is close, but *when* in the frame the runtime is invoked is a
   `render-browser` decision.

**What `render-js` would own once 1–3 land:** the constructor, `observe` /
   `unobserve` / `disconnect`, the `ResizeObserverEntry` and
   `ResizeObserverSize` shapes (`contentBoxSize` / `borderBoxSize` /
   `devicePixelContentBoxSize` are objects with `inlineSize`/`blockSize`, not
   numbers — a real trap, because code that reads them as numbers gets
   `undefined`), and the `@@toStringTag`. Roughly 200 lines in
   `runtime/builtins/observers.rs`, which is the point: the JS part is the small
   end. Doing it now yields an observer that fires on the wrong numbers.

## Shadow DOM (`customElements`, `attachShadow`, `ShadowRoot`)

This is `docs/generic-browser-todo.md` Priority 3 and it is a tree-model change,
not an API addition. The ordering is forced:

1. **`render-dom`: a second child list on an element.** A shadow root is a node
   whose children are *not* in the element's light-DOM child list. This is the
   foundation; nothing else can be correct until it exists, because every
   traversal below reads one child list.
2. **`render-core::document`: traversal, style resolution, and the event path.**
   `children`/`querySelector`/`getElementById` must see the shadow tree where
   the spec says they do, and — the part that is easy to get wrong — **style
   inheritance and the cascade must stop at the shadow boundary**: an element in
   a shadow root inherits from its shadow root's stylesheet, and document-level
   rules do not reach into it. `document.rs` builds `MatchContext` by naming all
   ten fields (S18), so this also needs the `..Default()` fix that S18 calls out
   or every new field breaks `render-browser`.
3. **`render-html`: `<template>` and the declarative form.** `template.content`
   parsing, plus `shadowrootmode` on `<template>` and on a host element.
4. **`render-layout`: the shadow tree as a box tree.** The shadow root gets its
   own layout context, and `<slot>` distributes light children into it. Until
   this exists a shadow tree renders as nothing, which is the specific failure
   named in the brief: scripts will use `attachShadow` and get blank output.
5. **`render-js` last:** `attachShadow`, `ShadowRoot`, `customElements.define` /
   `get` / `upgrade` / `whenDefined`, `slot` / `assignedNodes` /
   `assignedElements`, and the `slotchange` event.

**Upgrade timing** (when a definition retroactively upgrades already-parsed
elements, and when `connectedCallback` fires) is genuinely its own design
problem and should not be started before 1–2, because "when" depends on what
`document` traversal reports.

## Why not start from `render-js`

Both items' observable output is produced by other crates. A JS-only
`ResizeObserver` reports border boxes; a JS-only `attachShadow` creates a tree
that layout never visits. In both cases the global would exist, scripts would
not check it, and the page would render wrong — which is worse than the
`ReferenceError` the absence produces today, because the absence is loud and
the wrong answer is not.
