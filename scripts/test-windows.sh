#!/usr/bin/env bash
set -euo pipefail

VM_NAME="${WINDOWS_VM_NAME:-Windows 11}"
SSH_HOST="${WINDOWS_SSH_HOST:-}"
TARGET="${WINDOWS_CHECK_TARGET:-x86_64-pc-windows-gnu}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

run_in_vm() {
    local ssh_host="$1"
    echo "Syncing repo to ${ssh_host}..."
    ssh "${ssh_host}" "powershell -NoProfile -Command \"if (-not (Test-Path C:/mouse-thing)) { New-Item -ItemType Directory C:/mouse-thing | Out-Null }\""
    rsync -az --delete --exclude target --exclude .git "${REPO_ROOT}/" "${ssh_host}:C:/mouse-thing/"
    echo "Running tests inside Windows VM..."
    ssh "${ssh_host}" "cd C:/mouse-thing && cargo test --test trace_replay_tests -q && cargo test --test runtime_integration_tests -q && cargo test --test clipboard_integration_tests -q"
}

run_compile_check() {
    echo "No Windows VM reachable. Running Windows compile check (${TARGET})..."
    if cargo check --quiet --target "${TARGET}"; then
        echo "Windows compile check passed."
    else
        echo "Windows compile check failed." >&2
        return 1
    fi
    echo "For full test execution: start the '${VM_NAME}' UTM VM with SSH (set WINDOWS_SSH_HOST), or push to run the windows-tests CI job."
}

if [ -n "${SSH_HOST}" ] && ssh -o ConnectTimeout=5 -o BatchMode=yes "${SSH_HOST}" "echo ok" >/dev/null 2>&1; then
    run_in_vm "${SSH_HOST}"
    echo "Windows tests passed."
    exit 0
fi

if command -v utmctl >/dev/null 2>&1; then
    STATUS="$(utmctl status "${VM_NAME}" 2>/dev/null || echo "not_found")"
    if [ "${STATUS}" = "not_found" ]; then
        echo "Virtual machine '${VM_NAME}' not found in UTM. Set WINDOWS_VM_NAME or WINDOWS_SSH_HOST." >&2
        run_compile_check
        exit 0
    fi
    if [ "${STATUS}" != "started" ]; then
        echo "Starting ${VM_NAME}..."
        utmctl start "${VM_NAME}"
    fi
    if [ -n "${SSH_HOST}" ]; then
        echo "Waiting for ${SSH_HOST} to accept SSH..."
        for _ in $(seq 1 60); do
            if ssh -o ConnectTimeout=5 -o BatchMode=yes "${SSH_HOST}" "echo ok" >/dev/null 2>&1; then
                break
            fi
            sleep 5
        done
        run_in_vm "${SSH_HOST}"
        echo "Windows tests passed."
        exit 0
    fi
    echo "Windows VM ${VM_NAME} is active but WINDOWS_SSH_HOST is not set."
    echo "Set WINDOWS_SSH_HOST=user@host to run tests inside the VM via SSH."
    run_compile_check
    exit 0
fi

echo "UTM CLI (utmctl) not installed. Install with: brew install --cask utm"
run_compile_check
