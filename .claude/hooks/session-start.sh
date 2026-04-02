#!/bin/bash
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"

pip install -q -r requirements-dev.txt

echo 'export PYTHONPATH="$CLAUDE_PROJECT_DIR"' >> "$CLAUDE_ENV_FILE"
echo 'export QT_QPA_PLATFORM="offscreen"' >> "$CLAUDE_ENV_FILE"
