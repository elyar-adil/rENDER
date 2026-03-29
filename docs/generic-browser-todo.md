# Generic Browser TODO

This file tracks missing engine capability that should be implemented generically in the browser core.

The rule is simple:

- Do not add site-specific render adapters.
- Do not fetch alternate per-site data feeds to fake a DOM.
- Do not delegate runtime rendering to another browser engine.

## Priority 0: Runtime Integrity

- Keep `engine.py` on a single generic render path: parse HTML, load resources, execute supported JS, compute style, layout, paint.
- Reject browser-snapshot fallbacks, remote prerender services, and host-based special cases in runtime code.
- Keep real-browser usage limited to test tooling and visual comparison helpers only.

## Priority 1: JavaScript Execution

- Add a real event loop model for macro/microtasks.
- Implement `Promise`, async continuation scheduling, and timer semantics correctly enough for modern app bootstraps.
- Expand language coverage for modern syntax used by current frameworks.
- Improve module-script support, including dependency loading and execution order.

## Priority 2: DOM and Web APIs

- Expand DOM mutation APIs used by hydration frameworks.
- Implement missing query, traversal, and attribute reflection behavior.
- Add event dispatch/bubbling/capture behavior closer to browsers.
- Improve network-facing APIs needed by app bootstraps such as `fetch`-adjacent behavior if the engine chooses to support them.

## Priority 3: Custom Elements and Shadow DOM

- Implement custom element registration and upgrade timing.
- Support shadow roots and shadow tree attachment.
- Implement slotting and basic shadow DOM traversal rules needed for rendering.
- Add test coverage for declarative and imperative shadow DOM paths.

## Priority 4: Resource Loading Model

- Make stylesheet, script, image, and module loading order more browser-accurate.
- Model blocking vs deferred script behavior more precisely.
- Add caching and retry behavior without changing render semantics per site.

## Priority 5: Layout and Painting Gaps

- Continue improving flex, grid, replaced elements, transforms, sticky positioning, and pseudo-elements through generic tests.
- Add more interoperable handling for intrinsic sizing and shrink-to-fit behavior.
- Improve SVG-as-image support and other replaced content behavior.

## Priority 6: Compatibility Test Strategy

- Convert every real-page failure into a generic reduced test when possible.
- Keep fixture regressions, but map each fix back to an engine capability rather than a site name.
- Use WebKit-style and reduced fixtures to prove behavior, not host checks.

## Current Examples of Valid Generic Work

- Float shrink-to-fit fixes.
- Deferred absolute/fixed layout fixes.
- Better handling of invalid inline wrappers around block descendants.
- TLS/network robustness that does not branch on specific sites.

## Current Examples of Invalid Work

- `if host == "msn.cn": ...`
- Fetching a page-specific JSON feed and synthesizing replacement HTML.
- Taking screenshots with Edge/Chromium and painting the bitmap as the page.
