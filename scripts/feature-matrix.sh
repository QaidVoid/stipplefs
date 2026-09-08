#!/usr/bin/env bash
# Builds every combination of the crate's features and fails on any warning.
#
# A single-direction build that emits dead-code warnings is the earliest signal
# that an item is gated wrongly, so warnings are errors here.
set -uo pipefail

cd "$(dirname "$0")/.."

# Derived from Cargo.toml so the matrix cannot drift behind the manifest.
mapfile -t FEATURES < <(
    sed -n '/^\[features\]/,/^\[/p' Cargo.toml |
        grep -E '^[a-z0-9_-]+ *=' | cut -d= -f1 | tr -d ' ' | grep -v '^default$'
)
export RUSTFLAGS="-D warnings"

fail=0
run() {
    local label="$1"; shift
    if out=$(cargo build --quiet --no-default-features "$@" 2>&1); then
        printf '  ok    %s\n' "$label"
    else
        printf '  FAIL  %s\n' "$label"
        printf '%s\n' "$out" | sed 's/^/          /'
        fail=1
    fi
}

echo "feature matrix (${#FEATURES[@]} features, $(( 1 << ${#FEATURES[@]} )) combinations)"
for ((mask = 0; mask < (1 << ${#FEATURES[@]}); mask++)); do
    set=()
    for ((i = 0; i < ${#FEATURES[@]}; i++)); do
        (( mask & (1 << i) )) && set+=("${FEATURES[i]}")
    done
    if [ ${#set[@]} -eq 0 ]; then
        run "(no features)"
    else
        joined=$(IFS=,; echo "${set[*]}")
        run "$joined" --features "$joined"
    fi
done

if out=$(cargo build --quiet --all-features 2>&1); then
    printf '  ok    (all features)\n'
else
    printf '  FAIL  (all features)\n'
    printf '%s\n' "$out" | sed 's/^/          /'
    fail=1
fi

[ $fail -eq 0 ] && echo "all combinations built clean" || echo "matrix FAILED"
exit $fail
