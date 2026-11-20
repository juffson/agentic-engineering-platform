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

[[ $name =~ ^[a-z][a-z0-9-]{0,38}[a-z0-9]$ && $port =~ ^(0|[0-9]{4,5})$ ]] || {
    echo "Set a valid AEP_PILOT_NAME and AEP_PILOT_PORT" >&2
    exit 2
}

slot=${AEP_PILOT_WORKER_SLOT:-}
[[ -z $slot || $slot =~ ^[a-z0-9][a-z0-9-]{0,15}$ ]] || { echo "Invalid Worker slot" >&2; exit 2; }
suffix=${slot:+-$slot}
worker_name=$name-worker$suffix
workspace_volume=$name-workspace$suffix
worker_keys_volume=$name-worker-keys$suffix
[[ ${#worker_name} -le 63 ]]

worker_config() {
    local directory=$1
    [[ -z $slot && -r $directory/workers.json ]]
    jq -e 'type=="object" and length<=32 and all(to_entries[]; (.key|test("^[a-z0-9][a-z0-9-]{0,62}$")) and (.value|type=="string" and test("^[A-Za-z0-9-]{32,128}$")))' "$directory/workers.json" >/dev/null
    [[ $(docker volume inspect -f '{{index .Labels "org.aep.pilot"}}' "$name-control-keys") == "$name" ]]
    private_files "$directory" "$name-control-keys" 65532 65532 workers.json
}

worker_keys() (
    local directory=$1 staging
    [[ -n $slot && -r $directory/workers.json && -r $directory/ca.pem ]]
    umask 077
    staging=$(mktemp -d)
    trap 'for file in "$staging/device" "$staging/site-ca.pem"; do if [[ -f $file ]]; then unlink "$file"; fi; done; rmdir "$staging"' EXIT
    jq -er --arg worker "$worker_name" '.[$worker] | select(type=="string" and test("^[A-Za-z0-9-]{32,128}$"))' "$directory/workers.json" > "$staging/device"
    cp "$directory/ca.pem" "$staging/site-ca.pem"
    volume "$workspace_volume"
    volume "$worker_keys_volume"
    private_files "$staging" "$worker_keys_volume" 0 0 device site-ca.pem
)

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
            -CAkey "$directory/ca.key" -CAserial "$directory/ca.srl" -CAcreateserial -days 60 -sha256 \
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

control_service() {
    local binding="127.0.0.1:$port:8443"
    if [[ $port == 0 ]]; then binding="127.0.0.1::8443"; fi
    resume_or_run "$name-control" --network "$ingress" --restart unless-stopped \
        --publish "$binding" --cap-drop ALL --security-opt no-new-privileges --read-only \
        --mount "type=volume,src=$name-control-state,dst=/var/lib/aep" \
        --mount "type=volume,src=$name-control-keys,dst=/run/aep-control,readonly" \
        "$control_image" --device "$name-worker" --lease-seconds "${AEP_PILOT_LEASE_SECONDS:-30}"
    if [[ $(docker container inspect -f "{{if index .NetworkSettings.Networks \"$private\"}}joined{{end}}" "$name-control") != joined ]]; then
        docker network connect "$private" "$name-control"
    fi
}

https_service() {
    resume_or_run "$name-https" --network "container:$name-control" --restart unless-stopped \
        --cap-drop ALL --security-opt no-new-privileges --read-only --user 101:101 \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
        --mount "type=volume,src=$name-tls,dst=/run/aep-tls,readonly" \
        --mount "type=bind,src=$script_dir/nginx.conf,dst=/etc/nginx/nginx.conf,readonly" \
        --entrypoint nginx "$nginx_image" -g 'daemon off;'
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
    control_service
    https_service
    printf 'Operator HTTPS endpoint: https://%s\n' "$(docker port "$name-control" 8443/tcp)"
}

proxy_arg() {
    printf 'http://%s-egress:3128' "$name"
}

github_origin() {
    [[ $1 =~ ^https://github\.com/([a-zA-Z0-9_-]+/([a-zA-Z0-9._-]+))$ ]] || return 2
    local repository=${BASH_REMATCH[1]}
    repository=${repository%.git}
    [[ $repository != */ && $repository != */. && $repository != */.. ]] || return 2
    printf '%s' "$repository" | tr '[:upper:]' '[:lower:]'
}

repo_key() (
    [[ $# == 1 ]]
    local repository staging
    repository=$(github_origin "$1") || { echo "Expected a GitHub HTTPS origin" >&2; exit 2; }
    [[ $(docker volume inspect -f '{{index .Labels "org.aep.pilot"}}' "$worker_keys_volume") == "$name" ]]
    umask 077
    staging=$(mktemp -d)
    trap 'rm -f -- "$staging/github-key" "$staging/github-repository"; rmdir "$staging"' EXIT
    cat > "$staging/github-key"
    ssh-keygen -y -P '' -f "$staging/github-key" </dev/null >/dev/null
    printf '%s\n' "$repository" > "$staging/github-repository"
    private_files "$staging" "$worker_keys_volume" 0 0 github-key github-repository
)

repo_bind() (
    [[ $# == 1 ]]
    local repository staging
    repository=$(github_origin "$1") || { echo "Expected a GitHub HTTPS origin" >&2; exit 2; }
    [[ $(docker volume inspect -f '{{index .Labels "org.aep.pilot"}}' "$worker_keys_volume") == "$name" ]]
    umask 077
    staging=$(mktemp -d)
    trap 'rm -f -- "$staging/github-repository"; rmdir "$staging"' EXIT
    printf '%s\n' "$repository" > "$staging/github-repository"
    private_files "$staging" "$worker_keys_volume" 0 0 github-repository
)

prepare_workspace() {
    local repo=$1 branch=$2 git_name=$3 git_email=$4 proxy repository
    proxy=$(proxy_arg)
    repository=$(github_origin "$repo") || { echo "Expected a GitHub HTTPS origin" >&2; exit 2; }
    docker run --rm --network "$private" --dns 127.0.0.1 --security-opt no-new-privileges \
        --mount "type=volume,src=$workspace_volume,dst=/pilot" \
        --mount "type=volume,src=$worker_keys_volume,dst=/run/aep-worker,readonly" \
        --env "HTTPS_PROXY=$proxy" --env "HTTP_PROXY=$proxy" --entrypoint /bin/sh \
        "$worker_image" -ec '
            git=/usr/local/libexec/aep-pilot/git
            if [ -f /run/aep-worker/github-repository ]; then
                [ "$5" = "$(cat /run/aep-worker/github-repository)" ] || {
                    echo "The Worker deploy key belongs to another GitHub repository" >&2
                    exit 1
                }
            fi
            if [ ! -d /pilot/repo/.git ]; then
                install -d -m 700 /pilot/supervisor
                install -d -m 700 -o 10001 -g 10001 /pilot/agent /pilot/agent/.codex /pilot/agent/.claude /pilot/tmp /pilot/cargo /pilot/build
                install -d -m 755 -o 10001 -g 10001 /pilot/repo
                cp -a /opt/aep-cargo/registry /pilot/cargo/
                chown -R 10001:10001 /pilot/cargo
                if [ -f /run/aep-worker/github-repository ]; then
                    "$git" -C /pilot/repo init --quiet --template=
                    "$git" -C /pilot/repo remote add origin "$1"
                else
                    "$git" clone --branch "$2" --single-branch --no-tags -- "$1" /pilot/repo
                fi
            fi
            if [ "$("$git" -C /pilot/repo config --get remote.origin.url)" != "$1" ]; then
                echo "The existing Worker workspace belongs to another Git origin" >&2
                exit 1
            fi
            if [ -f /run/aep-worker/github-key ] && ! "$git" -C /pilot/repo rev-parse --verify HEAD >/dev/null 2>&1; then
                ref=refs/heads/$2
                /usr/local/libexec/aep-pilot/private-fetch.sh /pilot/repo "$5" "$ref" refs/aep/control/bootstrap
                "$git" -C /pilot/repo switch -c "$2" refs/aep/control/bootstrap
                "$git" -C /pilot/repo update-ref -d refs/aep/control/bootstrap
            fi
            "$git" -C /pilot/repo config user.name "$3"
            "$git" -C /pilot/repo config user.email "$4"
            # The supervisor and unprivileged executor share each Run cancellation lock.
            test ! -L /pilot/repo/.git && test ! -L /pilot/repo/.git/aep && test ! -L /pilot/repo/.git/aep/run-control
            install -d -m 700 -o 10001 -g 10001 /pilot/repo/.git/aep
            install -d -m 2770 -o 10001 -g 10001 /pilot/repo/.git/aep/run-control
        ' provision "$repo" "$branch" "$git_name" "$git_email" "$repository"
}

model_credential() {
    local provider=$1 path
    case "$provider" in
        codex) path=/pilot/agent/.codex/auth.json ;;
        claude) path=/pilot/agent/.claude/.credentials.json ;;
        *) return 2 ;;
    esac
    docker run --rm -i --network none --mount "type=volume,src=$workspace_volume,dst=/pilot" \
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
    # The supervisor uses SETPCAP to clear the agent's capability bounding set.
    docker run "${lifecycle[@]}" --init --name "$worker_name" --label "$label" --network "$private" \
        --dns 127.0.0.1 --cap-drop ALL --cap-add DAC_OVERRIDE --cap-add SETUID --cap-add SETGID --cap-add SETPCAP \
        --security-opt no-new-privileges --read-only --pids-limit 256 \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
        --env "AEP_PILOT_ORIGIN=https://$name-control:8443" \
        --env "HTTPS_PROXY=$proxy" --env "HTTP_PROXY=$proxy" --env "NO_PROXY=$name-control" \
        --mount "type=volume,src=$workspace_volume,dst=/pilot" \
        --mount "type=volume,src=$worker_keys_volume,dst=/run/aep-worker,readonly" \
        "$worker_image" "$@"
}

export_runs() {
    local output=$1 run partial=no
    shift
    if [[ ${1:-} == --partial ]]; then partial=yes; shift; fi
    [[ $# -gt 0 ]]
    for run in "$@"; do
        [[ $run =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]]
    done
    umask 077
    mkdir -p "$output"
    local staging
    staging=$(mktemp -d "$output/.transfer.XXXXXX")
    docker run --rm --network none --read-only --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
        --mount "type=volume,src=$workspace_volume,dst=/pilot,readonly" \
        --entrypoint /bin/sh "$worker_image" -ec '
            partial=$1
            shift
            repo=/pilot/repo
            data=$repo/.git/aep
            journal=/pilot/supervisor/journal
            work=$(mktemp -d)
            for run do
                if [ "$(readlink -f "$journal/$run")" = "$journal/$run" ]; then
                    for file in worker-error.txt worker.log; do
                        source="$journal/$run/$file"
                        if [ -f "$source" ] && [ ! -L "$source" ]; then
                            mkdir -p "$work/supervisor/$run"
                            cp "$source" "$work/supervisor/$run/"
                        fi
                    done
                fi
                folder="$data/runs/$run"
                if [ "$partial" = yes ] && [ ! -d "$folder" ]; then continue; fi
                test "$(readlink -f "$folder")" = "$folder"
                mkdir -p "$work/runs/$run"
                for file in evidence.json implementation.log handoff.json checks.log review.txt review-result.json; do
                    source="$folder/$file"
                    if [ -f "$source" ] && [ ! -L "$source" ]; then
                        cp "$source" "$work/runs/$run/"
                    fi
                done
            done
            if [ "$(readlink -f "$data")" = "$data" ] && [ -f "$data/memory.sqlite3" ] && [ ! -L "$data/memory.sqlite3" ]; then
                cp "$data/memory.sqlite3" "$work/"
            fi
            count=$#
            for run do
                ref="refs/aep/runs/$run"
                if [ "$partial" = yes ] && ! git -c safe.directory="$repo" -C "$repo" show-ref --verify --quiet "$ref"; then continue; fi
                set -- "$@" "$ref"
            done
            while [ "$count" -gt 0 ]; do shift; count=$((count - 1)); done
            if [ "$#" -gt 0 ]; then git -c safe.directory="$repo" -C "$repo" bundle create "$work/runs.bundle" "$@"; fi
            tar -C "$work" -cf - .
        ' export "$partial" "$@" | tar -xf - -C "$staging"
    # The caller chooses a fresh evidence directory for each transfer.
    for run in runs memory.sqlite3 runs.bundle supervisor; do
        if [[ -e $staging/$run ]]; then
            [[ ! -e $output/$run ]]
            mv "$staging/$run" "$output/$run"
        fi
    done
    rmdir "$staging"
}

check_gate() {
    case "$1" in hold|release) ;; *) return 2 ;; esac
    [[ $(docker volume inspect -f '{{index .Labels "org.aep.pilot"}}' "$workspace_volume") == "$name" ]]
    docker run --rm --network none --read-only \
        --mount "type=volume,src=$workspace_volume,dst=/pilot" \
        "$helper_image" /bin/sh -ec '
            install -d -m 755 /pilot/drill
            case "$1" in
                hold) install -m 444 /dev/null /pilot/drill/hold-checks ;;
                release) rm -f /pilot/drill/hold-checks ;;
            esac
        ' gate "$1"
}

remove_site() {
    local suffix object key driver existing
    local -a objects
    for driver in container network volume; do
        case "$driver" in
            container) objects=(worker https control egress); key=.Config.Labels ;;
            network) objects=(private ingress outbound); key=.Labels ;;
            volume) objects=(control-state control-keys tls worker-keys workspace); key=.Labels ;;
        esac
        if [[ $driver == container || $driver == volume ]]; then
            while IFS= read -r existing; do
                case "$driver:$existing" in
                    container:"$name-worker-"*|volume:"$name-worker-keys-"*|volume:"$name-workspace-"*)
                        objects+=("${existing#"$name-"}") ;;
                esac
            done < <(
                if [[ $driver == volume ]]; then
                    docker volume ls --filter "label=$label" --format '{{.Name}}'
                else
                    docker container ls -a --filter "label=$label" --format '{{.Names}}'
                fi
            )
        fi
        for suffix in "${objects[@]}"; do
            object="$name-$suffix"
            if ! docker "$driver" inspect "$object" >/dev/null 2>&1; then continue; fi
            [[ $(docker "$driver" inspect -f "{{index $key \"org.aep.pilot\"}}" "$object") == "$name" ]] || return 1
            if [[ $driver == container ]]; then
                docker container rm --force "$object" >/dev/null
            else
                docker "$driver" rm "$object" >/dev/null
            fi
        done
    done
}

case "${1:-}" in
    serve) shift; service "$@" ;;
    upgrade) shift; source "$script_dir/upgrade.sh"; upgrade_site "$@" ;;
    worker-config) shift; worker_config "$@" ;;
    worker-keys) shift; worker_keys "$@" ;;
    repo-key) shift; repo_key "$@" ;;
    repo-bind) shift; repo_bind "$@" ;;
    workspace) shift; prepare_workspace "$@" ;;
    credential) shift; model_credential "$@" ;;
    worker) shift; worker "$@" ;;
    export) shift; export_runs "$@" ;;
    gate) shift; check_gate "$@" ;;
    remove) shift; remove_site "$@" ;;
    *) echo "Usage: $0 serve PRIVATE-DIR | upgrade ACCESS-DIR RECEIPT-DIR | worker-config ACCESS-DIR | worker-keys ACCESS-DIR | repo-bind HTTPS-REPO | repo-key HTTPS-REPO < PRIVATE-KEY | workspace HTTPS-REPO BRANCH GIT-NAME GIT-EMAIL | credential codex|claude < AUTH-JSON | worker [WORKER-ARGS] | export EVIDENCE-DIR RUN [RUN...] | gate hold|release | remove" >&2; exit 2 ;;
esac
