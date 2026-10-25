#!/bin/bash

set -euo pipefail

: "${MOUSE_THING_LOG_PRESET:=debug}"
export MOUSE_THING_LOG_PRESET

cargo run --bin discovery-smoke -- "$@"
