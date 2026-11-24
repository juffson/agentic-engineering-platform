#!/bin/sh
set -eu
prompt=$(cat)
question="$AEP_FIXTURE_QUESTION"
receipt=receipt-lagoon
conversion='u64::from(seconds) * 1_000'
if [ "$AEP_FIXTURE_RUN" = memory-follow-up ]; then
  case "$prompt" in
    *"$AEP_FIXTURE_QUESTION"*|*receipt-lagoon*) echo 'Old memory reached the follow-up implementer' >&2; exit 1 ;;
    *"$AEP_FIXTURE_DECISION"*) ;;
    *) echo 'Decision missing from the follow-up implementer prompt' >&2; exit 1 ;;
  esac
  question=
  receipt=receipt-harbor
  conversion='u64::from(seconds) * 1000'
fi
while [ "$1" != --output-last-message ]; do
  shift
done
printf '{"note":"Widen seconds to u64 before multiplying. Receipt: %s.","question":"%s"}\n' "$receipt" "$question" > "$2"
cat > normalize.rs <<RUST
pub fn milliseconds(seconds: u32) -> u64 {
    $conversion
}
RUST
