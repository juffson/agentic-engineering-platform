#!/bin/sh
set -eu
prompt=$(cat)
case "$prompt" in
  *'Codex converted seconds.'*) ;;
  *) exit 1 ;;
esac
case "$prompt" in
  *' * 1000'*)
    case "$prompt" in *'Return the exact widened value.'*) ;; *) exit 2 ;; esac
    ;;
esac
printf '%s\n' '{"structured_output":{"note":"Claude checked the conversion.","question":"","report":"Reviewed and verified the conversion."}}'
