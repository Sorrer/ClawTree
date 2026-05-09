#!/usr/bin/env bash
#
# End-to-end tests for ClawTree.
# Launches the built binary inside tmux, sends keystrokes, and verifies
# terminal output via `tmux capture-pane -p`.
#
# Usage: tests/e2e_test.sh
#
# Requirements: tmux, built clawtree binary (target/release/clawtree or target/debug/clawtree)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TMUX_SESSION="clawtree-e2e-test"
COLS=120
ROWS=35
WAIT=3

# Find binary — prefer release, fall back to debug
BINARY="${CLAWTREE_BINARY:-}"
if [[ -z "$BINARY" ]]; then
    if [[ -x "$PROJECT_DIR/target/release/clawtree" ]]; then
        BINARY="$PROJECT_DIR/target/release/clawtree"
    elif [[ -x "$PROJECT_DIR/target/debug/clawtree" ]]; then
        BINARY="$PROJECT_DIR/target/debug/clawtree"
    else
        echo "ERROR: No clawtree binary found. Run 'cargo build' or 'cargo build --release' first." >&2
        exit 1
    fi
fi

PASSED=0
FAILED=0
ERRORS=""

# ── Helpers ──────────────────────────────────────────────────────────────

cleanup() {
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
    if [[ -n "${TEMP_DIR:-}" && -d "${TEMP_DIR:-}" ]]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

send_keys() {
    tmux send-keys -t "$TMUX_SESSION" "$@"
}

wait_settle() {
    sleep "${1:-$WAIT}"
}

capture_pane() {
    tmux capture-pane -t "$TMUX_SESSION" -p 2>/dev/null || true
}

assert_contains() {
    local test_name="$1"
    local pattern="$2"
    local content
    content="$(capture_pane)"

    if echo "$content" | grep -qiF "$pattern"; then
        echo "  PASS: $test_name"
        PASSED=$((PASSED + 1))
    else
        echo "  FAIL: $test_name (expected to find '$pattern')"
        echo "  ---- captured pane ----"
        echo "$content" | head -20
        echo "  ---- end ----"
        FAILED=$((FAILED + 1))
        ERRORS="${ERRORS}\n  - $test_name: pattern '$pattern' not found"
    fi
}

assert_not_contains() {
    local test_name="$1"
    local pattern="$2"
    local content
    content="$(capture_pane)"

    if echo "$content" | grep -qiF "$pattern"; then
        echo "  FAIL: $test_name (unexpectedly found '$pattern')"
        FAILED=$((FAILED + 1))
        ERRORS="${ERRORS}\n  - $test_name: pattern '$pattern' unexpectedly found"
    else
        echo "  PASS: $test_name"
        PASSED=$((PASSED + 1))
    fi
}

start_clawtree() {
    local dir="$1"
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
    tmux new-session -d -s "$TMUX_SESSION" -x "$COLS" -y "$ROWS" "$BINARY $dir"
    wait_settle 4
}

# ── Preflight ────────────────────────────────────────────────────────────

command -v tmux >/dev/null || { echo "ERROR: tmux is required" >&2; exit 1; }
[[ -x "$BINARY" ]] || { echo "ERROR: Binary not found: $BINARY" >&2; exit 1; }

echo "ClawTree E2E Tests"
echo "Binary: $BINARY"
echo "═══════════════════════════════════════════════════════"

# ── Test 1: Welcome screen when no repo detected ────────────────────────

echo ""
echo "Test 1: Welcome screen appears when no repo detected"
TEMP_DIR=$(mktemp -d)
EMPTY_DIR=$(mktemp -d)
start_clawtree "$EMPTY_DIR"
assert_contains "Welcome screen shows" "Initialize Bare Repo"
send_keys C-q
wait_settle 2
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
rm -rf "$EMPTY_DIR"

# ── Set up temp bare repo for remaining tests ────────────────────────────

echo ""
echo "Setting up test repo..."
REPO_DIR="$TEMP_DIR/repo"
mkdir -p "$REPO_DIR"

# Init bare repo
git init --bare "$REPO_DIR/.bare" >/dev/null 2>&1
echo "gitdir: ./.bare" > "$REPO_DIR/.git"
git -C "$REPO_DIR/.bare" symbolic-ref HEAD refs/heads/main
git -C "$REPO_DIR/.bare" config user.name "Test"
git -C "$REPO_DIR/.bare" config user.email "test@test.com"

# Initial commit — must use GIT_DIR + GIT_WORK_TREE because .bare has core.bare=true
GIT_DIR="$REPO_DIR/.bare" GIT_WORK_TREE="$REPO_DIR" \
    git -c user.name=Test -c user.email=test@test.com \
    commit --allow-empty -m "Initial commit" >/dev/null 2>&1

git -C "$REPO_DIR" worktree add "$REPO_DIR/main" main >/dev/null 2>&1
git -C "$REPO_DIR/main" config user.name "Test"
git -C "$REPO_DIR/main" config user.email "test@test.com"

# Add a file so it's not empty
echo "hello" > "$REPO_DIR/main/README.md"
git -C "$REPO_DIR/main" add -A >/dev/null 2>&1
git -C "$REPO_DIR/main" commit -m "Add README" >/dev/null 2>&1

# Create a second worktree
git -C "$REPO_DIR" worktree add -b feature "$REPO_DIR/feature" main >/dev/null 2>&1
git -C "$REPO_DIR/feature" config user.name "Test"
git -C "$REPO_DIR/feature" config user.email "test@test.com"

# Make feature dirty
echo "dirty" > "$REPO_DIR/feature/dirty.txt"

echo "Test repo ready at: $REPO_DIR"

# ── Test 2: Regular repo detection offers convert ────────────────────────

echo ""
echo "Test 2: Regular repo detection offers convert option"
REGULAR_DIR=$(mktemp -d)
git init "$REGULAR_DIR" >/dev/null 2>&1
git -C "$REGULAR_DIR" config user.name "Test"
git -C "$REGULAR_DIR" config user.email "test@test.com"
git -C "$REGULAR_DIR" checkout -b main >/dev/null 2>&1 || true
git -C "$REGULAR_DIR" commit --allow-empty -m "init" >/dev/null 2>&1

start_clawtree "$REGULAR_DIR"
assert_contains "Regular repo convert option" "convert"
send_keys C-q
wait_settle 2
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
rm -rf "$REGULAR_DIR"

# ── Test 3: Main interface shows worktrees ───────────────────────────────

echo ""
echo "Test 3: Main interface shows worktrees in sidebar"
start_clawtree "$REPO_DIR"
assert_contains "Shows main worktree" "main"
assert_contains "Shows feature worktree" "feature"

# ── Test 4: Navigation with j/k ─────────────────────────────────────────

echo ""
echo "Test 4: Navigation with j/k keys (no crash)"
send_keys j
wait_settle 1
send_keys k
wait_settle 1
# If we got here without tmux session dying, navigation works
assert_contains "Navigation survived" "main"

# ── Test 5: Create worktree dialog ───────────────────────────────────────

echo ""
echo "Test 5: Create worktree dialog opens with 'n'"
send_keys n
wait_settle 2
assert_contains "Create dialog visible" "branch"
send_keys Escape
wait_settle 1

# ── Test 6: Stage/commit dialog ─────────────────────────────────────────

echo ""
echo "Test 6: Stage/commit dialog opens with 's' on dirty worktree"
# Navigate to feature worktree (below Project item, has dirty files)
send_keys Home
wait_settle 0.5
send_keys j
wait_settle 0.5
send_keys s
wait_settle 2
assert_contains "Stage dialog visible" "dirty.txt"
send_keys Escape
wait_settle 1

# ── Test 7: Help overlay ────────────────────────────────────────────────

echo ""
echo "Test 7: Help overlay opens with '?'"
send_keys '?'
wait_settle 2
assert_contains "Help overlay visible" "help"
send_keys Escape
wait_settle 1

# ── Test 8: Ctrl+Q quits cleanly ────────────────────────────────────────

echo ""
echo "Test 8: Ctrl+Q quits cleanly"
send_keys C-q
wait_settle 3
# Check if tmux session is gone (process exited)
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    echo "  FAIL: Ctrl+Q quit (tmux session still alive)"
    FAILED=$((FAILED + 1))
    ERRORS="${ERRORS}\n  - Ctrl+Q quit: tmux session still alive"
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
else
    echo "  PASS: Ctrl+Q quit"
    PASSED=$((PASSED + 1))
fi

# ── Summary ─────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════"
echo "Results: $PASSED passed, $FAILED failed"

if [[ $FAILED -gt 0 ]]; then
    echo ""
    echo "Failures:"
    echo -e "$ERRORS"
    exit 1
fi

echo "All E2E tests passed!"
