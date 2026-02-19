# Clawtree (Worktree Claude TUI)

A terminal-based UI for managing git worktrees with integrated Claude Code sessions. Built for the "bare repository" workflow where a project uses a `.bare/` directory with multiple worktree directories for different branches.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
```

This downloads a pre-built binary for your platform and adds it to your PATH. Supports Linux (x86_64, aarch64) and macOS (Intel, Apple Silicon).

To install a specific version:

```bash
CLAWTREE_VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
```

To install to a custom directory:

```bash
CLAWTREE_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
```

### Usage

```bash
clawtree /path/to/bare/repo
```

The target directory should contain a `.bare` git repository structure.

## Requirements

| Dependency | Required | Purpose |
|---|---|---|
| **git** | Yes | Worktree management, staging, commits, merges |
| **claude** (Claude Code CLI) | Yes | AI assistant sessions spawned per worktree |
| **tmux** | Yes | Session persistence and reconnection across restarts |
| **wt.exe** | Optional | Windows Terminal tab integration (WSL only) |

## Building from source

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
