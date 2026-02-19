#!/usr/bin/env bash
set -euo pipefail

# ClawTree installer
# Usage: curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash
# Pin a version: curl -fsSL https://raw.githubusercontent.com/Sorrer/ClawTree/main/install.sh | bash -s -- v0.1.0

REPO="Sorrer/ClawTree"
BINARY_NAME="clawtree"
INSTALL_DIR="${CLAWTREE_INSTALL_DIR:-$HOME/.local/bin}"

info()  { printf '\033[1;34m[info]\033[0m  %s\n' "$*"; }
warn()  { printf '\033[1;33m[warn]\033[0m  %s\n' "$*"; }
error() { printf '\033[1;31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

detect_platform() {
    local os arch

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux-musl" ;;
        Darwin) os="apple-darwin" ;;
        *)      error "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              error "Unsupported architecture: $arch" ;;
    esac

    echo "${arch}-${os}"
}

get_release_tag() {
    local tag="${1:-latest}"

    if [ "$tag" = "latest" ]; then
        tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
        if [ -z "$tag" ]; then
            error "No stable release found. Specify a version: curl ... | bash -s -- v0.1.0"
        fi
    fi

    echo "$tag"
}

download_and_install() {
    local platform="$1"
    local tag="$2"
    local archive="${BINARY_NAME}-${platform}.tar.gz"
    local url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading ${BINARY_NAME} ${tag} for ${platform}..."
    if ! curl -fSL --progress-bar -o "${tmpdir}/${archive}" "$url"; then
        error "Download failed. Check that a release exists for tag '${tag}' and platform '${platform}'.
  URL: ${url}"
    fi

    info "Extracting..."
    tar xzf "${tmpdir}/${archive}" -C "$tmpdir"

    mkdir -p "$INSTALL_DIR"
    mv "${tmpdir}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    info "Installed to ${INSTALL_DIR}/${BINARY_NAME}"
}

ensure_path() {
    # Check if INSTALL_DIR is already in PATH
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) return ;;
    esac

    local line="export PATH=\"${INSTALL_DIR}:\$PATH\""
    local patched=false

    # Bash
    for rc in "$HOME/.bashrc" "$HOME/.bash_profile"; do
        if [ -f "$rc" ]; then
            if ! grep -qF "$INSTALL_DIR" "$rc" 2>/dev/null; then
                printf '\n# Added by ClawTree installer\n%s\n' "$line" >> "$rc"
                info "Added ${INSTALL_DIR} to PATH in ${rc}"
                patched=true
            fi
            break
        fi
    done

    # Zsh
    if [ -f "$HOME/.zshrc" ]; then
        if ! grep -qF "$INSTALL_DIR" "$HOME/.zshrc" 2>/dev/null; then
            printf '\n# Added by ClawTree installer\n%s\n' "$line" >> "$HOME/.zshrc"
            info "Added ${INSTALL_DIR} to PATH in ~/.zshrc"
            patched=true
        fi
    fi

    # Fish
    local fish_conf="$HOME/.config/fish/conf.d/clawtree.fish"
    if [ -d "$HOME/.config/fish" ]; then
        mkdir -p "$HOME/.config/fish/conf.d"
        if [ ! -f "$fish_conf" ]; then
            echo "fish_add_path ${INSTALL_DIR}" > "$fish_conf"
            info "Added ${INSTALL_DIR} to PATH in ${fish_conf}"
            patched=true
        fi
    fi

    # Generic fallback: .profile
    if [ "$patched" = false ]; then
        if [ -f "$HOME/.profile" ]; then
            if ! grep -qF "$INSTALL_DIR" "$HOME/.profile" 2>/dev/null; then
                printf '\n# Added by ClawTree installer\n%s\n' "$line" >> "$HOME/.profile"
                info "Added ${INSTALL_DIR} to PATH in ~/.profile"
                patched=true
            fi
        fi
    fi

    if [ "$patched" = true ]; then
        warn "Restart your shell or run: source ~/.bashrc (or equivalent) to update PATH"
    fi
}

main() {
    info "ClawTree installer"
    echo

    local platform tag
    platform="$(detect_platform)"
    tag="$(get_release_tag "$@")"

    info "Platform: ${platform}"
    info "Release:  ${tag}"
    echo

    download_and_install "$platform" "$tag"
    ensure_path

    echo
    info "Done! Run 'clawtree /path/to/bare/repo' to get started."
}

main "$@"
