#!/bin/sh
set -eu

AEP_CONTROL_OPERATOR_TOKEN=$(cat /run/aep-control/operator)
AEP_CONTROL_DEVICE_TOKEN=$(cat /run/aep-control/device)
export AEP_CONTROL_OPERATOR_TOKEN AEP_CONTROL_DEVICE_TOKEN
if [ -r /run/aep-control/workers.json ]; then
    set -- "$@" --workers /run/aep-control/workers.json
fi
exec /usr/local/bin/aep control serve --state /var/lib/aep/control.json "$@"
