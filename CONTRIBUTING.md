# Contributing to rENDER

## Setup

Install a stable Rust toolchain (1.88 or newer; the code uses let-chains), then build the workspace:

```bash
cargo build --workspace
```

## Testing Philosophy

- A test passing should imply the page still renders correctly, not just that parsing succeeded.
- Standards are the authority: WHATWG/CSS/ECMAScript specs, WPT, and test262 outrank intuition.
- Prefer conformance-backed fixes: reproduce a gap with a pinned test262 or WPT case when possible.
- Keep behavior deterministic; avoid tests that depend on network access.

## Required Checks

Before sending a change, run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_site_neutrality.py
cargo test --workspace
```

All four must pass. CI enforces them. `tools/check.sh` runs them in the same order
locally.

`cargo test --workspace` includes the test262 conformance gate, which takes about eight
minutes and holds the build lock. While iterating, scope to `cargo test -p <crate> --lib`
or name the test targets explicitly.

## Change Guidelines

- Prefer small, test-backed changes over broad rewrites.
- Preserve behavior unless the change explicitly fixes a bug or improves standards conformance.
- Add or update regression tests for parser, cascade, layout, network, or engine changes.
- Do not revert unrelated work in a dirty tree.
- Delete dead code rather than working around it; there is no legacy implementation to stay compatible with.

### Dead code is not the same as a missing capability

These two rules point in opposite directions on purpose, and the distinction matters:

- **Dead code** - a helper, a variant, a branch that nothing can reach - should be
  deleted. Nothing depends on it.
- **A missing capability** - a feature that is parsed but never consumed, computed but
  dropped, or absent entirely - must be implemented forward or reported precisely. It
  is never deleted, stubbed out, silently skipped, or routed around, and a fake
  approximation is not an acceptable substitute for the real thing.

`crates/render-core/src/image.rs` has a declared-but-never-constructed
`SrcsetUnsupported` variant: that is dead code and a candidate for removal. The absence
of `hsl()` colour support is a missing capability and is not.

`docs/visual_fidelity_gaps.md` is the register of known missing capabilities, with
evidence. Add to it rather than closing a gap by deleting the thing that records it.

### No per-site branches

Missing capability is fixed generically in the engine, never by branching on a site.
`tools/check_site_neutrality.py` enforces this in CI: a real commercial domain in a
comparison, match arm, or substring test in engine source fails the build. A domain used
as inert test data is fine. A genuine exception declares itself with a
`// site-neutral: <reason>` comment.

## Review Expectations

Good contributions usually include:

- a failing test or a concrete reproduction
- the minimal code change needed to fix it
- an explanation of any tradeoff in semantics or compatibility
- verification output for the commands above
