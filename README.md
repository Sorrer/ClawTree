<div align="center">

<img src="assets/logo.svg" alt="Clawtree" width="580">

<br>

**A terminal UI for managing git worktrees with integrated Claude Code sessions.**

[![Release](https://img.shields.io/github/v/release/Sorrer/ClawTree?style=flat-square&label=Release&color=blue)](https://github.com/Sorrer/ClawTree/releases/latest)
[![Nightly](https://img.shields.io/github/actions/workflow/status/Sorrer/ClawTree/release.yml?style=flat-square&label=Nightly)](https://github.com/Sorrer/ClawTree/actions/workflows/release.yml)
[![Rust](https://img.shields.io/badge/Rust-2021_Edition-f74c00?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platforms](https://img.shields.io/badge/Platforms-Linux_|_macOS_|_WSL-lightgrey?style=flat-square)](#platform)

</div>

---

Built for the **bare repository** workflow where a project uses a `.bare/` directory with multiple worktree directories for different branches.

## Table of Contents

- [Install](#install)
- [Features](#features)
- [Requirements](#requirements)
- [Building from Source](#building-from-source)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
```

This downloads a pre-built binary for your platform and adds it to your PATH. Supports Linux (x86_64, aarch64) and macOS (Intel, Apple Silicon).

<details>
<summary><strong>Advanced install options</strong></summary>

Install from a **private repo** (requires [GitHub CLI](https://cli.github.com/) authenticated via `gh auth login`):

```bash
gh api repos/Sorrer/ClawTree/contents/install.sh \
  -H "Accept: application/vnd.github.raw" | GITHUB_TOKEN=$(gh auth token) bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash -s -- v0.1.0
```

Install to a custom directory:

```bash
CLAWTREE_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
```

</details>

### Usage

```bash
clawtree /path/to/bare/repo
```

The target directory should contain a `.bare` git repository structure.

## Features

### Worktree Management
- Browse, create, and delete git worktrees from a sidebar tree view
- Initialize new bare repos or clone from a remote URL
- Convert existing regular git repos to the bare worktree layout (in-place or to a new location)
- Background git status polling with per-worktree caching (auto-refreshes every 10s)

### Git Operations
- Interactive file staging/unstaging (single file, stage all)
- Inline commit message input
- Branch merging with automatic dirty-worktree detection and pre-merge commit prompts
- Merge conflict resolution dialog — open in VS Code, JetBrains, Claude, or abort
- Push to remote with automatic upstream setup (`-u origin`)

### Claude Code Sessions
- Spawn multiple concurrent Claude Code sessions per worktree
- tmux-backed sessions persist across TUI restarts with automatic reconnection
- Truecolor (24-bit RGB) passthrough configured automatically
- Full VT100 terminal emulation with 1000-line scrollback
- Session renaming/nicknames
- Optional `--dangerously-skip-permissions` mode

### Prompt Queue
- Per-session prompt queues that auto-send when Claude is idle (5s cooldown)
- Add, edit, and delete queued prompts
- Queues persist to `.prompt_queues.json` and survive restarts

### Mini Mode
- Compact agent management view (toggle with F2)
- Drilldown into a single agent's terminal full-screen
- Agent status badges: Working, Idle, Needs Input, Exited
- Saved prompt templates (`.agent_prompts.json`) for quick agent creation
- Auto-captured agent summaries on Working-to-Idle transitions

### Terminal & UI
- Three screen modes: Normal, Mini, Mini Drilldown
- Keyboard-driven with vim-style navigation (j/k, Home/End, etc.)
- Mouse scroll support (3 lines per tick) with click-to-select text
- Scrollback browsing with scrollbar and `[+N]` offset indicator
- Context-sensitive help overlay with 6 tabs (press `?`)
- Status bar with auto-fading messages and mode badges
- Background Claude context usage tracking via debug log parsing

### Platform
- Linux (x86_64, aarch64) and macOS (Intel, Apple Silicon)
- WSL integration: open Windows Terminal tabs with `w`/`W` keys
- Graceful signal handling (SIGINT, SIGTERM, SIGHUP) with guaranteed terminal restoration

## Requirements

| Dependency | Required | Purpose |
|---|---|---|
| **git** | Yes | Worktree management, staging, commits, merges |
| **claude** (Claude Code CLI) | Yes | AI assistant sessions spawned per worktree |
| **tmux** | Yes | Session persistence and reconnection across restarts |
| **wt.exe** | Optional | Windows Terminal tab integration (WSL only) |

## Building from Source

Everything from the requirements above, plus Rust (edition 2021, 1.56+) and a C compiler.

```bash
./build.sh
# or manually:
cargo build --release
```

```bash
./run.sh [optional-directory]
# or manually:
./target/release/clawtree [optional-directory]
```
