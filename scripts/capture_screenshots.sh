#!/usr/bin/env bash
#
# Automated screenshot capture for ClawTree README.
# Creates a temp bare repo, launches clawtree in tmux, navigates to each
# feature state, and captures screenshots via term_capture.py.
#
# Usage: ./scripts/capture_screenshots.sh
#
# Requirements: tmux, python3 (pyte, Pillow), built clawtree binary
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/clawtree"
CAPTURE="$SCRIPT_DIR/term_capture.py"
OUTPUT_DIR="$PROJECT_DIR/docs/screenshots"
TMUX_SESSION="clawtree-capture"
COLS=140
ROWS=40
WAIT=3  # seconds to wait after each action for TUI to settle

# ── Helpers ──────────────────────────────────────────────────────────────

die() { echo "ERROR: $*" >&2; exit 1; }

cleanup() {
    echo "Cleaning up..."
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
    if [[ -n "${TEMP_DIR:-}" && -d "${TEMP_DIR:-}" ]]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

send_keys() {
    tmux send-keys -t "$TMUX_SESSION" "$@"
}

send_literal() {
    # Send a literal string character by character
    tmux send-keys -t "$TMUX_SESSION" -l "$@"
}

wait_settle() {
    sleep "${1:-$WAIT}"
}

capture() {
    local name="$1"
    local title="${2:-ClawTree}"
    echo "  Capturing: $name"
    python3 "$CAPTURE" "$TMUX_SESSION" "$OUTPUT_DIR/$name" \
        --mode tmux --title "$title" --cols "$COLS" --rows "$ROWS"
}

# ── Preflight checks ────────────────────────────────────────────────────

[[ -x "$BINARY" ]] || die "Binary not found: $BINARY (run ./build.sh first)"
command -v tmux >/dev/null || die "tmux is required"
command -v python3 >/dev/null || die "python3 is required"
python3 -c "import pyte; from PIL import Image" 2>/dev/null || die "Python pyte + Pillow required"

mkdir -p "$OUTPUT_DIR"

# ── Create temp bare repo with realistic content ─────────────────────────

echo "Setting up temp bare repo..."
TEMP_DIR=$(mktemp -d)

# Initialize bare repo structure
mkdir -p "$TEMP_DIR/.bare"
git init --bare "$TEMP_DIR/.bare"
echo "gitdir: ./.bare" > "$TEMP_DIR/.git"

# Configure worktrees dir so git places them correctly
git -C "$TEMP_DIR/.bare" config core.worktree "$TEMP_DIR"

# Create initial commit in bare repo
WORK_INIT=$(mktemp -d)
git clone "$TEMP_DIR/.bare" "$WORK_INIT" 2>/dev/null
(
    cd "$WORK_INIT"
    git config user.email "demo@clawtree.dev"
    git config user.name "Demo User"

    # Create realistic project files
    mkdir -p src tests docs
    cat > src/main.rs << 'RUST'
use std::io;

fn main() {
    println!("Hello from ClawTree demo!");
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    println!("You said: {}", input.trim());
}
RUST
    cat > src/lib.rs << 'RUST'
pub mod config;
pub mod session;
pub mod worktree;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
RUST
    cat > Cargo.toml << 'TOML'
[package]
name = "demo-project"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
TOML
    cat > README.md << 'MD'
# Demo Project
A sample project managed by ClawTree.
MD
    cat > tests/test_basic.rs << 'RUST'
#[test]
fn test_version() {
    assert!(!demo_project::version().is_empty());
}
RUST

    git add -A
    git commit -m "Initial project setup"

    # Create a feature branch
    git checkout -b feature/auth
    cat > src/auth.rs << 'RUST'
pub struct AuthConfig {
    pub api_key: String,
    pub timeout: u64,
}

impl AuthConfig {
    pub fn new(api_key: String) -> Self {
        Self { api_key, timeout: 30 }
    }
}
RUST
    echo 'pub mod auth;' >> src/lib.rs
    git add -A
    git commit -m "Add authentication module"

    # Create another branch
    git checkout main
    git checkout -b fix/logging
    cat > src/logger.rs << 'RUST'
use std::fs::OpenOptions;
use std::io::Write;

pub fn log(msg: &str) {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open("app.log")
        .unwrap();
    writeln!(file, "{}", msg).unwrap();
}
RUST
    git add -A
    git commit -m "Add logging utility"

    git push origin --all 2>/dev/null
)
rm -rf "$WORK_INIT"

# Add worktrees
git -C "$TEMP_DIR/.bare" worktree add "$TEMP_DIR/main" main 2>/dev/null
git -C "$TEMP_DIR/.bare" worktree add "$TEMP_DIR/feature-auth" feature/auth 2>/dev/null
git -C "$TEMP_DIR/.bare" worktree add "$TEMP_DIR/fix-logging" fix/logging 2>/dev/null

# Add some dirty files to make git status interesting
echo '// TODO: add error handling' >> "$TEMP_DIR/feature-auth/src/auth.rs"
cat > "$TEMP_DIR/feature-auth/src/middleware.rs" << 'RUST'
pub fn auth_middleware() {
    // New untracked file
}
RUST

echo '// Updated log format' >> "$TEMP_DIR/fix-logging/src/logger.rs"

echo "Temp repo created at: $TEMP_DIR"

# ── Kill any existing capture session ────────────────────────────────────

tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true

# ══════════════════════════════════════════════════════════════════════════
# 1. WELCOME SCREEN — launch without a bare repo
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing welcome screen ═══"

EMPTY_DIR=$(mktemp -d)
tmux new-session -d -s "$TMUX_SESSION" -x "$COLS" -y "$ROWS" "$BINARY $EMPTY_DIR"
wait_settle 4
capture "welcome.png" "ClawTree — Welcome"
# Quit clawtree
send_keys C-q
wait_settle 2
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
rm -rf "$EMPTY_DIR"

# ══════════════════════════════════════════════════════════════════════════
# 2. MAIN INTERFACE — launch with the bare repo, start a Claude session
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing main interface ═══"

tmux new-session -d -s "$TMUX_SESSION" -x "$COLS" -y "$ROWS" "$BINARY $TEMP_DIR"
wait_settle 5

# Spawn a Claude session on feature-auth (Shift+C = yolo/skip-permissions)
send_keys C
wait_settle 8  # wait for Claude to initialize

# Tab back to sidebar so we see the full layout
send_keys Tab
wait_settle 1

capture "main.png" "ClawTree"

# ══════════════════════════════════════════════════════════════════════════
# 3. WORKTREE MANAGEMENT — show create worktree dialog
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing worktree management ═══"

# Press 'n' to open create worktree dialog
send_keys n
wait_settle 2
capture "worktree-management.png" "ClawTree — New Worktree"

# Cancel dialog
send_keys Escape
wait_settle 1

# ══════════════════════════════════════════════════════════════════════════
# 4. GIT OPERATIONS — show git staging/commit dialog
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing git operations ═══"

# Navigate to feature-auth worktree (has dirty files)
send_keys Home
wait_settle 0.5
send_keys j  # move to feature-auth
wait_settle 0.5

# Press 's' to open stage/commit dialog
send_keys s
wait_settle 2
capture "git-operations.png" "ClawTree — Stage & Commit"

# Cancel dialog
send_keys Escape
wait_settle 1

# ══════════════════════════════════════════════════════════════════════════
# 5. CLAUDE CODE SESSIONS — spawn a session then capture
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing Claude Code sessions ═══"

# Navigate to main worktree and spawn a claude session (Shift+C = yolo mode)
send_keys Home
wait_settle 0.5
# Move to a different worktree for a second session
send_keys j
wait_settle 0.5
send_keys j  # fix/logging
wait_settle 0.5
send_keys C
wait_settle 8  # wait for Claude to initialize and start rendering

capture "claude-sessions.png" "ClawTree — Claude Code Session"

# ══════════════════════════════════════════════════════════════════════════
# 6. PROMPT QUEUE — show the prompt queue panel
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing prompt queue ═══"

# Tab back to sidebar first
send_keys Tab
wait_settle 1

# Ctrl+P to toggle prompt queue
send_keys C-p
wait_settle 2
capture "prompt-queue.png" "ClawTree — Prompt Queue"

# Toggle it off
send_keys Escape
wait_settle 1

# ══════════════════════════════════════════════════════════════════════════
# 7. HELP OVERLAY — press ?
# ══════════════════════════════════════════════════════════════════════════

echo ""
echo "═══ Capturing help overlay ═══"

send_keys '?'
wait_settle 2
capture "help-overlay.png" "ClawTree — Hotkeys"

# Close help
send_keys Escape
wait_settle 1

# ── Done ─────────────────────────────────────────────────────────────────

echo ""
echo "═══ All screenshots captured! ═══"
echo ""
ls -la "$OUTPUT_DIR"/*.png 2>/dev/null || echo "(no PNG files found)"
echo ""
echo "Output: $OUTPUT_DIR/"
