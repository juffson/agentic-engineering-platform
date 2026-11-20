#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
name=${AEP_PILOT_NAME:-aep-pilot}
port=${AEP_PILOT_PORT:-18443}
private=$name-private
outbound=$name-outbound
ingress=$name-ingress
control_image=${AEP_PILOT_CONTROL_IMAGE:-aep-pilot-control:trial}
worker_image=${AEP_PILOT_WORKER_IMAGE:-aep-pilot-worker:trial}
egress_image=${AEP_PILOT_EGRESS_IMAGE:-aep-pilot-egress:trial}
nginx_image=${AEP_PILOT_NGINX_IMAGE:-nginx:1-alpine@sha256:fb4658584bd9a916ffb6f6e365f97c25cfdb2923801f551ea08356e695f14ff7}
helper_image=${AEP_PILOT_HELPER_IMAGE:-debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc}
label=org.aep.pilot=$name

[[ $name =~ ^[a-z][a-z0-9-]{0,38}[a-z0-9]$ && $port =~ ^[0-9]{4,5}$ ]] || {
    echo "Set a valid AEP_PILOT_NAME and AEP_PILOT_PORT" >&2
    exit 2
}

assets() {
    local directory=$1
    umask 077
    mkdir -p -- "$directory"
    chmod 700 "$directory"
    if [[ ! -f $directory/ca.pem ]]; then
        openssl req -new -x509 -nodes -newkey rsa:3072 -sha256 -days 90 \
            -subj "/CN=AEP pilot CA ($name)" \
            -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
            -addext 'keyUsage=critical,keyCertSign,cRLSign' \
            -keyout "$directory/ca.key" -out "$directory/ca.pem" 2>/dev/null
    fi
    if [[ ! -f $directory/server.pem ]]; then
        openssl req -new -nodes -newkey rsa:2048 \
            -subj "/CN=$name-control" -keyout "$directory/server.key" \
            -out "$directory/server.csr" 2>/dev/null
        openssl x509 -req -in "$directory/server.csr" -CA "$directory/ca.pem" \
            -CAkey "$directory/ca.key" -CAcreateserial -days 60 -sha256 \
            -extfile <(printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:%s-control,IP:127.0.0.1\n' "$name") \
            -out "$directory/server.pem"
    fi
    for role in operator device; do
        if [[ ! -f $directory/$role ]]; then
            openssl rand -hex 32 > "$directory/$role"
        fi
    done
    cp "$directory/ca.pem" "$directory/site-ca.pem"
}

volume() {
    docker volume create --label "$label" "$1" >/dev/null
}

private_files() {
    local directory=$1 destination=$2 uid=$3 gid=$4
    shift 4
    # Stream only the named files. Credentials never become image layers or Docker environment.
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$directory" -cf - "$@" | \
        docker run --rm -i --network none --mount "type=volume,src=$destination,dst=/secrets" \
            "$helper_image" /bin/sh -ec 'tar -xf - -C /secrets; chown "$1:$2" /secrets /secrets/*; chmod 700 /secrets; chmod 400 /secrets/*' \
            provision "$uid" "$gid"
}

resume_or_run() {
    local container=$1
    shift
    if docker container inspect "$container" >/dev/null 2>&1; then
        [[ $(docker container inspect -f '{{index .Config.Labels "org.aep.pilot"}}' "$container") == "$name" ]]
        docker start "$container" >/dev/null
    else
        docker run -d --name "$container" --label "$label" "$@" >/dev/null
    fi
}

service() {
    local directory=$1
    assets "$directory"
    for network in "$private" "$outbound" "$ingress"; do
        if ! docker network inspect "$network" >/dev/null 2>&1; then
            if [[ $network == "$private" ]]; then
                docker network create --internal --label "$label" "$network" >/dev/null
            else
                docker network create --label "$label" "$network" >/dev/null
            fi
        else
            [[ $(docker network inspect -f '{{index .Labels "org.aep.pilot"}}' "$network") == "$name" ]]
        fi
    done
    for suffix in control-state control-keys tls worker-keys workspace; do
        volume "$name-$suffix"
    done
    private_files "$directory" "$name-control-keys" 65532 65532 operator device
    private_files "$directory" "$name-tls" 101 101 server.pem server.key
    private_files "$directory" "$name-worker-keys" 0 0 device site-ca.pem
    docker run --rm --network none --mount "type=volume,src=$name-control-state,dst=/state" \
        "$helper_image" /bin/sh -ec 'chown 65532:65532 /state; chmod 700 /state'

    resume_or_run "$name-egress" --network "$private" --restart unless-stopped \
        --cap-drop ALL --security-opt no-new-privileges --read-only \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 "$egress_image"
    if [[ $(docker container inspect -f "{{if index .NetworkSettings.Networks \"$outbound\"}}joined{{end}}" "$name-egress") != joined ]]; then
        docker network connect --gw-priority 1 "$outbound" "$name-egress"
    fi
    resume_or_run "$name-control" --network "$ingress" --restart unless-stopped \
        --publish "127.0.0.1:$port:8443" --cap-drop ALL --security-opt no-new-privileges --read-only \
        --mount "type=volume,src=$name-control-state,dst=/var/lib/aep" \
        --mount "type=volume,src=$name-control-keys,dst=/run/aep-control,readonly" \
        "$control_image" --device "$name-worker" --lease-seconds "${AEP_PILOT_LEASE_SECONDS:-30}"
    if [[ $(docker container inspect -f "{{if index .NetworkSettings.Networks \"$private\"}}joined{{end}}" "$name-control") != joined ]]; then
        docker network connect "$private" "$name-control"
    fi
    resume_or_run "$name-https" --network "container:$name-control" --restart unless-stopped \
        --cap-drop ALL --security-opt no-new-privileges --read-only --user 101:101 \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
        --mount "type=volume,src=$name-tls,dst=/run/aep-tls,readonly" \
        --mount "type=bind,src=$script_dir/nginx.conf,dst=/etc/nginx/nginx.conf,readonly" \
        --entrypoint nginx "$nginx_image" -g 'daemon off;'
    printf 'Operator HTTPS endpoint: https://127.0.0.1:%s\n' "$port"
}

proxy_arg() {
    printf 'http://%s-egress:3128' "$name"
}

prepare_workspace() {
    local repo=$1 branch=$2 git_name=$3 git_email=$4 proxy
    proxy=$(proxy_arg)
    [[ $repo =~ ^https://github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+$ ]] || {
        echo "The Worker accepts a public GitHub HTTPS origin without credentials" >&2
        exit 2
    }
    docker run --rm --network "$private" --dns 127.0.0.1 --security-opt no-new-privileges \
        --mount "type=volume,src=$name-workspace,dst=/pilot" \
        --env "HTTPS_PROXY=$proxy" --env "HTTP_PROXY=$proxy" --entrypoint /bin/sh \
        "$worker_image" -ec '
            test ! -e /pilot/repo/.git || exit 0
            install -d -m 700 /pilot/supervisor
            install -d -m 700 -o 10001 -g 10001 /pilot/agent /pilot/agent/.codex /pilot/agent/.claude /pilot/tmp /pilot/cargo /pilot/build
            install -d -m 755 -o 10001 -g 10001 /pilot/repo
            cp -a /opt/aep-cargo/registry /pilot/cargo/
            chown -R 10001:10001 /pilot/cargo
            git=/usr/local/libexec/aep-pilot/git
            "$git" clone --branch "$2" --single-branch --no-tags -- "$1" /pilot/repo
            "$git" -C /pilot/repo config user.name "$3"
            "$git" -C /pilot/repo config user.email "$4"
        ' provision "$repo" "$branch" "$git_name" "$git_email"
}

model_credential() {
    local provider=$1 path
    case "$provider" in
        codex) path=/pilot/agent/.codex/auth.json ;;
        claude) path=/pilot/agent/.claude/.credentials.json ;;
        *) return 2 ;;
    esac
    docker run --rm -i --network none --mount "type=volume,src=$name-workspace,dst=/pilot" \
        --entrypoint /bin/sh "$worker_image" -ec '
            umask 077
            profile=$(dirname "$1")
            temp=$(mktemp "$profile/.auth.XXXXXX")
            trap "rm -f -- \"$temp\"" EXIT
            cat > "$temp"
            test -s "$temp"
            node -e "try { const a=JSON.parse(require(\"fs\").readFileSync(process.argv[1],\"utf8\")); if (!a || Array.isArray(a) || Object.keys(a).length === 0) process.exit(1) } catch { process.exit(1) }" "$temp"
            chown 10001:10001 "$temp"
            mv -- "$temp" "$1"
        ' provision "$path"
}

