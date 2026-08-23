# Contributing to rENDER

## Setup

Install a stable Rust toolchain (1.85 or newer), then build the workspace:

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
cargo test --workspace
```

All three must pass. CI enforces them.

## Change Guidelines

- Prefer small, test-backed changes over broad rewrites.
- Preserve behavior unless the change explicitly fixes a bug or improves standards conformance.
- Add or update regression tests for parser, cascade, layout, network, or engine changes.
- Do not revert unrelated work in a dirty tree.
- Delete dead code rather than working around it; there is no legacy implementation to stay compatible with.

## Review Expectations

Good contributions usually include:

- a failing test or a concrete reproduction
- the minimal code change needed to fix it
- an explanation of any tradeoff in semantics or compatibility
- verification output for the commands above
