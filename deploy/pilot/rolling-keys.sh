#!/usr/bin/env bash
set -euo pipefail
umask 077

source_dir=$(cd -- "$(dirname -- "$0")/../.." && pwd -P)
site='' repository='' access='' output='' studio_session=''
selected_workers=()
resume=no
while (( $# )); do
    case $1 in
        --help|-h)
            cat <<'USAGE'
Usage: deploy/pilot/rolling-keys.sh --site NAME --repo OWNER/PRIVATE-REPO
                                    --access-dir DIRECTORY [--output-dir DIRECTORY]

Run against an existing site deployed by site.sh with two online, idle Workers.
Use the site's ca.pem and operator files and the signed-in GitHub CLI account.
Each run creates two new test/https-rolling-* branches and their pinned Specs.
Studio renders offscreen. An operator must review and type every retirement,
publication and merge confirmation in the foreground terminal.
USAGE
            exit 0 ;;
        --resume) resume=yes; shift ;;
        --site|--repo|--access-dir|--output-dir|--studio-session|--worker)
            [[ $# -ge 2 && -n $2 && $2 != --* ]] || { echo "$1 requires a value" >&2; exit 2; }
            case $1 in
                --site) site=$2 ;;
                --repo) repository=$2 ;;
                --access-dir) access=$2 ;;
                --output-dir) output=$2 ;;
                --studio-session) studio_session=$2 ;;
                --worker) selected_workers+=("$2") ;;
            esac
            shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

failure=''
fail() { failure=$*; printf 'Rolling Deploy Key rehearsal: %s\n' "$failure" >&2; exit 1; }
[[ $site =~ ^[a-z][a-z0-9-]{0,38}[a-z0-9]$ &&
   $repository =~ ^[A-Za-z0-9_-]+/[A-Za-z0-9._-]+$ && -d $access ]] ||
    fail "Supply --site, --repo OWNER/PRIVATE-REPO and --access-dir (see --help)"
