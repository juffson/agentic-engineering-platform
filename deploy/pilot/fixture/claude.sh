#!/bin/sh
set -eu
prompt=$(cat)
receipt=receipt-lagoon
if [ "$AEP_FIXTURE_RUN" = memory-follow-up ]; then
  receipt=receipt-harbor
  case "$prompt" in
    *"$AEP_FIXTURE_QUESTION"*|*receipt-lagoon*) echo 'Old memory reached the follow-up reviewer' >&2; exit 1 ;;
    *"$AEP_FIXTURE_DECISION"*) ;;
    *) echo 'Decision missing from the follow-up reviewer prompt' >&2; exit 1 ;;
  esac
fi
case "$prompt" in
  *'"author":"implementer"'*"Receipt: $receipt."*) ;;
  *) echo 'Implementer handoff missing from reviewer input' >&2; exit 1 ;;
esac
printf '{"structured_output":{"note":"Reviewed overflow handling at the u32 boundary.","question":"","report":"Received %s. The conversion widens before multiplying."}}\n' "$receipt"
