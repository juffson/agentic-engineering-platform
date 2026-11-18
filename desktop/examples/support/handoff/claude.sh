#!/bin/sh
set -eu
prompt=$(cat)
case "$prompt" in
  *'"author":"implementer"'*'Receipt: receipt-lagoon.'*) ;;
  *) echo 'Implementer handoff missing from reviewer input' >&2; exit 1 ;;
esac
cat <<'JSON'
{"structured_output":{"note":"Reviewed overflow handling at the u32 boundary.","question":"","report":"Received receipt-lagoon. The conversion widens before multiplying."}}
JSON
