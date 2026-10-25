#!/usr/bin/env bash
set -euo pipefail

cargo run --bin mock-peer -- --bind 127.0.0.1:54322 --name perf-server --no-mdns &
SERVER_PID=$!
sleep 2

cargo run --bin mock-peer -- --bind 127.0.0.1:54323 --name perf-client --no-mdns &
CLIENT_PID=$!
sleep 10

kill "$SERVER_PID" "$CLIENT_PID" 2>/dev/null || true
echo "QUIC perf smoke test complete"
