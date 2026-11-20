#!/bin/sh
set -eu

# Cancellation locks created by the supervisor inherit the executor group from run-control.
umask 002
export HOME=/pilot/supervisor
export PATH=/usr/local/libexec/aep-pilot:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin
export GIT_TERMINAL_PROMPT=0
AEP_CONTROL_DEVICE_TOKEN=$(cat /run/aep-worker/device)
export AEP_CONTROL_DEVICE_TOKEN
exec /usr/local/bin/aep --repo /pilot/repo control     --url "${AEP_PILOT_ORIGIN:?}" --ca-cert /run/aep-worker/site-ca.pem     worker --journal-dir /pilot/supervisor/journal --sandbox-launcher /usr/local/libexec/aep-pilot/sandbox.sh "$@"
