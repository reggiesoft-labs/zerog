#!/usr/bin/env bash
set -euo pipefail

XVFB_DISPLAY="127.0.0.1:99"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/mouse-thing-runtime}"
mkdir -p "${XDG_RUNTIME_DIR}"
chmod 700 "${XDG_RUNTIME_DIR}"

# The container exists to run live tests. A missing Xvfb or Weston is a harness
# failure, not a pass, so make live tests fail instead of skipping silently.
export MOUSE_THING_REQUIRE_LIVE="${MOUSE_THING_REQUIRE_LIVE:-1}"

Xvfb :99 -listen tcp -screen 0 1920x1080x24 &
XVFB_PID=$!
READY=0
for _ in $(seq 1 100); do
    if DISPLAY="${XVFB_DISPLAY}" xdpyinfo >/dev/null 2>&1; then
        READY=$((READY + 1))
        if [ "${READY}" -ge 5 ]; then
            break
        fi
    else
        READY=0
    fi
    sleep 0.2
done
if [ "${READY}" -lt 5 ]; then
    echo "Xvfb on ${XVFB_DISPLAY} never became ready; aborting instead of running vacuous tests." >&2
    kill "${XVFB_PID}" 2>/dev/null || true
    exit 1
fi
export DISPLAY="${XVFB_DISPLAY}"

if command -v weston >/dev/null 2>&1; then
    weston --backend=headless --socket=mouse-thing-test --idle-time=0 &
    for _ in $(seq 1 50); do
        if [ -S "${XDG_RUNTIME_DIR}/mouse-thing-test" ]; then
            break
        fi
        sleep 0.2
    done
    if [ -S "${XDG_RUNTIME_DIR}/mouse-thing-test" ]; then
        export MOUSE_THING_TEST_WAYLAND_SOCKET="mouse-thing-test"
        export MOUSE_THING_TEST_LIVE_WAYLAND="1"
    fi
fi

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

# The live X11/Wayland tests depend on process-global display and environment
# state. Running the suites single-threaded keeps them deterministic, so a
# green run means the live assertions actually executed and passed.
if [ $# -eq 0 ]; then
    cargo test -q --lib -- --test-threads=1 || exit 1
    cargo test -q --test trace_replay_tests -- --test-threads=1 || exit 1
    cargo test -q --test runtime_integration_tests -- --test-threads=1 || exit 1
    exec cargo test -q --test clipboard_integration_tests -- --test-threads=1
fi

exec cargo "$@"
