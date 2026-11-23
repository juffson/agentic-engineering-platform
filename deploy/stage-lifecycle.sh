#!/bin/sh
set -eu
usage='Usage: deploy/stage-lifecycle.sh PLAN.json OUTPUT_DIR [--rehearse]'
if [ "${1-}" = "--help" ] || [ "${1-}" = "-h" ]; then
    echo "$usage"
    echo 'Waits for decisions and handoffs in Studio. --rehearse exercises those controls with the plan.'
    exit 0
fi
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ] || { [ "$#" -eq 3 ] && [ "$3" != "--rehearse" ]; }; then
    echo "$usage" >&2
    exit 2
fi
if [ ! -r "$1" ]; then
    echo "Cannot read plan: $1" >&2
    exit 2
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --locked --manifest-path "$root/Cargo.toml" --bin aep --target-dir "$root/target"
cargo build --locked --manifest-path "$root/desktop/Cargo.toml" --example ui_debug --target-dir "$root/desktop/target"
export AEP_STUDIO_RUNNER="$root/target/debug/aep"
plan=$1
output=$2
shift 2
exec "$root/desktop/target/debug/examples/ui_debug" --live-stage-lifecycle "$plan" --output-dir "$output" "$@"