worker() {
    local proxy
    local lifecycle=(--rm)
    if [[ ${1:-} == --detach ]]; then
        shift
        lifecycle=(--detach --restart unless-stopped)
    fi
    proxy=$(proxy_arg)
    docker run "${lifecycle[@]}" --init --name "$name-worker" --label "$label" --network "$private" \
        --dns 127.0.0.1 --cap-drop ALL --cap-add DAC_OVERRIDE --cap-add SETUID --cap-add SETGID \
        --security-opt no-new-privileges --read-only --pids-limit 256 \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
        --env "AEP_PILOT_ORIGIN=https://$name-control:8443" \
        --env "HTTPS_PROXY=$proxy" --env "HTTP_PROXY=$proxy" --env "NO_PROXY=$name-control" \
        --mount "type=volume,src=$name-workspace,dst=/pilot" \
        --mount "type=volume,src=$name-worker-keys,dst=/run/aep-worker,readonly" \
        "$worker_image" "$@"
}

export_runs() {
    local output=$1 run
    shift
    [[ $# -gt 0 ]]
    for run in "$@"; do
        [[ $run =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]]
    done
    umask 077
    mkdir -p "$output"
    local staging
    staging=$(mktemp -d "$output/.transfer.XXXXXX")
    docker run --rm --network none --read-only --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
        --mount "type=volume,src=$name-workspace,dst=/pilot,readonly" \
        --entrypoint /bin/sh "$worker_image" -ec '
            repo=/pilot/repo
            data=$repo/.git/aep
            test "$(readlink -f "$data")" = "$data"
            cd "$data"
            work=$(mktemp -d)
            for run do
                test "$(readlink -f "runs/$run")" = "$data/runs/$run"
                mkdir -p "$work/runs/$run"
                for file in evidence.json implementation.log handoff.json checks.log review.txt review-result.json; do
                    source="runs/$run/$file"
                    if [ -f "$source" ] && [ ! -L "$source" ]; then
                        cp "$source" "$work/runs/$run/"
                    fi
                done
            done
            if [ -f memory.sqlite3 ] && [ ! -L memory.sqlite3 ]; then
                cp memory.sqlite3 "$work/"
            fi
            count=$#
            for run do set -- "$@" "refs/aep/runs/$run"; done
            while [ "$count" -gt 0 ]; do shift; count=$((count - 1)); done
            git -c safe.directory="$repo" -C "$repo" bundle create "$work/runs.bundle" "$@"
            tar -C "$work" -cf - .
        ' export "$@" | tar -xf - -C "$staging"
    # The caller chooses a fresh evidence directory for each transfer.
    for run in runs memory.sqlite3 runs.bundle; do
        if [[ -e $staging/$run ]]; then
            [[ ! -e $output/$run ]]
            mv "$staging/$run" "$output/$run"
        fi
    done
    rmdir "$staging"
}

case "${1:-}" in
    serve) shift; service "$@" ;;
    workspace) shift; prepare_workspace "$@" ;;
    credential) shift; model_credential "$@" ;;
    worker) shift; worker "$@" ;;
    export) shift; export_runs "$@" ;;
    *) echo "Usage: $0 serve PRIVATE-DIR | workspace HTTPS-REPO BRANCH GIT-NAME GIT-EMAIL | credential codex|claude < AUTH-JSON | worker [WORKER-ARGS] | export EVIDENCE-DIR RUN [RUN...]" >&2; exit 2 ;;
esac
