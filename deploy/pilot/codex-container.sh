#!/usr/bin/env bash
set -euo pipefail

# Docker supplies the filesystem, network, and process boundary for this deployment.
if ! [[ -f /.dockerenv && -f /etc/aep-pilot-container && $(id -u) == 10001 ]] ||
    ! grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status ||
    ! findmnt -n -o OPTIONS / | grep -Eq '(^|,)ro(,|$)'; then
    echo 'Codex requires the isolated AEP pilot container' >&2
    exit 1
fi
for capability in CapBnd CapEff CapPrm CapInh CapAmb; do
    if ! grep -Eq "^${capability}:[[:space:]]+0+$" /proc/self/status; then
        printf 'The Codex sandbox must clear %s\n' "$capability" >&2
        exit 1
    fi
done

arguments=("$@")
if [[ ${arguments[0]:-} == exec ]]; then
    for ((i = 1; i + 1 < ${#arguments[@]}; i++)); do
        if [[ ${arguments[i]} == --sandbox && ${arguments[i + 1]} == workspace-write ]]; then
            arguments[i + 1]=danger-full-access
            break
        fi
    done
fi
exec /usr/local/bin/codex "${arguments[@]}"
