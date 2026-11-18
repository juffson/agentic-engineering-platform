#!/bin/sh
set -eu
cat >/dev/null
while [ "$1" != --output-last-message ]; do
  shift
done
cat > "$2" <<'JSON'
{"note":"Widen seconds to u64 before multiplying. Receipt: receipt-lagoon.","question":""}
JSON
cat > normalize.rs <<'RUST'
pub fn milliseconds(seconds: u32) -> u64 {
    u64::from(seconds) * 1_000
}
RUST
