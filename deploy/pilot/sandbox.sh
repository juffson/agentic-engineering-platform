#!/bin/sh
set -eu

test "$1" = /usr/local/bin/aep
shift
umask 077
exec setpriv --reuid=10001 --regid=10001 --clear-groups     --bounding-set=-all --inh-caps=-all --ambient-caps=-all --no-new-privs     env -i PATH=/usr/local/libexec/aep-executor:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin     HOME=/pilot/agent CODEX_HOME=/pilot/agent/.codex     CLAUDE_CONFIG_DIR=/pilot/agent/.claude     TMPDIR=/pilot/tmp RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/pilot/cargo CARGO_TARGET_DIR=/pilot/build     CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 GIT_TERMINAL_PROMPT=0     DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 DISABLE_NONESSENTIAL_MODEL_CALLS=1     HTTPS_PROXY="${HTTPS_PROXY:?}" HTTP_PROXY="${HTTP_PROXY:?}"     /usr/local/bin/aep "$@"
