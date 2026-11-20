#!/bin/sh
set -eu

exec setpriv --reuid=10001 --regid=10001 --clear-groups     --bounding-set=-all --inh-caps=-all --ambient-caps=-all --no-new-privs     env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/pilot/agent     GIT_TERMINAL_PROMPT=0 HTTPS_PROXY="${HTTPS_PROXY:?}"     HTTP_PROXY="${HTTP_PROXY:?}" /usr/bin/git "$@"
