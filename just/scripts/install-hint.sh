#!/usr/bin/env bash
# Print an OS-appropriate install command for a dev-tool dependency.
#
# Usage: install-hint.sh <tool>
#
# Emits a single suggested install command (one line) for the detected platform
# and package manager, so callers can interpolate it into their own
# "✗ <tool> not found — <hint>" messages and have it read sensibly on both
# macOS (Homebrew) and Linux (apt / dnf / pacman / apk).
set -euo pipefail

tool="${1:-}"

# Detect platform / package manager.
mgr=""
case "$(uname -s)" in
    Darwin) mgr="brew" ;;
    Linux)
        if   command -v apt-get >/dev/null 2>&1; then mgr="apt"
        elif command -v dnf     >/dev/null 2>&1; then mgr="dnf"
        elif command -v pacman  >/dev/null 2>&1; then mgr="pacman"
        elif command -v apk     >/dev/null 2>&1; then mgr="apk"
        else mgr="linux"; fi ;;
    *) mgr="other" ;;
esac

# Generic package-manager install line for a plain package name.
pkg_install() {
    case "$mgr" in
        brew)   echo "brew install $1" ;;
        apt)    echo "sudo apt install $1" ;;
        dnf)    echo "sudo dnf install $1" ;;
        pacman) echo "sudo pacman -S $1" ;;
        apk)    echo "sudo apk add $1" ;;
        *)      echo "install '$1' via your package manager" ;;
    esac
}

case "$tool" in
    sccache|cargo-sweep)
        # Rust tools: cargo install is the portable path on every OS.
        echo "cargo install $tool" ;;
    ollama)
        if [[ "$mgr" == "brew" ]]; then
            echo "brew install ollama  (or the macOS app: https://ollama.com/download)"
        else
            echo "curl -fsSL https://ollama.com/install.sh | sh"
        fi ;;
    nomad)
        if [[ "$mgr" == "brew" ]]; then
            echo "brew install hashicorp/tap/nomad"
        else
            echo "see https://developer.hashicorp.com/nomad/install"
        fi ;;
    direnv)
        if [[ "$mgr" == "other" ]]; then
            echo "see https://direnv.net/docs/installation.html"
        else
            pkg_install direnv
        fi ;;
    *)
        pkg_install "${tool:-<tool>}" ;;
esac
