#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$SCRIPT_DIR/target/release/worktree-claude-tui"

if [ ! -f "$BINARY" ]; then
    echo "Binary not found. Run ./build.sh first."
    exit 1
fi

# Pass the target directory to the binary.
# Default: the caller's current working directory.
exec "$BINARY" "${1:-$(pwd)}"
