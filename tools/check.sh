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

echo "==> site neutrality gate self-test"
python3 tools/check_site_neutrality_test.py

echo "==> site neutrality"
python3 tools/check_site_neutrality.py

echo "==> cargo test --workspace"
cargo test --workspace

# The full pinned test262 suite is too slow in the debug profile: the generated
# RegExp property-escape files exceed the per-file timeout even though they pass
# in release. It is ignored by the workspace test and measured here, where the
# pass count is comparable with the checked-in baseline.
echo "==> test262 conformance gate (release)"
cargo test --release -p render-core --test test262 \
  pinned_test262_manifest_reports_a_real_baseline -- --ignored --exact --nocapture

# The acceptance harness is the project's only measurement instrument, so a gate that does
# not run it is a gate that cannot tell whether a change helped. It is a separate workspace
# with its own lock and its own target directory, hence the explicit manifest path.
#
# The five remaining ignored tests are each mapped to a named gap in
# docs/visual_fidelity_gaps.md. They are not noise to be cleared - they are the list of
# what this engine cannot yet do, and they stop being ignored as each gap closes.
echo "==> acceptance harness, static gate"
python3 tests/test_real_site_capabilities.py

echo "==> acceptance harness, contract"
cargo test --manifest-path tests/real_site_tasks/Cargo.toml

echo "All checks passed."
