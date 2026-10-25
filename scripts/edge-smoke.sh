#!/bin/bash

set -euo pipefail

cargo run --bin edge-smoke -- "$@"
