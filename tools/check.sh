#!/usr/bin/env bash
# Run every check the project requires before finishing a change.
# Mirrors the CI pipeline (".github/workflows/rust.yml) so failures
# surface locally in the same order.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "All checks passed."
