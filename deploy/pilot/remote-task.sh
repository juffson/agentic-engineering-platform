#!/usr/bin/env bash
# Invoked by Studio in its private session directory.
set -euo pipefail
umask 077
[[ $# == 3 && -d $1 && -x $2 && -d $3 && -p /dev/stdin ]]
session=$1
aep=$2
source_repo=$3
exec 3<&0 </dev/null
request=$session/request.json
stage=preflight
failure='' repository='' task='' branch='' baseline='' spec='' candidate='' old_run='' recovered_run='' pr=''
evidence='' archive=''
ticket="https-task-$(openssl rand -hex 12)"

report() {
    jq -n --arg stage "$stage" --arg reason "$failure" --arg repository "$repository" --arg task "$task" \
        --arg branch "$branch" --arg ticket "$ticket" --arg baseline "$baseline" --arg spec "$spec" \
        --arg candidate "$candidate" --arg old "$old_run" --arg recovered "$recovered_run" --arg pr "$pr" \
        --arg archive "$archive" --argjson code "${1:-null}" \
        '{remote:true,stage:$stage,reason:$reason,repository:$repository,task:$task,branch:$branch,ticket:$ticket,
          baseline:$baseline,spec:$spec,candidate:$candidate,old_run:$old,recovered_run:$recovered,
          pr:(if $pr=="" then null else ($pr|tonumber) end),archive:($archive!=""),exit_code:$code}' > "$session/state.tmp"
    mv -f "$session/state.tmp" "$session/state.json"
}

fail() { failure=$*; printf '%s\n' "$failure" >&2; exit 1; }
step() { stage=$1; report; printf '%s\n' "$stage"; }
finish() {
    local code=$1
    trap - EXIT
    set +e
    if [[ -n $evidence ]]; then
        jq -n --arg stage "$stage" --arg repo "$repository" --arg branch "$branch" --arg ticket "$ticket" \
            --arg task "$task" --arg candidate "$candidate" --arg old "$old_run" --arg recovered "$recovered_run" \
            --arg pr "$pr" --arg reason "$failure" --argjson code "$code" \
            '{exit_code:$code,stage:$stage,repository:$repo,branch:$branch,ticket:$ticket,task:$task,candidate:$candidate,
              old_run:$old,recovered_run:$recovered,pr:$pr,reason:$reason}' > "$evidence/result.json"
        cp "$session/studio.log" "$evidence/studio.log"
        if tar -czf "$session/$task/evidence.tar.gz.tmp" -C "$session/$task" evidence &&
            gzip -t "$session/$task/evidence.tar.gz.tmp" &&
            mv -f "$session/$task/evidence.tar.gz.tmp" "$session/$task/evidence.tar.gz"; then
            archive=$session/$task/evidence.tar.gz
        else
            failure="Could not package evidence"
            (( code != 0 )) || code=1
        fi
    fi
    report "$code"
    exit "$code"
}
trap 'finish $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

retry_read() {
    local attempt
    for attempt in 1 2 3 4; do
        if "$@"; then return; fi
        if (( attempt < 4 )); then sleep "$((attempt * 3))"; fi
    done
    return 1
}

confirm() {
    local answer
    if ! IFS= read -r answer <&3 || [[ $answer != "$1" ]]; then
        failure="Stopped at the operator gate"
        printf '%s\n' "$failure"
        exit 3
    fi
    printf '%s\t%s\n' "$(date -u +%FT%TZ)" "$1" >> "$evidence/confirmations.tsv"
}

for tool in curl git gh jq openssl tar gzip; do
    command -v "$tool" >/dev/null || fail "Install $tool"
