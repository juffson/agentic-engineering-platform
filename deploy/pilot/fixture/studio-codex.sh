#!/bin/sh
set -eu
prompt=$(cat)
factor=1_000
question='Which rounding rule should a future fractional API use?'
case "$prompt" in
  *'Return the exact widened value.'*)
    grep -q 'u64::from(seconds) \* 1_000' normalize.rs
    factor=1000
    question=
    ;;
esac
if [ -f "$(dirname "$0")/fail-check" ]; then
  factor=1
fi
while [ "$1" != --output-last-message ]; do shift; done
printf '{"note":"Codex converted seconds.","question":"%s"}\n' "$question" > "$2"
printf 'pub fn milliseconds(seconds: u32) -> u64 { u64::from(seconds) * %s }\n' "$factor" > normalize.rs
