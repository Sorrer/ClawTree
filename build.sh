#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Building worktree-claude-tui (release)..."
cargo build --release

echo ""
echo "Done. Binary at: target/release/worktree-claude-tui"