done
report
task=$(jq -er .task.name "$request")
branch=$(jq -er .branch.name "$request")
baseline=$(jq -er .branch.sha "$request")
spec=$(jq -er .task.sha "$request")
endpoint=$(jq -er .endpoint "$request")
ca=$(jq -r '.ca // empty' "$request")
token_file=$(jq -er .operator_file "$request")
[[ $task =~ ^[a-z0-9][a-z0-9-]{0,62}$ && $baseline =~ ^[a-f0-9]{40}$ &&
   $endpoint == https://* && -r $token_file ]] || fail "Invalid pinned task, HTTPS origin or operator token file"
authored=no
if [[ -z $spec ]]; then
    [[ -s $session/draft/spec.md && -s $session/draft/task.json ]] || fail "Missing Studio task draft"
    authored=yes
else
    [[ $spec =~ ^[a-f0-9]{40}$ ]] || fail "Invalid pinned Spec SHA"
fi
git check-ref-format "refs/heads/$branch"
opts=(--url "$endpoint")
if [[ -n $ca ]]; then
    [[ -r $ca ]] || fail "Cannot read the control CA file"
    opts+=(--ca-cert "$ca")
fi
export AEP_CONTROL_OPERATOR_TOKEN=$(cat "$token_file")
unset AEP_CONTROL_DEVICE_TOKEN
export GIT_TERMINAL_PROMPT=0
operator=$session/$task/operator
evidence=$session/$task/evidence
mkdir -p "$operator" "$evidence"
control() { "$aep" --repo "$operator" control "${opts[@]}" "$@"; }
source "$session/live-github.sh"

step remote_preflight
origin=$(git -C "$source_repo" remote get-url origin)
repository=$(retry_read gh repo view "$origin" --json nameWithOwner,isPrivate --jq 'select(.isPrivate==false)|.nameWithOwner')
[[ -n $repository ]] || fail "Use a public GitHub repository accessible by the remote Worker"
actor=$(retry_read gh api user --jq .login)
git -C "$operator" init --quiet --template=
git -C "$operator" remote add origin "$origin"
target_ref=refs/heads/$branch
spec_ref=refs/heads/aep-task/$task
refs=("$target_ref:refs/aep/remote/baseline")
if [[ $authored == no ]]; then refs+=("$spec_ref:refs/aep/remote/spec"); fi
retry_read git -C "$operator" fetch -q --no-tags origin "${refs[@]}"
[[ $(git -C "$operator" rev-parse refs/aep/remote/baseline) == "$baseline" ]] ||
    fail "The selected target changed. Refresh the repository branches and select again."
git -C "$operator" switch -qc "$branch" "$baseline"
retry_read control status > "$evidence/control-before.json"
jq -e --arg repository "$repository" '
    (.data.attempts | all(.[]; .task.repository==$repository)) and
    (.data.device.installation | type=="string" and length>0)
' "$evidence/control-before.json" >/dev/null ||
    fail "The control plane belongs to another repository or has no registered Worker"

if [[ $authored == yes ]]; then
    step remote_create
    existing_spec=$(retry_read git -C "$operator" ls-remote --refs origin "$spec_ref")
    [[ -z $existing_spec ]] ||
        fail "This task ID is already dispatched. Choose a new ID or select the existing task."
    for key in user.name user.email; do
        value=$(git -C "$source_repo" config --get "$key") ||
            fail "Configure Git $key in the selected repository"
        git -C "$operator" config --local "$key" "$value"
    done
    git -C "$operator" switch -q --detach "$baseline"
    [[ ! -L $operator/specs ]] || fail "The selected target must have a regular specs directory"
    mkdir -p "$operator/specs"
    mkdir "$operator/specs/$task" || fail "The selected target already contains this task directory"
    cp "$session/draft/spec.md" "$session/draft/task.json" "$operator/specs/$task/"
    git -C "$operator" add -- "specs/$task"
    git -C "$operator" -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm "Specify $task"
    spec=$(git -C "$operator" rev-parse HEAD)
    report
    git -C "$operator" switch -q --no-guess -- "$branch"
else
    [[ $(git -C "$operator" rev-parse refs/aep/remote/spec) == "$spec" ]] ||
        fail "The selected Spec changed. Refresh the repository branches and select again."
fi
for file in spec.md task.json; do git -C "$operator" show "$spec:specs/$task/$file" > "$evidence/$file"; done

if [[ $authored == yes ]]; then
    current_target=$(retry_read git -C "$operator" ls-remote --refs origin "$target_ref")
    [[ $current_target == "$baseline"$'\t'"$target_ref" ]] ||
        fail "The target changed before dispatch. Refresh the repository branches and select again."
    retry_read "$aep" --repo "$operator" dispatch --task "$task" --rev "$spec" > "$evidence/spec-dispatch.txt"
    jq -n --arg task "$task" --arg spec "$spec" --arg target "$branch" --arg base "$baseline" \
        --arg ref "$spec_ref" '{task:$task,spec:$spec,target:$target,baseline:$base,dispatch_ref:$ref}' \
        > "$evidence/authoring.json"
fi
jq -n --arg repo "$repository" --arg actor "$actor" --arg ticket "$ticket" --arg task "$task" \
    --arg branch "$branch" --arg baseline "$baseline" --arg spec "$spec" \
    '{repository:$repo,actor:$actor,ticket:$ticket,task:$task,target:$branch,baseline:$baseline,spec:$spec}' > "$evidence/plan.json"

step remote_dispatch
retry_read control submit --ticket "$ticket" --task "$task" --spec "$spec" --target "$branch" \
    --baseline "$baseline" --max-attempts 2 > "$evidence/submission.json"
old_run=$(jq -er '.data.id' "$evidence/submission.json")
step candidate

wait_run() {
    local run=$1 work_stage=$2 deadline=$((SECONDS + 900)) offline=no snapshot active sha phase next
    while (( SECONDS < deadline )); do
        if ! control status > "$evidence/status.tmp" 2> "$evidence/control-last-error.log"; then
            if [[ $offline == no ]]; then
                offline=yes
                printf '%s\tdisconnected\t%s\n' "$(date -u +%FT%TZ)" "$run" >> "$evidence/connection.tsv"
                cp "$evidence/control-last-error.log" "$evidence/control-disconnection.log"
                step remote_outage
            fi
            sleep 2
            continue
        fi
        mv -f "$evidence/status.tmp" "$evidence/control-status.json"
        snapshot=$(jq -ce --arg run "$run" --arg ticket "$ticket" '.data.attempts[]|select(.id==$run and .task.ticket==$ticket)' "$evidence/control-status.json")
        printf '%s\n' "$snapshot" > "$evidence/run-$run.tmp"
        mv -f "$evidence/run-$run.tmp" "$evidence/run-$run.json"
        active=$(jq -r .stage <<<"$snapshot")
        sha=$(jq -r '[.events[].progress.candidate|select(.!=null)]|last // ""' <<<"$snapshot")
        phase=$(jq -r '.events[-1].progress.phase // ""' <<<"$snapshot")
        if [[ $offline == yes ]]; then
            offline=no
            printf '%s\treconnected\t%s\n' "$(date -u +%FT%TZ)" "$run" >> "$evidence/connection.tsv"
            step reconnect
        fi
        next=$work_stage
        if [[ $active == interrupted ]]; then
            [[ -f $evidence/interrupted-$run.json ]] || cp "$evidence/run-$run.json" "$evidence/interrupted-$run.json"
            next=reconnect
        elif [[ $work_stage == candidate && $active == running ]]; then
            case $phase in
                checking) next=remote_checking ;;
                reviewing) next=remote_reviewing ;;
            esac
        fi
        if [[ -n $sha && $sha != "$candidate" ]]; then candidate=$sha; report; fi
        if [[ $stage != "$next" ]]; then
            step "$next"
        fi
        if jq -e '.events[-1].progress.type == "exit"' <<<"$snapshot" >/dev/null; then return; fi
        sleep 2
    done
    fail "Timed out waiting for the remote Run or its terminal event replay"
}

