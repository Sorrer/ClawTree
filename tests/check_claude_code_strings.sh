#!/usr/bin/env bash
# Verify that Claude Code's CLI source still contains the emoji/text patterns
# clawtree relies on for status detection. If any pattern is missing, Claude Code
# may have changed its UI and clawtree needs updating.
#
# Runs against the published @anthropic-ai/claude-code npm package.
set -euo pipefail

PASS=0
FAIL=0
WARNINGS=""

check_string() {
    local description="$1"
    local literal="$2"
    local file="$3"

    if grep -qF "$literal" "$file"; then
        echo "  PASS: $description"
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

    if grep -qE "$pattern" "$file"; then
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

CLI_JS="$(find package -name 'cli.js' -type f | head -1)"
if [ -z "$CLI_JS" ]; then
    echo "ERROR: Could not find cli.js in the npm package"
    exit 1
fi

echo "Found CLI source: $CLI_JS"
echo ""

# ─── Terminal title patterns ─────────────────────────────────────────
echo "Terminal title indicators:"

# Idle title icon: ✳ (U+2733 Eight Spoked Asterisk)
check_string \
    "Idle title icon ✳ (U+2733)" \
    "✳" \
    "$CLI_JS"

# Working title spinner: ⠂ (U+2802) and ⠐ (U+2810) braille dots
check_string \
    "Working title spinner ⠂ (U+2802)" \
    "⠂" \
    "$CLI_JS"

check_string \
    "Working title spinner ⠐ (U+2810)" \
    "⠐" \
    "$CLI_JS"

echo ""

# ─── Mode indicators ─────────────────────────────────────────────────
echo "Mode indicators:"

# Plan mode icon: ⏸ (U+23F8 Double Vertical Bar)
check_string \
    "Plan mode icon ⏸ (U+23F8)" \
    "⏸" \
    "$CLI_JS"

# Plan mode label
check_regex \
    "Plan mode label 'Plan Mode'" \
    '[Pp]lan.?[Mm]ode' \
    "$CLI_JS"

echo ""

# ─── Prompt indicators ───────────────────────────────────────────────
echo "Prompt indicators:"

# Input prompt character: ❯ (U+276F)
check_string \
    "Input prompt ❯ (U+276F)" \
    "❯" \
    "$CLI_JS"

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
