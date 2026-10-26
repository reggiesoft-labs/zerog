#!/bin/sh
script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

sign_binaries() {
    for bin in "$repo_root/target/debug/ZeroG" "$repo_root/target/release/ZeroG" "$repo_root/target/universal-apple-darwin/debug/ZeroG" "$repo_root/target/universal-apple-darwin/release/ZeroG"; do
        if [ -f "$bin" ]; then
            codesign --force --sign - --identifier com.reggiesoft.zerog "$bin" >/dev/null 2>&1 || true
        fi
    done
}

if [ "$1" = "run" ]; then
    shift
    build_cmd="cargo build"
    for arg in "$@"; do
        if [ "$arg" = "--" ]; then
            break
        fi
        build_cmd="$build_cmd $(printf '%q' "$arg")"
    done
    eval "$build_cmd"
    build_exit=$?
    if [ $build_exit -ne 0 ]; then
        exit $build_exit
    fi
    sign_binaries
    exec cargo run "$@"
else
    cargo "$@"
    exit_code=$?
    if [ $exit_code -eq 0 ]; then
        sign_binaries
    fi
    exit $exit_code
fi
