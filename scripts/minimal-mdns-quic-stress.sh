#!/usr/bin/env bash

set -euo pipefail

rounds=25
run_for_secs=5
pause_secs=1
port=54321
service="_mmqstress._udp.local."
log_filter="info"
connect_addr=""

usage() {
  cat <<'EOF'
Usage:
  scripts/minimal-mdns-quic-stress.sh [options]

Runs repeated minimal-mdns-quic startup/connect/shutdown cycles.

Run the same command on both machines to stress mDNS-based peer discovery:
  scripts/minimal-mdns-quic-stress.sh

Or bypass mDNS and direct-connect to a known peer:
  scripts/minimal-mdns-quic-stress.sh --connect 192.168.1.10:54321

Options:
  --rounds <n>          Number of iterations to run. Default: 25
  --run-for-secs <n>    Seconds each iteration stays alive. Default: 5
  --pause-secs <n>      Seconds to wait between iterations. Default: 1
  --port <port>         Listening port for each iteration. Default: 54321
  --service <type>      mDNS service type. Default: _mmqstress._udp.local.
  --connect <addr>      Optional direct peer address host:port
  --log <filter>        Tracing filter. Default: info
  -h, --help            Show this help

Notes:
  - Use the same --service value on both machines when using mDNS mode.
  - Logs are written under tmp/minimal-mdns-quic-stress/<timestamp-hostname>/.
EOF
}

require_value() {
  local flag="$1"
  local value="${2:-}"
  if [[ -z "$value" ]]; then
    echo "$flag requires a value" >&2
    exit 1
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rounds)
      require_value "$1" "${2:-}"
      rounds="$2"
      shift 2
      ;;
    --run-for-secs)
      require_value "$1" "${2:-}"
      run_for_secs="$2"
      shift 2
      ;;
    --pause-secs)
      require_value "$1" "${2:-}"
      pause_secs="$2"
      shift 2
      ;;
    --port)
      require_value "$1" "${2:-}"
      port="$2"
      shift 2
      ;;
    --service)
      require_value "$1" "${2:-}"
      service="$2"
      shift 2
      ;;
    --connect)
      require_value "$1" "${2:-}"
      connect_addr="$2"
      shift 2
      ;;
    --log)
      require_value "$1" "${2:-}"
      log_filter="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      echo >&2
      usage >&2
      exit 1
      ;;
  esac
done

timestamp="$(date +%Y%m%d-%H%M%S)"
hostname_short="$(hostname -s 2>/dev/null || hostname)"
safe_hostname="$(printf '%s' "$hostname_short" | tr -cs '[:alnum:]._- ' '-' | tr ' ' '-')"
log_dir="tmp/minimal-mdns-quic-stress/${timestamp}-${safe_hostname}"
mkdir -p "$log_dir"

echo "minimal-mdns-quic stress test"
echo "  host: ${hostname_short}"
echo "  rounds: ${rounds}"
echo "  run_for_secs: ${run_for_secs}"
echo "  pause_secs: ${pause_secs}"
echo "  port: ${port}"
echo "  service: ${service}"
if [[ -n "$connect_addr" ]]; then
  echo "  connect: ${connect_addr}"
else
  echo "  connect: mDNS"
fi
echo "  logs: ${log_dir}"
echo

established_count=0
error_count=0

unexpected_error_pattern='ERROR|error =|failed to|connect failed|handshake failed|No buffer space available'
known_shutdown_noise='unregister: cannot find such service|unregister: failed to send response: sending on a closed channel|exit: failed to send response of shutdown: sending on a closed channel'

for round in $(seq 1 "$rounds"); do
  name="${hostname_short}-stress-${round}"
  log_file="${log_dir}/round-$(printf '%03d' "$round").log"
  cmd=(
    cargo run --quiet --bin minimal-mdns-quic --
    --name "$name"
    --port "$port"
    --run-for-secs "$run_for_secs"
    --log "$log_filter"
    --service "$service"
  )

  if [[ -n "$connect_addr" ]]; then
    cmd+=(--connect "$connect_addr")
  fi

  echo "[$(printf '%03d' "$round")/${rounds}] starting ${name}"
  if "${cmd[@]}" >"$log_file" 2>&1; then
    :
  else
    echo "[$(printf '%03d' "$round")] command failed, see ${log_file}" >&2
    error_count=$((error_count + 1))
  fi

  if rg -q "session established" "$log_file"; then
    established_count=$((established_count + 1))
  fi

  unexpected_errors="$(rg -n "$unexpected_error_pattern" "$log_file" | rg -v "$known_shutdown_noise" || true)"
  if [[ -n "$unexpected_errors" ]]; then
    error_count=$((error_count + 1))
  fi

  if (( round < rounds )); then
    sleep "$pause_secs"
  fi
done

echo
echo "summary"
echo "  rounds: ${rounds}"
echo "  rounds_with_session: ${established_count}"
echo "  rounds_with_errors: ${error_count}"
echo "  logs: ${log_dir}"

if (( error_count > 0 )); then
  echo
  echo "error scan"
  rg -n "$unexpected_error_pattern" "$log_dir" | rg -v "$known_shutdown_noise" || true
  exit 1
fi