if [[ -n $studio_session ]]; then
    [[ -z $output && -d $studio_session && ${#selected_workers[@]} == 2 ]] ||
        fail "Studio requires a private session directory and two Workers"
    studio_session=$(cd -- "$studio_session" && pwd -P)
    exec 4>&1 >> "$studio_session/rolling.log" 2>&1
    export AEP_STUDIO_ROLLING_SESSION=$studio_session
else
    [[ $resume == no && -t 0 ]] || fail "Run in a terminal for Studio's confirmation checkpoints"
    unset AEP_STUDIO_ROLLING_SESSION
fi
if [[ -z $output ]]; then
    output=$(git -C "$source_dir" rev-parse --path-format=absolute --git-common-dir)/aep/https-rolling
fi
mkdir -p -- "$output"
id=""
if [[ -n $studio_session ]]; then
    root=$studio_session
else
    root=$(mktemp -d "$output/rolling.XXXXXX")
    root=$(cd -- "$root" && pwd -P)
fi
proof="$root/evidence"
checkout="$root/checkout"
mkdir -p "$proof"
stage=preflight plan='' first='' second='' key_slot='' first_task='' second_task=''

exec_site() {
    docker exec --user 0 "$@"
}

restore_slot() {
    exec_site "$first" sh -ec '
        file=/pilot/supervisor/deploy-keys/$1
        hidden=$file.rehearsal-$2
        if [ -e "$hidden" ]; then
            test ! -e "$file"
            mv -- "$hidden" "$file"
        fi
        test -f "$file"
    ' restore "$key_slot" "$id"
}

finish() {
    local status=$1 report
    if [[ -f $root/pause-request && $status != 0 ]]; then status=4; fi
    trap - EXIT
    set +e
    if [[ -n $key_slot ]] && restore_slot; then
        rm -f -- "$root/fault-slot"
    elif [[ -n $key_slot ]]; then
        status=1
        printf 'Could not restore the staged file on %s (slot %s, suffix .rehearsal-%s).\n' "$first" "$key_slot" "$id" >&2
    fi
    if (( status != 0 )) && [[ -d $checkout/.git && -n ${aep:-} && -n ${AEP_STUDIO_CONTROL_URL:-} ]] && ! snapshot "$proof/final-site-status.json"; then
        printf 'Could not read the site after the interruption.\n' >&2
    fi
    report=$(jq -n --argjson exit_code "$status" --arg stage "$stage" --arg repository "$repository" \
        --arg plan "$plan" --arg first_task "$first_task" --arg second_task "$second_task" --arg reason "$failure" \
        '{exit_code:$exit_code,stage:$stage,reason:$reason,repository:$repository,rotation:$plan,tasks:([$first_task,$second_task]|map(select(length>0)))}')
    printf '%s\n' "$report" > "$proof/result.json"
    mkdir -p "$proof/attempts"
    local attempt
    attempt=$(mktemp -d "$proof/attempts/attempt.XXXXXX")
    if [[ -n $attempt ]]; then
        cp "$proof/result.json" "$attempt/result.json"
        if [[ -f $root/gate.json ]]; then cp "$root/gate.json" "$attempt/pending-gate.json"; fi
    fi
    if COPYFILE_DISABLE=1 tar --no-xattrs -czf "$root/evidence.tar.gz.tmp" -C "$root" evidence &&
        gzip -t "$root/evidence.tar.gz.tmp" && mv "$root/evidence.tar.gz.tmp" "$root/evidence.tar.gz"; then
        printf 'Evidence: %s/evidence.tar.gz\n' "$root"
    else
        printf 'Could not package evidence. Check %s\n' "$proof" >&2
        (( status != 0 )) || status=1
    fi
    studio_status "$status"
    if (( status != 0 )); then
        printf 'Stopped in %s. Studio task checkout: %s\n' "$stage" "$checkout" >&2
    fi
    exit "$status"
}
trap 'finish $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

studio_status() {
    [[ -n $studio_session ]] || return 0
    jq -n --arg stage "$stage" --arg reason "$failure" --argjson exit_code "${1:-null}" \
        '{stage:$stage,reason:$reason,exit_code:$exit_code}' > "$root/rolling-state.tmp"
    mv -f "$root/rolling-state.tmp" "$root/rolling-state.json"
}

step() { stage=$1; studio_status; printf '\n%s\n' "$1"; }

snapshot() {
    AEP_CONTROL_OPERATOR_TOKEN="$(cat "$AEP_STUDIO_CONTROL_OPERATOR_FILE")" \
        "$aep" --repo "$checkout" control --url "$AEP_STUDIO_CONTROL_URL" --ca-cert "$AEP_STUDIO_CONTROL_CA" \
        status > "$1.tmp" && mv "$1.tmp" "$1"
}

git_trial() { git -C "$checkout" "$@"; }

active_github_keys() {
    gh api "repos/$repository/keys" --paginate --slurp | jq -e --slurpfile snapshot "$1" '
        [.[][]] as $registered |
        [$snapshot[0].data.deploy_keys | to_entries[] | . as $worker |
         ($registered[] | select((.key | split(" ") | .[0:2] | join(" "))==$worker.value.inventory.active.public)) |
         {worker:$worker.key,id,read_only,enabled,fingerprint:$worker.value.inventory.active.fingerprint}
        ] | select(length==2 and all(.[]; .read_only and .enabled) and ([.[].id]|unique|length)==2)
    ' > "$2"
}

studio() {
    local worker=$1 action=$2
    shift 2
    "$source_dir/desktop/target/debug/examples/ui_debug" \
        --live-https-keys --https-remote-repo "$checkout" --https-key-worker "$worker" \
        --output-dir "$proof/$action" "$@"
}

rotation_at() {
    jq -e --arg id "$plan" --argjson next "$2" '
        .data | (.queue.paused|not) and
        (.key_rotation | .id==$id and .next==$next and (.stopped|not) and (.workers|length)==2)
    ' "$1" >/dev/null
}

if [[ $resume == yes ]]; then
    rm -f -- "$root/pause-request"
    printf '\nResuming the same Studio rehearsal\n'
fi
step preflight
for tool in cargo docker gh git gzip jq openssl tar; do
    command -v "$tool" >/dev/null || fail "Install $tool"
done
access=$(cd -- "$access" && pwd -P)
[[ -s $access/operator && -s $access/ca.pem ]] || fail "The site access directory needs operator and ca.pem"
container() {
    [[ $(docker container inspect -f '{{.State.Running}} {{index .Config.Labels "org.aep.pilot"}}' "$1") == "true $site" ]] ||
        fail "Expected a running container for $site: $1"
}
container "$site-control"
container "$site-https"
address=$(docker port "$site-control" 8443/tcp)
[[ $address =~ ^127\.0\.0\.1:[0-9]+$ ]] || fail "Use the deployment host with the site's published HTTPS loopback port"
export AEP_STUDIO_CONTROL_URL="https://$address"
export AEP_STUDIO_CONTROL_CA="$access/ca.pem"
export AEP_STUDIO_CONTROL_OPERATOR_FILE="$access/operator"
export NO_PROXY="127.0.0.1${NO_PROXY:+,$NO_PROXY}"
export GIT_TERMINAL_PROMPT=0
unset AEP_CONTROL_DEVICE_TOKEN AEP_CONTROL_OPERATOR_TOKEN
cargo build --locked --quiet --manifest-path "$source_dir/Cargo.toml" --target-dir "$source_dir/target" --bin aep
aep="$source_dir/target/debug/aep"

if [[ -f $root/rehearsal-id ]]; then
    id=$(cat "$root/rehearsal-id")
else
    id=$(openssl rand -hex 8)
    printf '%s\n' "$id" > "$root/rehearsal-id.tmp"
    mv "$root/rehearsal-id.tmp" "$root/rehearsal-id"
fi
[[ $id =~ ^[a-f0-9]{16}$ ]] || fail "Invalid saved rehearsal ID"
first_task="https-rolling-$id-peer"
second_task="https-rolling-$id-switched"

meta=$(gh repo view "$repository" --json nameWithOwner,isPrivate,defaultBranchRef)
repository=$(jq -er 'select(.isPrivate) | .nameWithOwner | ascii_downcase' <<<"$meta") || fail "Select the Workers' private GitHub repository"
default=$(jq -er .defaultBranchRef.name <<<"$meta")
actor=$(gh api user --jq .login)
helper="!$(command -v gh) auth git-credential"
if [[ ! -d $checkout/.git ]]; then
    git -c "credential.https://github.com.helper=$helper" clone -q --no-tags --single-branch --branch "$default" \
        "https://github.com/$repository.git" "$checkout"
fi
[[ $(git_trial remote get-url origin) == "https://github.com/$repository.git" ]] || fail "The saved checkout belongs to another repository"
git_trial config "credential.https://github.com.helper" "$helper"
git_trial config user.name "$(git -C "$source_dir" config --get user.name)"
git_trial config user.email "$(git -C "$source_dir" config --get user.email)"
if [[ ! -f $root/identity.json ]]; then
    jq -n --arg repo "$repository" --arg actor "$actor" --arg branch "$default" --arg baseline "$(git_trial rev-parse HEAD)" \
        '{repo:$repo,actor:$actor,branch:$branch,baseline:$baseline}' > "$root/identity.tmp"
    mv "$root/identity.tmp" "$root/identity.json"
fi
baseline=$(jq -er --arg repo "$repository" --arg actor "$actor" --arg branch "$default" '
    select(.repo==$repo and .actor==$actor and .branch==$branch) | .baseline
' "$root/identity.json") || fail "The saved rehearsal GitHub repository, actor or default branch changed"
snapshot "$proof/live.json"
if [[ ! -f $proof/before.json ]]; then
    jq -e --arg repo "$repository" '
        .data as $s | ($s.queue.paused|not) and ($s.queue.order|length)==0 and
        all($s.attempts[]; .stage!="running" and .stage!="cancel_pending") and
        ($s.key_rotation==null or $s.key_rotation.stopped or $s.key_rotation.next==($s.key_rotation.workers|length)) and
        ($s.deploy_keys|length)==2 and
        all($s.deploy_keys|to_entries[]; . as $w | $s.workers[$w.key].online and
            $s.workers[$w.key].installation==$w.value.installation and $w.value.repository==$repo and
            $w.value.request==null and $w.value.inventory.active!=null and
            $w.value.inventory.retiring==null and $w.value.inventory.staged==null)
    ' "$proof/live.json" >/dev/null || fail "The site needs two idle Workers with active keys and no pending rotation or key changes"
    active_github_keys "$proof/live.json" "$proof/old-github-keys.tmp" ||
        fail "Each original key must be independent, enabled and read-only in GitHub"
    cp "$proof/live.json" "$proof/before.tmp"
    mv "$proof/old-github-keys.tmp" "$proof/old-github-keys.json"
    mv "$proof/before.tmp" "$proof/before.json"
fi

workers=$(jq -er '.data.deploy_keys|keys|join(" ")' "$proof/before.json")
read -r first second <<<"$workers"
if (( ${#selected_workers[@]} != 0 )); then
    [[ ${#selected_workers[@]} == 2 && ${selected_workers[0]} == "$first" && ${selected_workers[1]} == "$second" ]] ||
        fail "The selected Worker pair no longer matches this site's enrolled Workers"
fi
for worker in "$first" "$second"; do
    [[ $worker == "$site-worker" || $worker == "$site-worker-"* ]] || fail "Unexpected Worker: $worker"
    container "$worker"
    installed=$(exec_site "$worker" cat /pilot/supervisor/journal/installation.json)
    jq -e --arg w "$worker" --argjson installed "$installed" --slurpfile before "$proof/before.json" '
        .data.workers[$w] | .online and .installation==$installed and .installation==$before[0].data.workers[$w].installation
    ' "$proof/live.json" >/dev/null || fail "An enrolled Worker is offline or its installation changed"
done
if [[ -f $root/fault-slot ]]; then
    key_slot=$(cat "$root/fault-slot")
    [[ $key_slot =~ ^[a-f0-9]{32}$ ]] || { key_slot=''; fail "Invalid saved staged-key slot"; }
    restore_slot
    rm -f -- "$root/fault-slot"
    key_slot=''
fi

step build-studio
cargo build --locked --quiet --manifest-path "$source_dir/desktop/Cargo.toml" \
    --target-dir "$source_dir/desktop/target" --example ui_debug

prepare_task() (
    local task=$1 target="test/$1" dir="specs/$1" fixture="$source_dir/desktop/examples/support/handoff" spec
    local receipt="$proof/$task.json" target_ref="refs/heads/$target" dispatch_ref="refs/heads/aep-task/$task"
    if [[ ! -f $receipt ]]; then
        git_trial switch -q --detach "$baseline"
        mkdir -p "$checkout/$dir"
        sed "s/{task}/$task/g" "$source_dir/deploy/pilot/live-spec.md" > "$checkout/$dir/spec.md"
        cp "$fixture/normalize.rs" "$fixture/acceptance.rs" "$checkout/$dir/"
        jq -n --arg path "$dir" '
            {title:"Convert pilot fixture seconds to milliseconds",allowed_paths:[$path+"/normalize.rs"],criteria:["AC-1","AC-2"],
             check:["sh","-ec","binary=$(mktemp); trap '\''rm -f -- \"$binary\"'\'' EXIT; rustc --edition=2024 --test \"$1/acceptance.rs\" -o \"$binary\"; \"$binary\"","pilot-acceptance",$path]}
        ' > "$checkout/$dir/task.json"
        git_trial add -- "$dir"
        git_trial commit -qm "Pin rolling Deploy Key rehearsal Spec $task"
        spec=$(git_trial rev-parse HEAD)
        git_trial update-ref "refs/aep/rolling/$task" "$spec"
        jq -n --arg task "$task" --arg target "$target" --arg baseline "$baseline" --arg spec "$spec" \
            '{task:$task,target:$target,baseline:$baseline,spec:$spec}' > "$receipt.tmp"
        mv "$receipt.tmp" "$receipt"
    fi
    spec=$(jq -er --arg task "$task" --arg target "$target" --arg baseline "$baseline" '
        select(.task==$task and .target==$target and .baseline==$baseline) | .spec' "$receipt")
    local refs target_sha spec_sha
    refs=$(git_trial ls-remote --refs origin "$target_ref" "$dispatch_ref")
    target_sha=$(printf '%s\n' "$refs" | awk -v ref="$target_ref" '$2==ref{print $1}')
    spec_sha=$(printf '%s\n' "$refs" | awk -v ref="$dispatch_ref" '$2==ref{print $1}')
    if [[ $target_sha == "$baseline" && $spec_sha == "$spec" ]]; then return 0; fi
    [[ -z $target_sha && -z $spec_sha ]] || fail "The isolated task refs changed for $task"
    git_trial push --atomic --no-follow-tags --force-with-lease="$target_ref:" --force-with-lease="$dispatch_ref:" \
        origin "$baseline:$target_ref" "$spec:$dispatch_ref"
)

if [[ ! -f $root/prepared ]]; then
    step prepare-isolated-tasks
    prepare_task "$first_task"
    prepare_task "$second_task"
    git_trial switch -q "$default"
    touch "$root/prepared"
fi

step start-rotation
if [[ ! -f $root/plan-id ]]; then
    started=
    for receipt in "$proof/start-rotation"/studio-keys-*/rotation.json; do
        if [[ -f $receipt ]]; then
            [[ -z $started ]] || fail "Multiple rotation start receipts; inspect the site before continuing"
            started=$receipt
        fi
    done
    if [[ -z $started ]]; then
        jq -e --slurpfile before "$proof/before.json" '
            .data.key_rotation == $before[0].data.key_rotation
            and .data.deploy_keys == $before[0].data.deploy_keys
        ' "$proof/live.json" >/dev/null || fail "The site's rotation or keys changed before a start receipt was saved"
        studio "$first" "$stage" --https-key-step start
        for receipt in "$proof/start-rotation"/studio-keys-*/rotation.json; do
            if [[ -f $receipt ]]; then started=$receipt; fi
        done
    fi
    jq -er --arg f "$first" --arg s "$second" --arg repo "$repository" '
        select(.next==0 and .repository==$repo and (.workers|map(.worker))==[$f,$s]) | .id
    ' "$started" > "$root/plan-id.tmp"
    mv "$root/plan-id.tmp" "$root/plan-id"
fi
plan=$(cat "$root/plan-id")
export AEP_STUDIO_ROLLING_PLAN=$plan
snapshot "$proof/live.json"
jq -e --arg plan "$plan" --arg f "$first" --arg s "$second" --arg repo "$repository" --slurpfile before "$proof/before.json" '
    .data.key_rotation | .id==$plan and .repository==$repo and (.stopped|not)
    and (.workers|map(.worker))==[$f,$s]
    and all(.workers[]; . as $w | $before[0].data.deploy_keys[$w.worker].inventory.active.fingerprint==$w.original
        and $before[0].data.deploy_keys[$w.worker].installation==$w.installation)
' "$proof/live.json" >/dev/null || fail "The saved rotation changed"

wait_key_idle() {
    local worker=$1 i
    for ((i=0;i<45;i++)); do
        snapshot "$proof/live.json"
        if jq -e --arg w "$worker" '.data.deploy_keys[$w].request==null' "$proof/live.json" >/dev/null; then return 0; fi
        sleep 2
    done
    fail "The previous Deploy Key operation has not settled"
}

if [[ ! -f $proof/after-fault.json ]]; then
    wait_key_idle "$first"
    rotation_at "$proof/live.json" 0
    if jq -e --arg w "$first" '.data.deploy_keys[$w].inventory.staged==null' "$proof/live.json" >/dev/null; then
        studio "$first" stage-first --https-key-step stage
    fi
    snapshot "$proof/before-fault.json"
    rotation_at "$proof/before-fault.json" 0
    step simulate-failed-cutover
    staged=$(jq -er --arg w "$first" '.data.deploy_keys[$w].inventory.staged.fingerprint' "$proof/before-fault.json")
    key_slot=$(exec_site "$first" node -e '
        const keys = JSON.parse(require("node:fs").readFileSync("/pilot/supervisor/deploy-keys/keys.json"));
        const slot = keys.staged?.file;
        if (!/^[a-f0-9]{32}$/.test(slot) || keys.active?.file === slot || keys.staged?.key.fingerprint !== process.argv[1]) process.exit(1);
        console.log(slot);
    ' "$staged")
    printf '%s\n' "$key_slot" > "$root/fault-slot.tmp"
    mv "$root/fault-slot.tmp" "$root/fault-slot"
    exec_site "$first" sh -ec '
        file=/pilot/supervisor/deploy-keys/$1
        hidden=$file.rehearsal-$2
        test -f "$file" && test ! -e "$hidden" && mv -- "$file" "$hidden"
    ' inject "$key_slot" "$id"
    studio "$first" "$stage" --https-key-step expect-failure
    restore_slot
    rm -f -- "$root/fault-slot"
    key_slot=''
    snapshot "$proof/fault-result.json"
    rotation_at "$proof/fault-result.json" 0
    jq -e --arg w "$first" --slurpfile before "$proof/before-fault.json" '
        .data.deploy_keys[$w] | .inventory==$before[0].data.deploy_keys[$w].inventory and
        .request==null and .last.error=="GitHub could not be read with this Deploy Key"
    ' "$proof/fault-result.json" >/dev/null || fail "Failure did not preserve the old key and staged replacement"
    mv "$proof/fault-result.json" "$proof/after-fault.json"
fi

deliver() {
    local worker=$1 task=$2 position=$3 role=$4 artifacts result pr candidate merge head review
    if [[ -f $root/delivered-$role ]]; then return 0; fi
    local first_snapshot="$proof/dispatch-$role.json"
    if [[ ! -f $first_snapshot ]]; then
        snapshot "$first_snapshot"
    fi
    rotation_at "$first_snapshot" "$position"
    AEP_STUDIO_LIVE_TASK="$task" AEP_STUDIO_LIVE_TARGET="test/$task" AEP_STUDIO_ROLLING_TASK_PLAN="$proof/$task.json" AEP_STUDIO_ROLLING_DELIVERY="$role" \
        studio "$worker" "$role" --https-key-step deliver
    artifacts=
    for result in "$proof/$role"/studio-keys-*/result.json; do
        if [[ -f $result ]]; then artifacts=${result%/result.json}; fi
    done
    [[ -n $artifacts ]] || fail "Missing Studio checkpoint directory for $task"
    if [[ $position == 0 ]]; then
        jq -e --arg w "$first" --slurpfile fault "$proof/after-fault.json" '
            .data.deploy_keys[$w] | .last==$fault[0].data.deploy_keys[$w].last and
            .inventory.active==$fault[0].data.deploy_keys[$w].inventory.active
        ' "$first_snapshot" >/dev/null
    else
        jq -e --arg w "$first" '
            .data | .key_rotation.workers[0].replacement==.deploy_keys[$w].inventory.active.fingerprint and
            .deploy_keys[$w].inventory.retiring==null
        ' "$first_snapshot" >/dev/null
    fi
    result="$artifacts/result.json"
    jq -e --arg repo "$repository" --arg task "$task" --arg w "$worker" --slurpfile session "$artifacts/session-state.json" '
        .repository==$repo and .task==$task and .branch==("test/"+$task) and .stage=="completed" and .exit_code==0 and
        ($session[0] | .worker==$w and .archive and .exit_code==0)
    ' "$result" >/dev/null || fail "Archive does not match the isolated task and Worker: $task"
    pr=$(jq -er '.pr|tonumber' "$result")
    candidate=$(jq -er .candidate "$result")
    local bundle="$artifacts/pilot-evidence.tar.gz"
    gzip -t "$bundle"
    tar -xOf "$bundle" evidence/cleanup.json > "$artifacts/cleanup.json"
    review=$(tar -xOf "$bundle" evidence/confirmations.tsv)
    [[ $review == *$'\tpublish '"$candidate"* && $review == *$'\tmerge '"$pr $candidate"* ]] ||
        fail "The archive lacks the Studio publication or merge receipt for $task"
    gh pr view "$pr" -R "$repository" --json state,baseRefName,headRefName,headRefOid,mergeCommit,mergedBy,url,statusCheckRollup \
        > "$artifacts/github.json"
    jq -e --arg candidate "$candidate" --arg target "test/$task" --arg actor "$actor" --slurpfile cleanup "$artifacts/cleanup.json" '
        .state=="MERGED" and .baseRefName==$target and .headRefOid==$candidate and
        .mergedBy.login==$actor and .mergeCommit.oid==$cleanup[0].merge_sha and
        ($cleanup[0]|.target==$target and .target_synced and .candidate_branch_removed) and
        ([.statusCheckRollup[]|select(.name=="acceptance" and .conclusion=="SUCCESS")]|length)>0
    ' "$artifacts/github.json" >/dev/null || fail "GitHub merge and cleanup receipts disagree for $task"
    merge=$(jq -er .mergeCommit.oid "$artifacts/github.json")
    head=$(jq -er .headRefName "$artifacts/github.json")
    [[ $(git_trial ls-remote --refs origin "refs/heads/test/$task") == "$merge"$'\t'"refs/heads/test/$task" &&
       -z $(git_trial ls-remote --refs origin "refs/heads/$head") ]] || fail "GitHub branch cleanup did not match the merge for $task"
    for screenshot in 02-task-target-selected 03-peer-publication-review 04-peer-approved 05-peer-merge 06-peer-archive; do
        [[ -n $(find "$proof/$role" -name "$screenshot.png" -type f -size +0c -print -quit) ]] || fail "Missing Studio screenshot for $task: $screenshot"
    done
    touch "$root/delivered-$role"
    printf 'Verified %s on %s: %s\n' "$task" "$worker" "$(jq -r .url "$artifacts/github.json")"
}

rotate() {
    local worker=$1 position=$2
    if [[ -f $root/retired-$position ]]; then return 0; fi
    wait_key_idle "$worker"
    local next
    next=$(jq -er '.data.key_rotation.next' "$proof/live.json")
    rotation_at "$proof/live.json" "$next"
    if [[ $next == "$position" ]]; then
        if jq -e --arg w "$worker" '.data.deploy_keys[$w].inventory.retiring==null' "$proof/live.json" >/dev/null; then
            studio "$worker" "retry-or-switch-$position"
        elif ! jq -e --arg w "$worker" '.data.deploy_keys[$w].verified_at!=null' "$proof/live.json" >/dev/null; then
            studio "$worker" "verify-replacement-$position" --https-key-step verify
        fi
        snapshot "$proof/verified-$position.json"
        rotation_at "$proof/verified-$position.json" "$position"
        jq -e --arg w "$worker" --argjson n "$position" '
            .data as $d | $d.deploy_keys[$w] | .verified_at!=null and .inventory.staged==null and
            .inventory.active.fingerprint!=$d.key_rotation.workers[$n].original and
            .inventory.retiring.fingerprint==$d.key_rotation.workers[$n].original
        ' "$proof/verified-$position.json" >/dev/null
        active_github_keys "$proof/verified-$position.json" "$proof/verified-github-$position.json"
        studio "$worker" "confirm-retirement-$position" --https-key-revoke
    fi
    snapshot "$proof/retired-$position.json"
    rotation_at "$proof/retired-$position.json" "$((position + 1))"
    jq -e --arg w "$worker" --argjson n "$position" '
        .data as $d | $d.deploy_keys[$w] | .verified_at!=null and .inventory.retiring==null and
        .inventory.active.fingerprint==$d.key_rotation.workers[$n].replacement
    ' "$proof/retired-$position.json" >/dev/null
    local old_id
    old_id=$(jq -er --arg w "$worker" '.[]|select(.worker==$w)|.id' "$proof/old-github-keys.json")
    gh api "repos/$repository/keys" --paginate --slurp | jq -e --argjson id "$old_id" 'all(.[][]; .id!=$id)' >/dev/null
    touch "$root/retired-$position"
}

step dispatch-during-failure
deliver "$second" "$first_task" 0 peer
step retry-and-retire-first
rotate "$first" 0
step dispatch-after-first-retirement
deliver "$first" "$second_task" 1 switched
step rotate-and-retire-second
rotate "$second" 1

step verify
snapshot "$proof/final-site-status.json"
rotation_at "$proof/final-site-status.json" 2
jq -e --arg f "$first" --arg s "$second" '.data | .workers[$f].online and .workers[$s].online' "$proof/final-site-status.json" >/dev/null
active_github_keys "$proof/final-site-status.json" "$proof/new-github-keys.json"
jq -e --slurpfile old "$proof/old-github-keys.json" '([.[].id]-[$old[0][].id]|length)==2' "$proof/new-github-keys.json" >/dev/null
stage=completed
printf 'Both Workers have distinct replacement keys and two Studio delivery archives were verified.\n'