collect() {
    local run=$1 output="$evidence/transfer/$1" deadline=$((SECONDS + 100))
    mkdir -p "$evidence/transfer"
    while (( SECONDS < deadline )); do
        if control collect --ticket "$ticket" --run "$run" --output "$output" > "$evidence/collect-$run.log" 2>&1; then
            break
        fi
        sleep 2
    done
    [[ -d $output ]] || fail "Worker evidence is unavailable for $run. Check --upload-evidence and the HTTPS route."
    if [[ -d $output/run ]]; then
        mkdir -p "$evidence/worker/runs" "$operator/.git/aep/runs"
        cp -R "$output/run" "$evidence/worker/runs/$run"
        cp -R "$output/run" "$operator/.git/aep/runs/$run"
    fi
    if [[ -s $output/memory.sqlite3 ]]; then cp "$output/memory.sqlite3" "$evidence/worker/memory.sqlite3"; fi
    if [[ -s $output/candidate.bundle ]]; then
        git -C "$operator" fetch -q "$output/candidate.bundle" "refs/aep/runs/$run:refs/aep/runs/$run"
    elif [[ $(jq -r '.candidate_sha // ""' "$output/run/evidence.json" 2>/dev/null || true) == "$spec" ]]; then
        git -C "$operator" update-ref "refs/aep/runs/$run" "$spec"
    fi
}

