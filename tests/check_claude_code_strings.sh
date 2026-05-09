#!/usr/bin/env bash
# Verify that Claude Code's CLI source still contains the emoji/text patterns
# clawtree relies on for status detection. If any pattern is missing, Claude Code
# may have changed its UI and clawtree needs updating.
#
# Runs against the published @anthropic-ai/claude-code npm package.
# Supports both the legacy JS bundle (cli.js) and the newer native binary layout.
# In native binaries, Unicode characters may appear as \uXXXX escape sequences
# rather than literal UTF-8, so we check for both forms.
set -euo pipefail

PASS=0
FAIL=0
WARNINGS=""

# Check for a literal string OR its \uXXXX escape form in a (possibly binary) file.
# Usage: check_string "description" "literal" "file" ["escape"]
#   escape: optional \uXXXX pattern to also search for (for native binaries)
check_string() {
    local description="$1"
    local literal="$2"
    local file="$3"
    local escape="${4:-}"

    if grep -qaF "$literal" "$file"; then
        echo "  PASS: $description"
        PASS=$((PASS + 1))
    elif [ -n "$escape" ] && grep -qaiF "$escape" "$file"; then
        echo "  PASS: $description (via escape sequence)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $description"
        FAIL=$((FAIL + 1))
        WARNINGS="${WARNINGS}\n  - ${description}"
    fi
}

check_regex() {
    local description="$1"
    local pattern="$2"
    local file="$3"

    if grep -qaE "$pattern" "$file"; then
        echo "  PASS: $description"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $description  (pattern: $pattern)"
        FAIL=$((FAIL + 1))
        WARNINGS="${WARNINGS}\n  - ${description}"
    fi
}

echo "=== Claude Code String Verification ==="
echo ""

# Create a temp directory and download the package
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

echo "Downloading @anthropic-ai/claude-code from npm..."
cd "$WORK_DIR"
npm pack @anthropic-ai/claude-code --pack-destination . 2>/dev/null
tar xzf anthropic-ai-claude-code-*.tgz 2>/dev/null

# Try legacy JS bundle first, then fall back to native binary
CLI_SOURCE="$(find package -name 'cli.js' -type f 2>/dev/null | head -1)"
if [ -n "$CLI_SOURCE" ]; then
    echo "Found CLI source (JS bundle): $CLI_SOURCE"
else
    # New native binary layout — download the platform-specific package
    echo "No cli.js found; downloading linux-x64 native binary..."
    npm pack @anthropic-ai/claude-code-linux-x64 --pack-destination . 2>/dev/null
    tar xzf anthropic-ai-claude-code-linux-x64-*.tgz 2>/dev/null

    CLI_SOURCE="$(find package -name 'claude' -type f ! -name '*.tgz' 2>/dev/null | head -1)"
    if [ -z "$CLI_SOURCE" ]; then
        echo "ERROR: Could not find claude binary in the native package"
        exit 1
    fi
    echo "Found CLI source (native binary): $CLI_SOURCE"
fi
echo ""

# ─── Terminal title patterns ─────────────────────────────────────────
echo "Terminal title indicators:"

# Idle title icon: ✳ (U+2733 Eight Spoked Asterisk)
check_string \
    "Idle title icon ✳ (U+2733)" \
    "✳" \
    "$CLI_SOURCE" \
    '\u2733'

# Working title spinner: ⠂ (U+2802) and ⠐ (U+2810) braille dots
check_string \
    "Working title spinner ⠂ (U+2802)" \
    "⠂" \
    "$CLI_SOURCE" \
    '\u2802'

check_string \
    "Working title spinner ⠐ (U+2810)" \
    "⠐" \
    "$CLI_SOURCE" \
    '\u2810'

echo ""

# ─── Mode indicators ─────────────────────────────────────────────────
echo "Mode indicators:"

# Plan mode icon: ⏸ (U+23F8 Double Vertical Bar)
check_string \
    "Plan mode icon ⏸ (U+23F8)" \
    "⏸" \
    "$CLI_SOURCE" \
    '\u23f8'

# Plan mode label
check_regex \
    "Plan mode label 'Plan Mode'" \
    '[Pp]lan.?[Mm]ode' \
    "$CLI_SOURCE"

echo ""

# ─── Prompt indicators ───────────────────────────────────────────────
echo "Prompt indicators:"

# Input prompt character: ❯ (U+276F)
check_string \
    "Input prompt ❯ (U+276F)" \
    "❯" \
    "$CLI_SOURCE" \
    '\u276f'

echo ""

# ─── Summary ─────────────────────────────────────────────────────────
echo "========================================="
echo "Results: $PASS passed, $FAIL failed"

if [ "$FAIL" -gt 0 ]; then
    echo ""
    echo "WARNING: The following patterns were NOT found in Claude Code's source."
    echo "Claude Code may have changed its UI. clawtree status detection may need updating."
    echo -e "$WARNINGS"
    echo ""
    exit 1
fi

echo "All patterns verified successfully."
