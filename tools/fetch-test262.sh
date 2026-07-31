#!/bin/sh
set -eu

TEST262_REVISION="5ef1e5723be95296f36afb0386676fed0205869c"
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET="$ROOT/third_party/test262"
ARCHIVE="${TMPDIR:-/tmp}/test262-${TEST262_REVISION}.tar.gz"
EXTRACTED="${TMPDIR:-/tmp}/test262-${TEST262_REVISION}"

mkdir -p "$ROOT/third_party"
curl --fail --location --retry 2 \
  "https://codeload.github.com/tc39/test262/tar.gz/$TEST262_REVISION" \
  --output "$ARCHIVE"
rm -rf "$EXTRACTED"
tar -xzf "$ARCHIVE" -C "${TMPDIR:-/tmp}"
rm -rf "$TARGET"
mv "$EXTRACTED" "$TARGET"
printf '%s\n' "$TEST262_REVISION" > "$TARGET/.render-revision"
printf 'Fetched test262 revision %s\n' "$TEST262_REVISION"
