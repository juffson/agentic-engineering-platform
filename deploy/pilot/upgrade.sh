# Sourced by site.sh. The original containers stay available until the new site is healthy.
upgrade_site() (
    set -euo pipefail
    umask 077
    [[ $# == 2 ]] || { echo "Usage: site.sh upgrade ACCESS-DIR RECEIPT-DIR" >&2; exit 2; }
    local access=$1 receipt=$2 lock='' maintenance=no committed=no id endpoint lease worker_json
    local repin=no old_control_image old_nginx_image legacy_fingerprint='' legacy_repository=''
    local suffix object actual
    local -a worker_args=() extra_paused=()
    [[ -z $slot ]] || { echo "Upgrade the site without a Worker slot"; exit 2; }
    [[ -r $access/operator && -r $access/ca.pem ]] ||
        { echo "Use the existing operator token and CA certificate" >&2; exit 2; }
    for tool in jq curl openssl; do command -v "$tool" >/dev/null; done
    mkdir -- "$receipt"
    id=$(openssl rand -hex 5)

    fail_upgrade() { printf '%s\n' "$*" >&2; exit 1; }
    owned() { [[ $(docker inspect -f '{{index .Config.Labels "org.aep.pilot"}}' "$1") == "$name" ]]; }
    backup_name() { printf '%s-%s-prev-%s' "$name" "$1" "$id"; }

    http() {
        printf 'header = "Authorization: Bearer %s"\n' "$(cat "$access/operator")" |
            curl --config - --noproxy '*' --silent --show-error --max-time 5 \
                --cacert "$access/ca.pem" "$@"
    }

    status() {
        local target=$1
        if http --fail --header 'Content-Type: application/json' --data '{"action":"status"}' \
            "$endpoint/v1/control" > "$target.tmp"; then
            mv -f "$target.tmp" "$target"
        else
            return 1
        fi
    }

    fingerprint() {
        # Some TLS clients return an EOF error after printing the leaf certificate.
        { openssl s_client -connect "${endpoint#https://}" </dev/null 2>/dev/null || true; } |
            openssl x509 -noout -fingerprint -sha256
    }

    idle() {
        jq -e '.result=="snapshot" and
          (.data.attempts | all(.[]; .stage | IN("interrupted","cancelled","failed","blocked","published","superseded")))' \
          "$1" >/dev/null
    }

    runtime_snapshot() {
        local target=$1
        docker container inspect "$name-control" "$name-https" "$name-worker" "$name-egress" |
            jq '[.[] | {name:.Name,id:.Id,image:.Image,argv:.Config.Cmd,running:.State.Running,
                  volumes:[.Mounts[]|select(.Type=="volume")|{name:.Name,destination:.Destination}],
                  port:.NetworkSettings.Ports["8443/tcp"],
                  binding:.HostConfig.PortBindings["8443/tcp"]}]' > "$target"
    }

    evidence_inventory() {
        docker run --rm --network none --read-only \
            --mount "type=volume,src=$name-control-state,dst=/state,readonly" \
            "$helper_image" sh -ec '
                cd /state
                if [ -d control.evidence ]; then
                    find control.evidence -type f -exec sha256sum {} +
                fi
            ' | LC_ALL=C sort > "$1"
    }

    stop_new() {
        local kind=$1 original kept
        original=$name-$kind
        kept=$(backup_name "$kind")
        docker container inspect "$kept" >/dev/null 2>&1 || return 0
        if docker container inspect "$original" >/dev/null 2>&1; then
            owned "$original" && docker stop -t 10 "$original" >/dev/null &&
                docker container rm "$original" >/dev/null
        fi
    }

    restore() {
        local kind original kept recreate=no rc=0
        kept=$(backup_name control)
        if [[ $repin == yes ]] && docker container inspect "$kept" >/dev/null 2>&1 &&
            [[ $(docker inspect -f '{{.State.Running}}' "$kept") != true ]]; then
            recreate=yes
        fi
        for kind in worker https control; do stop_new "$kind" || rc=1; done
        if [[ $recreate == yes ]]; then
            # Docker can allocate a different port when the old automatic binding restarts.
            (control_image=$old_control_image; nginx_image=$old_nginx_image; control_service && https_service) || return 1
        fi
        for kind in control https worker; do
            if [[ $recreate == yes && $kind != worker ]]; then continue; fi
            original=$name-$kind
            kept=$(backup_name "$kind")
            if docker container inspect "$kept" >/dev/null 2>&1; then
                owned "$kept" && docker rename "$kept" "$original" || { rc=1; continue; }
            fi
            owned "$original" || { rc=1; continue; }
            if [[ $(docker inspect -f '{{.State.Paused}}' "$original") == true ]]; then
                docker unpause "$original" >/dev/null || rc=1
            elif [[ $(docker inspect -f '{{.State.Running}}' "$original") != true ]]; then
                docker start "$original" >/dev/null || rc=1
            fi
        done
        if (( rc == 0 )); then
            for _ in {1..20}; do
                if status "$receipt/rollback-status.json"; then
                    if [[ $recreate == yes ]]; then
                        for kind in https control; do
                            kept=$(backup_name "$kind")
                            owned "$kept" && docker container rm "$kept" >/dev/null || return 1
                        done
                    fi
                    return 0
                fi
                sleep 1
            done
        fi
        return 1
    }

    finish_upgrade() {
        local code=$1 result=refused
        trap - EXIT
        set +e
        if [[ $committed == yes ]]; then
            result=upgraded
        elif [[ $maintenance == yes ]]; then
            if restore > "$receipt/rollback.log" 2>&1; then
                result=rolled_back
                echo "Upgrade failed. Restored the original services."
            else
                result=rollback_failed
                printf 'Rollback needs attention. See %s/rollback.log\n' "$receipt" >&2
                code=1
            fi
        fi
        for extra in ${extra_paused[@]+"${extra_paused[@]}"}; do
            docker unpause "$extra" >/dev/null || code=1
        done
        if [[ -n $lock ]]; then docker container rm "$lock" >/dev/null || code=1; fi
        jq -n --arg site "$name" --arg endpoint "${endpoint:-}" --arg result "$result" \
            --argjson exit_code "$code" '{site:$site,endpoint:$endpoint,result:$result,exit_code:$exit_code}' \
            > "$receipt/result.json"
        exit "$code"
    }
    trap 'finish_upgrade $?' EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    lock=$(docker container create --name "$name-upgrade-lock" --label "$label" --network none \
        --entrypoint /bin/true "$helper_image")
    docker image inspect "$control_image" "$worker_image" "$nginx_image" >/dev/null
    for suffix in control https worker egress; do
        object=$name-$suffix
        owned "$object" && [[ $(docker inspect -f '{{.State.Status}}' "$object") == running ]] ||
            fail_upgrade "Expected the labeled, running service $object"
    done
    for suffix in control-state control-keys worker-keys tls workspace; do
        [[ $(docker volume inspect -f '{{index .Labels "org.aep.pilot"}}' "$name-$suffix") == "$name" ]] ||
            fail_upgrade "Unexpected volume owner for $name-$suffix"
    done
    for object in "$ingress" "$private" "$outbound"; do
        [[ $(docker network inspect -f '{{index .Labels "org.aep.pilot"}}' "$object") == "$name" ]] ||
            fail_upgrade "Unexpected network owner for $object"
    done
    runtime_snapshot "$receipt/before-runtime.json"
    actual=$(docker port "$name-control" 8443/tcp)
    [[ $actual =~ ^127\.0\.0\.1:[0-9]{1,5}$ ]] || fail_upgrade "Expected one loopback HTTPS binding"
    endpoint=https://$actual
    port=${actual##*:}
    local binding
    binding=$(docker inspect -f '{{json (index .HostConfig.PortBindings "8443/tcp")}}' "$name-control" |
        jq -er 'select(length==1 and .[0].HostIp=="127.0.0.1") | .[0].HostPort | select(type=="string")') ||
        fail_upgrade "Expected one loopback port binding"
    [[ -z $binding || $binding == "$port" ]] || fail_upgrade "Unexpected loopback port binding"
    if [[ -z $binding ]]; then repin=yes; fi
    old_control_image=$(jq -er '.[0].image' "$receipt/before-runtime.json")
    old_nginx_image=$(jq -er '.[1].image' "$receipt/before-runtime.json")
    lease=$(jq -er --arg worker "$name-worker" '
      .[0].argv | select(length==4 and .[0]=="--device" and .[1]==$worker and .[2]=="--lease-seconds")
      | .[3] | select(test("^[0-9]+$"))' "$receipt/before-runtime.json") ||
        fail_upgrade "Unsupported control arguments"
    export AEP_PILOT_LEASE_SECONDS=$lease
    worker_json=$(jq -er '
        .[2].argv | select(type=="array" and all(.[]; type=="string" and (test("[\r\n]") | not)))
        | map(select(.!="--upload-evidence"))
        | select(length==0 or (length==2 and .[0]=="--codex-model" and (.[1]|length>0)))' "$receipt/before-runtime.json") ||
        fail_upgrade "Unsupported Worker arguments"
    while IFS= read -r actual; do worker_args+=("$actual"); done < <(jq -r '.[]' <<< "$worker_json")
    worker_args+=(--upload-evidence)

    status "$receipt/before-status.json"
    idle "$receipt/before-status.json" || fail_upgrade "Finish active tickets before upgrading"
    jq -e '.data.device.installation | type=="string" and length>0' "$receipt/before-status.json" >/dev/null ||
        fail_upgrade "The current site has no enrolled Worker"
    legacy_repository=$(docker exec "$name-worker" sh -ec '
        if [ -r /run/aep-worker/github-key ] && [ ! -e /pilot/supervisor/deploy-keys/keys.json ]; then
            cat /run/aep-worker/github-repository
        fi')
    if [[ -n $legacy_repository ]]; then
        legacy_fingerprint=$(docker exec "$name-worker" ssh-keygen -lf /run/aep-worker/github-key | awk 'NR==1 {print $2}')
        [[ $legacy_fingerprint == SHA256:* ]] || fail_upgrade "The legacy Deploy Key has no fingerprint"
        jq -n --arg worker "$name-worker" --arg repository "$legacy_repository" --arg fingerprint "$legacy_fingerprint" \
            '{worker:$worker,repository:$repository,fingerprint:$fingerprint}' > "$receipt/legacy-key.json"
    fi
    docker run --rm --network none --cap-drop ALL --user 101:101 --read-only \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
        --mount "type=volume,src=$name-tls,dst=/run/aep-tls,readonly" \
        --mount "type=bind,src=$script_dir/nginx.conf,dst=/etc/nginx/nginx.conf,readonly" \
        --entrypoint nginx "$nginx_image" -t > "$receipt/nginx-test.log" 2>&1
    fingerprint > "$receipt/before-certificate.txt"

    maintenance=yes
    while IFS= read -r object; do
        [[ $object == "$name-worker-"* ]] || continue
        owned "$object"
        docker pause "$object" >/dev/null
        extra_paused+=("$object")
    done < <(docker ps --filter "label=$label" --filter status=running --format '{{.Names}}')
    docker pause "$name-worker" >/dev/null
    status "$receipt/paused-status.json"
    idle "$receipt/paused-status.json" || fail_upgrade "A ticket became active while preparing the upgrade"
    docker run --rm --network none --read-only \
        --mount "type=volume,src=$name-workspace,dst=/pilot,readonly" \
        "$helper_image" sh -ec 'test ! -e /pilot/supervisor/journal/active.json' ||
        fail_upgrade "The paused Worker has an unfinished journal"
    # The suspended idle Worker cannot acquire a new Lease before it stops.
    docker kill "$name-worker" >/dev/null
    docker rename "$name-worker" "$(backup_name worker)"
    docker run --rm --network none --mount "type=volume,src=$name-workspace,dst=/pilot" \
        --entrypoint sh "$worker_image" -ec '
            test -d /pilot/repo/.git && test ! -L /pilot/repo/.git
            test ! -L /pilot/repo/.git/aep && test ! -L /pilot/repo/.git/aep/run-control
            install -d -m 700 -o 10001 -g 10001 /pilot/repo/.git/aep
            install -d -m 2770 -o 10001 -g 10001 /pilot/repo/.git/aep/run-control
        '
    for suffix in https control; do
        object=$(backup_name "$suffix")
        docker rename "$name-$suffix" "$object"
        docker stop -t 10 "$object" >/dev/null
    done
    # Keep a state-only backup in the operator receipt directory. TLS/model keys remain in their volumes.
    docker run --rm --network none --read-only \
        --mount "type=volume,src=$name-control-state,dst=/state,readonly" \
        "$helper_image" cat /state/control.json > "$receipt/control-backup.json"
    evidence_inventory "$receipt/before-evidence.sha256"
    control_service
    https_service

    local ready=no probe
    for _ in {1..25}; do
        if status "$receipt/new-control.json"; then ready=yes; break; fi
        sleep 1
    done
    [[ $ready == yes ]] || fail_upgrade "The replacement control plane did not become healthy"
    probe=$(http --header 'x-aep-ticket: upgrade-health' --output "$receipt/evidence-probe.json" \
        --write-out '%{http_code}' "$endpoint/v1/evidence/upgrade-health")
    [[ $probe == 409 ]] || fail_upgrade "The replacement evidence route returned HTTP $probe"
    fingerprint > "$receipt/after-certificate.txt"
    cmp "$receipt/before-certificate.txt" "$receipt/after-certificate.txt"
    worker --detach "${worker_args[@]}" >/dev/null

    ready=no
    for _ in {1..30}; do
        if status "$receipt/after-status.json" && jq -e --slurpfile before "$receipt/before-status.json" \
            --arg worker "$name-worker" --arg fingerprint "$legacy_fingerprint" --arg repository "$legacy_repository" '
            .data as $after | $before[0].data as $old |
            ($after.device.installation==$old.device.installation) and
            ($after.device.last_seen > $old.device.last_seen) and
            ($old.queue==null or $after.queue==$old.queue) and
            ($fingerprint=="" or ($after.deploy_keys[$worker].repository==$repository and
                $after.deploy_keys[$worker].inventory.active.fingerprint==$fingerprint)) and
            all($old.attempts[]; . as $run | any($after.attempts[];
                .==$run or (del(.worker)==$run and .worker==$old.device.id)))
        ' "$receipt/after-status.json" >/dev/null; then
            ready=yes
            break
        fi
        sleep 1
    done
    [[ $ready == yes ]] || fail_upgrade "The existing Worker did not reconnect with its original identity and history"
    evidence_inventory "$receipt/after-evidence.sha256"
    cmp "$receipt/before-evidence.sha256" "$receipt/after-evidence.sha256" ||
        fail_upgrade "Archived control evidence changed during the upgrade"
    runtime_snapshot "$receipt/after-runtime.json"
    committed=yes
    for suffix in worker https control; do
        object=$(backup_name "$suffix")
        owned "$object"
        docker logs "$object" > "$receipt/previous-$suffix.log" 2>&1
        docker container rm "$object" >/dev/null
    done
    printf 'Upgraded %s at %s. Receipts: %s\n' "$name" "$endpoint" "$receipt"
    exit 0
)
