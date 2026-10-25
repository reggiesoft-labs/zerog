#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

run_macos() {
    echo "=== Running macOS Tests ==="
    cargo test --test trace_replay_tests -q
    echo "macOS tests passed."
}

run_linux() {
    echo "=== Running Linux Tests (Docker + Xvfb) ==="
    "${REPO_ROOT}/scripts/test-linux.sh"
    echo "Linux tests passed."
}

run_windows() {
    echo "=== Running Windows Tests (UTM) ==="
    "${REPO_ROOT}/scripts/test-windows.sh"
}

if [[ $# -eq 0 ]]; then
    run_macos
    if docker info >/dev/null 2>&1; then
        run_linux
    else
        echo "Skipping Linux tests: Docker daemon is not running."
    fi
    if command -v utmctl >/dev/null 2>&1; then
        run_windows
    else
        echo "Skipping Windows tests: UTM CLI (utmctl) is not installed."
    fi
    exit 0
fi

for arg in "$@"; do
    case "${arg}" in
        --macos)
            run_macos
            ;;
        --linux)
            run_linux
            ;;
        --windows)
            run_windows
            ;;
        *)
            echo "Usage: $0 [--macos] [--linux] [--windows]"
            exit 1
            ;;
    esac
done
