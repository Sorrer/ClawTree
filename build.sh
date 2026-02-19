#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Building clawtree (release)..."
cargo build --release

echo ""
echo "Done. Binary at: target/release/clawtree"
