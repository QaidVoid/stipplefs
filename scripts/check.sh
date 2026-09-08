#!/usr/bin/env bash
# Runs every gate and reports each command's own exit status.
#
# Piping cargo into another command hides its exit code, so each gate is run
# directly and its status captured before anything else touches $?.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
gate() {
    local label="$1"; shift
    local out
    out=$("$@" 2>&1)
    local status=$?
    if [ $status -eq 0 ]; then
        printf '  ok    %s\n' "$label"
    else
        printf '  FAIL  %s (exit %d)\n' "$label" "$status"
        printf '%s\n' "$out" | tail -25 | sed 's/^/          /'
        fail=1
    fi
}

gate "fmt"                cargo fmt --all --check
gate "clippy all"         cargo clippy --all-features --all-targets --quiet -- -D warnings
gate "clippy read"        cargo clippy --no-default-features --features read --all-targets --quiet -- -D warnings
gate "clippy write"       cargo clippy --no-default-features --features write --all-targets --quiet -- -D warnings
gate "tests all"          cargo test --all-features --quiet
gate "tests full"         cargo test --no-default-features --features read,write,cli,mount,zstd,dict,fs,mmap --quiet
gate "tests read"         cargo test --no-default-features --features read --quiet
gate "docs"               env RUSTDOCFLAGS=-Dwarnings cargo doc --all-features --no-deps --quiet
gate "feature matrix"     ./scripts/feature-matrix.sh

[ $fail -eq 0 ] && echo "all gates passed" || echo "GATES FAILED"
exit $fail
