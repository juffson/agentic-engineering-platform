#!/bin/sh
set -eu
umask 077

checkout=$1 repository=$2 source=$3 destination=$4
[ "$checkout" = /pilot/repo ]
[ "$repository" = "$(cat /run/aep-worker/github-repository)" ] || {
    echo "The Worker deploy key belongs to another GitHub repository" >&2
    exit 1
}
case "$source" in refs/heads/*) ;; *) exit 2 ;; esac
case "$destination" in refs/aep/control/*|refs/heads/*) ;; *) exit 2 ;; esac
/usr/bin/git check-ref-format "$source"
/usr/bin/git check-ref-format "$destination"

proxy=${HTTPS_PROXY#http://}
case "$proxy" in ""|*[!a-zA-Z0-9.:-]*) echo "Invalid GitHub egress proxy" >&2; exit 2 ;; esac
ssh="/usr/bin/ssh -F /dev/null -i /run/aep-worker/github-key -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/usr/local/libexec/aep-pilot/github-known-hosts -o GlobalKnownHostsFile=/dev/null -o HostKeyAlias=github.com -o HostKeyAlgorithms=ssh-ed25519 -o ConnectTimeout=20 -o 'ProxyCommand=/usr/bin/nc -X connect -x $proxy %h %p'"
trusted_git() {
    env -i PATH=/usr/bin:/bin HOME=/pilot/supervisor GIT_CONFIG_NOSYSTEM=1 \
        GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND="$ssh" /usr/bin/git "$@"
}

# Keep privileged Git in a supervisor-owned repository. The agent only receives
# the fetched ref as a bundle and imports it under its existing unprivileged UID.
cache=/pilot/supervisor/github.git
transfer=$(mktemp -d /tmp/aep-github.XXXXXX)
trap 'rm -rf -- "$transfer"' EXIT
trusted_git init --quiet --bare --template= "$cache"
ref=refs/heads/aep-transfer
trusted_git --git-dir="$cache" fetch --quiet --no-tags --no-recurse-submodules \
    "ssh://git@ssh.github.com:443/$repository.git" "+$source:$ref"
trusted_git --git-dir="$cache" bundle create "$transfer/fetched.bundle" "$ref"
chmod 0444 "$transfer/fetched.bundle"
chmod 0711 "$transfer"
/usr/local/libexec/aep-pilot/git -C "$checkout" fetch --quiet --no-tags --no-recurse-submodules \
    "$transfer/fetched.bundle" "$ref:$destination"