wait_run "$old_run" candidate
collect "$old_run"
first_stage=$(jq -r .stage "$evidence/run-$old_run.json")
if [[ $first_stage == interrupted ]]; then
    previous_candidate=$candidate
    retry=(retry --ticket "$ticket")
    recovery_stage=remote_retry
    if [[ -n $previous_candidate ]]; then
        recovery_stage=recovery
        [[ $(git -C "$operator" rev-parse "refs/aep/runs/$old_run") == "$previous_candidate" ]] ||
            fail "The interrupted Run has no matching candidate evidence"
        retry+=(--reverify)
        payload=$(jq -nc --arg ticket "$ticket" --arg run "$old_run" --arg spec "$spec" --arg head "$previous_candidate" \
            '{action:"begin_publication",ticket:$ticket,run:$run,spec:$spec,candidate:$head,report_sha256:("0"*64)}')
        trust=()
        if [[ -n $ca ]]; then trust=(--cacert "$ca"); fi
        denied=$(printf 'header = "Authorization: Bearer %s"\n' "$AEP_CONTROL_OPERATOR_TOKEN" |
            curl --config - --silent --show-error --max-time 10 "${trust[@]}" --header 'Content-Type: application/json' \
                --data "$payload" --output "$evidence/stale-denied.json" --write-out '%{http_code}' "$endpoint/v1/control")
        [[ $denied == 409 ]] || fail "The interrupted Run was not rejected for publication"
        jq -e '.error | contains("stale Run")' "$evidence/stale-denied.json" >/dev/null
    fi
    control "${retry[@]}" > "$evidence/retry.json"
    recovered_run=$(jq -er --arg predecessor "$old_run" '.data|select(.predecessor==$predecessor)|.id' "$evidence/retry.json")
    step "$recovery_stage"
    wait_run "$recovered_run" "$recovery_stage"
    collect "$recovered_run"
    [[ -z $previous_candidate || $candidate == "$previous_candidate" ]] || fail "Recovery changed the pinned candidate"
else
    recovered_run=$old_run
fi
last="$evidence/worker/runs/$recovered_run"
if [[ $(jq -r .stage "$evidence/run-$recovered_run.json") != awaiting_approval ]]; then
    if [[ -f $last/evidence.json ]]; then jq -r '.error // empty' "$last/evidence.json" >&2; fi
    if [[ -f $evidence/transfer/$recovered_run/supervisor/worker-error.txt ]]; then
        cat "$evidence/transfer/$recovered_run/supervisor/worker-error.txt" >&2
    fi
    fail "The remote Run did not pass acceptance and review"
fi
jq -e --arg sha "$candidate" --arg spec "$spec" --arg task "$task" '
    .candidate_sha==$sha and .spec_sha==$spec and .task==$task and .check_exit_code==0 and
    .review_status=="findings_recorded" and .blocking_findings==[]
' "$last/evidence.json" >/dev/null || fail "The evidence does not match the passing Run"
[[ $(git -C "$operator" rev-parse "refs/aep/runs/$recovered_run") == "$candidate" ]] ||
    fail "The transferred candidate differs from the passing Run"
git -C "$operator" diff --check "$spec" "$candidate"
git -C "$operator" diff --no-ext-diff "$spec" "$candidate" > "$evidence/candidate.diff"

step publish_confirmation
confirm "publish $candidate"
step publication
scope=(--ticket "$ticket" --run "$recovered_run" --spec "$spec" --candidate "$candidate")
control approve "${scope[@]}" > "$evidence/approval.json"
control publish "${scope[@]}" > "$evidence/publication.json"
pr=$(jq -er --arg candidate "$candidate" --arg actor "$actor" '.data|select(.stage=="published" and .approval.candidate==$candidate and .approval.actor==$actor)|.pull_request' "$evidence/publication.json")
[[ $pr =~ ^[0-9]+$ ]] || fail "Missing GitHub PR receipt"
head_branch="aep/pr/$task/$recovered_run"
step ci
wait_ci
step merge_confirmation
confirm "merge $pr $candidate"
pr_view > "$evidence/github-before-merge.json"
check_pr "$evidence/github-before-merge.json"
ready_rollup "$evidence/github-before-merge.json" || fail "GitHub checks or review are no longer ready"
[[ $(approval_context "$evidence/github-ci.json") == "$(approval_context "$evidence/github-before-merge.json")" ]] ||
    fail "GitHub changed during merge confirmation; reconcile before merging"
step merge
gh pr merge "$pr" -R "$repository" --merge --match-head-commit "$candidate"
step cleanup
sync_branch_refs
jq -n --arg sha "$merge_sha" --arg branch "$branch" \
    '{merge_sha:$sha,target:$branch,target_synced:true,candidate_branch_removed:true}' > "$evidence/cleanup.json"
stage=completed
