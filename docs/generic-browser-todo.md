# Generic Browser TODO

This file tracks missing engine capability that should be implemented generically in the browser core.

The rule is simple:

- Do not add site-specific render adapters.
- Do not fetch alternate per-site data feeds to fake a DOM.
- Do not delegate runtime rendering to another browser engine.

These three are enforced mechanically, not by review: `tools/check_site_neutrality.py`
runs in CI and fails the build when a **site identifier in a decision position** appears in
engine source. A decision position is a comparison, a match arm, a substring test, or a
site-selecting environment or cargo feature. The identifier may be a domain, a brand token
with no TLD, a bare address, or an identifier split across a `concat!` - the obvious
evasion is not writing the domain literally, so the obvious form alone is not enough.

Reserved names and addresses are allowlisted: RFC 2606 domains, RFC 5737 documentation
ranges, loopback, and the specification URIs that correct namespace handling compares
against. The allowlist is applied to the *matched identifier*, not to the line, so one
reserved token cannot excuse an unrelated site identifier on the same line. A genuine
exception declares itself with a `// site-neutral: <reason>` comment on the line or the two
lines above it.

`tools/check_site_neutrality_test.py` is the gate's own test. It runs first in CI, because
a gate that has silently stopped detecting reads as enforcement while enforcing nothing.
It covers 23 cases: 13 that must be flagged, including the evasions, and 10 that must not
be, including every allowlist entry.

**What the gate cannot see** - read this before trusting a clean run. It is a lint, not a
proof. It does not see a site identifier assembled at runtime rather than from literals, a
branch keyed on a site-specific class name or title containing no recognisable brand token
(`if class == "J-global-header"`), or a condition whose identifier is passed in from
elsewhere. Those need review, not this script. Add new identifier shapes to the script
rather than relying on reviewers to notice them.

## Priority -1: Gaps Are Implemented Forward, Never Removed

An unowned, unimplemented, or undocumented capability is a **TODO to implement**, not
something to delete, stub out, silently skip, or route around. This applies to code,
to tests, and to documentation.

- Never delete a working behaviour, test, or document to make a problem disappear.
- Never substitute a fake approximation for a real implementation and present it as
  done. If a correct implementation needs another crate or subsystem, report the
  blocker precisely and leave the honest gap visible.
- Never "fix" a gap by special-casing a host, a URL, or a document shape.
- A capability that is parsed but not consumed, or computed but dropped, is a defect
  with a location. Track it; do not pretend the parse implies the behaviour.
- Stale documentation is a defect too. When the architecture moves, rewrite the
  document to the new truth - including its file paths, crate names, and API shapes.
  A document that describes a retired architecture is worse than no document, because
  it actively misleads the next reader (and the next automated agent).

Standards are the authority. A comment that cites a spec section must be true; a
false citation is worse than no citation, and a deviation from a spec must be
labelled as a deviation with its reason, not disguised as conformance.

## Priority 0: Runtime Integrity

- Keep the engine on a single generic render path: parse HTML, load resources, execute supported JS, compute style, layout, paint. The path lives in `render-core/src/page.rs` (the `Page` type) on top of the `render-dom` / `render-html` / `render-css` / `render-layout` / `render-js` crates.
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
