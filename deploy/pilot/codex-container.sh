#!/usr/bin/env bash
set -euo pipefail

# Docker supplies the filesystem, network, and process boundary for this deployment.
[[ -f /.dockerenv && $(id -u) == 10001 ]]
grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status
for capability in CapBnd CapEff CapPrm; do
    grep -Eq "^${capability}:[[:space:]]+0+$" /proc/self/status
done
findmnt -n -o OPTIONS / | grep -Eq '(^|,)ro(,|$)'
test -f /etc/aep-pilot-container

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
