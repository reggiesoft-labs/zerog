#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME="mouse-thing-linux-test"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! docker info >/dev/null 2>&1; then
    echo "Error: Docker daemon is not running. Please launch Docker Desktop or OrbStack." >&2
    exit 1
fi

docker build -t "${IMAGE_NAME}" -f "${REPO_ROOT}/docker/Dockerfile.linux-test" "${REPO_ROOT}"

docker run --rm --init \
    -v "${REPO_ROOT}:/workspace" \
    -v "mouse-thing-linux-cargo-cache:/root/.cargo/registry" \
    -v "mouse-thing-linux-target-cache:/workspace/target" \
    -w /workspace \
    "${IMAGE_NAME}" \
    /workspace/scripts/linux-test-entrypoint.sh "$@"
