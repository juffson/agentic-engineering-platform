#!/usr/bin/env bash
# Invoked by Studio in its private session directory.
set -euo pipefail
umask 077
[[ $# == 5 && -d $1 && -x $2 && -d $3 && -p /dev/stdin && $4 =~ ^[0-9]+$ && $5 =~ ^(observe|retry)$ ]]
session=$1
aep=$2
source_repo=$3
studio=$4
mode=$5
# Studio passes the locked open file as stdout. Keep the lock until the shell
# and any in-flight commands have exited, and send output to the append-only log.
exec 4>&1 >> "$session/studio.log" 2>&1
exec 3<&0 </dev/null
request=$session/request.json
stage=preflight
failure='' repository='' task='' branch='' baseline='' spec='' candidate='' old_run='' recovered_run='' pr=''
evidence='' archive='' worker=''
ticket=''

report() {
    jq -n --arg stage "$stage" --arg reason "$failure" --arg repository "$repository" --arg task "$task" \
        --arg branch "$branch" --arg ticket "$ticket" --arg baseline "$baseline" --arg spec "$spec" \
        --arg candidate "$candidate" --arg old "$old_run" --arg recovered "$recovered_run" --arg pr "$pr" --arg worker "$worker" \
        --arg archive "$archive" --argjson code "${1:-null}" \
        '{remote:true,stage:$stage,reason:$reason,repository:$repository,task:$task,branch:$branch,ticket:$ticket,
          baseline:$baseline,spec:$spec,candidate:$candidate,old_run:$old,recovered_run:$recovered,worker:$worker,
          pr:(if $pr=="" then null else ($pr|tonumber) end),archive:($archive!=""),exit_code:$code}' > "$session/state.tmp"
    mv -f "$session/state.tmp" "$session/state.json"
}

fail() { failure=$*; printf '%s\n' "$failure" >&2; exit 1; }
step() { stage=$1; report; printf '%s\n' "$stage"; }

checkpoint() {
    if [[ -f $session/pause-request ]] || ! kill -0 "$studio" 2>/dev/null; then
        failure="Studio closed. The remote Run and its receipts were retained."
        exit 4
    fi
}
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
        checkpoint
        failure="Stopped at the operator gate"
        printf '%s\n' "$failure"
        exit 3
    fi
    checkpoint
    printf '%s\t%s\n' "$(date -u +%FT%TZ)" "$1" >> "$evidence/confirmations.tsv"
}

for tool in curl git gh jq tar gzip; do
    command -v "$tool" >/dev/null || { echo "Install $tool" >&2; exit 1; }
done

prior='{}'
if [[ -s $session/state.json ]]; then prior=$(cat "$session/state.json"); fi
task=$(jq -er .task.name "$request")
branch=$(jq -er .branch.name "$request")
baseline=$(jq -er .branch.sha "$request")
requested_spec=$(jq -er .task.sha "$request")
ticket=$(jq -r '.ticket // empty' "$request")
if [[ -z $ticket ]]; then ticket=$(jq -r '.ticket // empty' <<<"$prior"); fi
[[ $task =~ ^[a-z0-9][a-z0-9-]{0,62}$ && $ticket =~ ^https-task-[a-f0-9]{24,32}$ && $baseline =~ ^[a-f0-9]{40}$ &&
   ( -z $requested_spec || $requested_spec =~ ^[a-f0-9]{40}$ ) ]] || fail "Invalid persisted task, ticket or baseline"
git check-ref-format "refs/heads/$branch"
operator=$session/$task/operator
evidence=$session/$task/evidence
mkdir -p "$operator" "$evidence/studio-attempts"
[[ ! -f $session/$task/evidence.tar.gz ]] || archive=$session/$task/evidence.tar.gz
if jq -e '.stage != null' <<<"$prior" >/dev/null; then
    receipt=$(mktemp "$evidence/studio-attempts/previous.XXXXXX")
    printf '%s\n' "$prior" > "$receipt"
fi
# Keep the information displayed before a restart, even if preflight fails.
repository=$(jq -r '.repository // empty' <<<"$prior")
spec=$(jq -r '.spec // empty' <<<"$prior")
candidate=$(jq -r '.candidate // empty' <<<"$prior")
old_run=$(jq -r '.old_run // empty' <<<"$prior")
recovered_run=$(jq -r '.recovered_run // empty' <<<"$prior")
pr=$(jq -r '.pr // empty' <<<"$prior")
worker=$(jq -r '.worker // empty' <<<"$prior")
step remote_preflight
checkpoint

endpoint=$(jq -er .endpoint "$request")
ca=$(jq -r '.ca // empty' "$request")
token_file=$(jq -er .operator_file "$request")
[[ $endpoint == https://* && -r $token_file ]] || fail "Invalid HTTPS origin or operator token file"
opts=(--url "$endpoint")
if [[ -n $ca ]]; then
    [[ -r $ca ]] || fail "Cannot read the control CA file"
    opts+=(--ca-cert "$ca")
fi
export AEP_CONTROL_OPERATOR_TOKEN=$(cat "$token_file")
unset AEP_CONTROL_DEVICE_TOKEN
export GIT_TERMINAL_PROMPT=0
control() { "$aep" --repo "$operator" control "${opts[@]}" "$@"; }
source "$session/live-github.sh"

origin=$(git -C "$source_repo" remote get-url origin)
repository=$(retry_read gh repo view "$origin" --json nameWithOwner,isPrivate --jq 'select(.isPrivate==false)|.nameWithOwner')
[[ -n $repository ]] || fail "Use a public GitHub repository accessible by the remote Worker"
actor=$(retry_read gh api user --jq .login)
git -C "$operator" init --quiet --template=
if ! git -C "$operator" remote get-url origin >/dev/null 2>&1; then
    git -C "$operator" remote add origin "$origin"
fi
[[ $(git -C "$operator" remote get-url origin) == "$origin" ]] || fail "The source repository remote changed"
target_ref=refs/heads/$branch
spec_ref=refs/heads/aep-task/$task
pinned_ref=refs/aep/studio/spec

status_snapshot() {
    jq -e --arg repository "$repository" '
        (.data.attempts | all(.[]; .task.repository==$repository)) and
        (.data.device.installation | type=="string" and length>0)
    ' "$1" >/dev/null || fail "The control plane belongs to another repository or has no registered Worker"
    if [[ -s $evidence/control-before.json ]]; then
        [[ $(jq -er .data.device.installation "$1") == "$(jq -er .data.device.installation "$evidence/control-before.json")" ]] ||
            fail "The saved task belongs to another Worker installation"
    fi
}
retry_read control status > "$evidence/status.tmp" || fail "Cannot reach the saved HTTPS control plane; see the Studio log"
status_snapshot "$evidence/status.tmp"
mv -f "$evidence/status.tmp" "$evidence/control-status.json"
if [[ ! -s $evidence/control-before.json ]]; then
    cp "$evidence/control-status.json" "$evidence/control-before.json"
fi

# The remote ticket and Git refs, rather than the last displayed step, decide
# which side effects have already happened.
saved_plan=$evidence/plan.json
if [[ -s $saved_plan ]]; then
    jq -e --arg repo "$repository" --arg actor "$actor" --arg ticket "$ticket" --arg task "$task" \
        --arg branch "$branch" --arg base "$baseline" '
        .repository==$repo and .actor==$actor and .ticket==$ticket and .task==$task and .target==$branch and .baseline==$base
    ' "$saved_plan" >/dev/null || fail "The repository, approver or task changed since dispatch"
    spec=$(jq -er .spec "$saved_plan")
    [[ $spec =~ ^[a-f0-9]{40}$ && ( -z $requested_spec || $spec == "$requested_spec" ) ]] || fail "The saved Spec changed"
else
    refs=("$target_ref:refs/aep/remote/baseline")
    if [[ -n $requested_spec ]]; then refs+=("$spec_ref:refs/aep/remote/spec"); fi
    retry_read git -C "$operator" fetch -q --no-tags origin "${refs[@]}"
    [[ $(git -C "$operator" rev-parse refs/aep/remote/baseline) == "$baseline" ]] ||
        fail "The selected target changed. Refresh the repository branches and select again."
    if ! git -C "$operator" show-ref --verify --quiet "$target_ref"; then
        git -C "$operator" switch -qc "$branch" "$baseline"
    fi
    if [[ -n $requested_spec ]]; then
        spec=$requested_spec
        [[ $(git -C "$operator" rev-parse refs/aep/remote/spec) == "$spec" ]] || fail "The selected Spec changed"
    else
        step remote_create
        for file in spec.md task.json; do
            [[ -s $session/draft/$file ]] || fail "Missing Studio task draft"
        done
        pinned=$(git -C "$operator" rev-parse -q --verify "$pinned_ref" || true)
        if [[ -z $pinned && $spec =~ ^[a-f0-9]{40}$ ]] && git -C "$operator" cat-file -e "$spec^{commit}"; then
            # Sessions created by the older Studio saved the authored commit in state.json.
            pinned=$spec
        fi
        if [[ -z $pinned ]]; then
            specs=$(git -C "$operator" ls-tree "$baseline" -- specs)
            [[ -z $specs || $specs == "040000 tree "* ]] || fail "The selected target must have a regular specs directory"
            if git -C "$operator" cat-file -e "$baseline:specs/$task" 2>/dev/null; then
                fail "The selected target already contains this task directory"
            fi
            for key in user.name user.email; do
                value=$(git -C "$source_repo" config --get "$key") || fail "Configure Git $key in the selected repository"
                git -C "$operator" config --local "$key" "$value"
            done
            export GIT_INDEX_FILE=$session/spec.index
            git -C "$operator" read-tree "$baseline"
            for file in spec.md task.json; do
                blob=$(git -C "$operator" hash-object -w "$session/draft/$file")
                git -C "$operator" update-index --add --cacheinfo "100644,$blob,specs/$task/$file"
            done
            tree=$(git -C "$operator" write-tree)
            unset GIT_INDEX_FILE
            pinned=$(git -C "$operator" -c commit.gpgsign=false commit-tree "$tree" -p "$baseline" -m "Specify $task")
        fi
        spec=$pinned
        [[ $(git -C "$operator" rev-parse "$spec^") == "$baseline" ]] || fail "The authored Spec has a different baseline"
        for file in spec.md task.json; do
            git -C "$operator" show "$spec:specs/$task/$file" | cmp - "$session/draft/$file" ||
                fail "The pinned Spec differs from the saved draft"
        done
        git -C "$operator" update-ref "$pinned_ref" "$spec"
        report
        checkpoint
        retry_read "$aep" --repo "$operator" dispatch --task "$task" --rev "$spec" > "$evidence/spec-dispatch.txt"
        jq -n --arg task "$task" --arg spec "$spec" --arg target "$branch" --arg base "$baseline" \
            --arg ref "$spec_ref" '{task:$task,spec:$spec,target:$target,baseline:$base,dispatch_ref:$ref}' \
            > "$evidence/authoring.json"
    fi
    for file in spec.md task.json; do git -C "$operator" show "$spec:specs/$task/$file" > "$evidence/$file"; done
    jq -n --arg repo "$repository" --arg actor "$actor" --arg ticket "$ticket" --arg task "$task" \
        --arg branch "$branch" --arg baseline "$baseline" --arg spec "$spec" \
        '{repository:$repo,actor:$actor,ticket:$ticket,task:$task,target:$branch,baseline:$baseline,spec:$spec}' > "$saved_plan.tmp"
    mv -f "$saved_plan.tmp" "$saved_plan"
fi

ticket_runs() {
    jq -c --arg ticket "$ticket" '[.data.attempts[] | select(.task.ticket==$ticket)]' "$evidence/control-status.json"
}

runs=$(ticket_runs)
if [[ $(jq 'length' <<<"$runs") == 0 ]]; then
    [[ -z $old_run && ! -s $evidence/submission.json ]] || fail "The saved ticket is missing from the original control plane"
    step remote_dispatch
    checkpoint
    retry_read control submit --ticket "$ticket" --task "$task" --spec "$spec" --target "$branch" \
        --baseline "$baseline" --max-attempts 4 > "$evidence/submission.tmp" ||
        fail "The control plane could not dispatch the pinned task; see the Studio log"
    mv -f "$evidence/submission.tmp" "$evidence/submission.json"
    runs=$(jq -c '[.data]' "$evidence/submission.json")
elif [[ ! -s $evidence/submission.json ]]; then
    jq '{data:.[0]}' <<<"$runs" > "$evidence/submission.json"
fi
jq -e --arg spec "$spec" --arg task "$task" --arg target "$branch" --arg baseline "$baseline" '
    length>0 and all(.[]; .task.spec==$spec and .task.name==$task and .task.target==$target and .task.baseline==$baseline)
' <<<"$runs" >/dev/null || fail "The control plane ticket now pins a different task"
first=$(jq -er '.[0].id' <<<"$runs")
[[ -z $old_run || $old_run == "$first" ]] || fail "The control plane no longer has the original Run"
old_run=$first
recovered_run=$(jq -er '.[-1].id' <<<"$runs")

wait_run() {
    local run=$1 work_stage=$2 deadline=$((SECONDS + 900)) offline=no snapshot active sha phase next assigned
    while (( SECONDS < deadline )); do
        checkpoint
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
        status_snapshot "$evidence/status.tmp"
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
        if [[ $active == queued ]]; then
            next=queue_wait
            deadline=$((SECONDS + 900))
        elif [[ $active == cancel_pending ]]; then
            next=remote_cancelling
            deadline=$((SECONDS + 900))
        elif [[ $active == interrupted ]]; then
            [[ -f $evidence/interrupted-$run.json ]] || cp "$evidence/run-$run.json" "$evidence/interrupted-$run.json"
            next=reconnect
        elif [[ $work_stage != recovery && $active == running ]]; then
            case $phase in
                checking) next=remote_checking ;;
                reviewing) next=remote_reviewing ;;
            esac
        fi
        assigned=$(jq -r '.worker // empty' <<<"$snapshot")
        if [[ $sha != "$candidate" || $worker != "$assigned" ]]; then candidate=$sha; worker=$assigned; report; fi
        if [[ $stage != "$next" ]]; then
            step "$next"
        fi
        if jq -e '.events[-1].progress.type == "exit" or (.stage == "cancelled" and (.events|length)==0)' <<<"$snapshot" >/dev/null; then return; fi
        sleep 2
    done
    fail "Timed out waiting for the remote Run or its terminal event replay"
}

collect() {
    local run=$1 output="$evidence/transfer/$1" deadline=$((SECONDS + 100)) staging dest
    mkdir -p "$evidence/transfer"
    while [[ ! -d $output ]] && (( SECONDS < deadline )); do
        checkpoint
        staging=$(mktemp -d "$session/collect.XXXXXX")
        if control collect --ticket "$ticket" --run "$run" --output "$staging/data" > "$evidence/collect-$run.log" 2>&1; then
            mv "$staging/data" "$output"
        fi
        if [[ ! -e $staging/data ]]; then rmdir "$staging"; fi
        if [[ ! -d $output ]]; then sleep 2; fi
    done
    [[ -d $output ]] || return 1
    if [[ -d $output/run ]]; then
        for dest in "$evidence/worker/runs/$run" "$operator/.git/aep/runs/$run"; do
            mkdir -p "$dest"
            cp -R "$output/run/." "$dest/"
        done
    fi
    if [[ -s $output/memory.sqlite3 ]]; then
        mkdir -p "$evidence/worker"
        cp "$output/memory.sqlite3" "$evidence/worker/memory.sqlite3"
    fi
    if [[ -s $output/candidate.bundle ]]; then
        git -C "$operator" fetch -q "$output/candidate.bundle" "refs/aep/runs/$run:refs/aep/runs/$run"
    elif [[ $(jq -r '.candidate_sha // ""' "$output/run/evidence.json" 2>/dev/null || true) == "$spec" ]]; then
        git -C "$operator" update-ref "refs/aep/runs/$run" "$spec"
    fi
}

# A worker retry is issued at most once per operator request. If the response
# was lost, the next Studio instance discovers the successor on the server.
terminal_at_open=$(jq -er '.[-1].stage' <<<"$runs")
previous_at_open=$(jq -r '.recovered_run // empty' <<<"$prior")
retry_failed=no
if [[ $mode == retry && $recovered_run == "$previous_at_open" ]]; then
    case $terminal_at_open in
        failed|blocked|interrupted) retry_failed=yes ;;
    esac
fi

work_stage=candidate
if [[ $old_run != "$recovered_run" ]]; then
    if jq -e '.[-1].reverify' <<<"$runs" >/dev/null; then work_stage=recovery; else work_stage=remote_retry; fi
fi
step "$work_stage"
while :; do
    wait_run "$recovered_run" "$work_stage"
    run_file=$evidence/run-$recovered_run.json
    terminal=$(jq -er .stage "$run_file")
    automatic=no
    if [[ $terminal == interrupted ]] && jq -e --arg previous "$(jq -r '.predecessor // empty' "$run_file")" '
        all(.data.attempts[]; .id!=$previous or .stage!="interrupted")
    ' "$evidence/control-status.json" >/dev/null; then automatic=yes; fi
    if [[ $automatic == yes || $retry_failed == yes ]]; then
        retry_failed=no
        previous=$recovered_run
        previous_candidate=$candidate
        retry=(retry --ticket "$ticket")
        work_stage=remote_retry
        if [[ $terminal == interrupted && -n $previous_candidate ]]; then
            if ! collect "$previous"; then fail "Worker evidence is unavailable for the interrupted Run"; fi
            [[ $(git -C "$operator" rev-parse "refs/aep/runs/$previous") == "$previous_candidate" ]] ||
                fail "The interrupted Run has no matching candidate evidence"
            retry+=(--reverify)
            work_stage=recovery
            payload=$(jq -nc --arg ticket "$ticket" --arg run "$previous" --arg spec "$spec" --arg head "$previous_candidate" \
                '{action:"begin_publication",ticket:$ticket,run:$run,spec:$spec,candidate:$head,report_sha256:("0"*64)}')
            trust=()
            if [[ -n $ca ]]; then trust=(--cacert "$ca"); fi
            denied=$(printf 'header = "Authorization: Bearer %s"\n' "$AEP_CONTROL_OPERATOR_TOKEN" |
                curl --config - --silent --show-error --max-time 10 "${trust[@]}" --header 'Content-Type: application/json' \
                    --data "$payload" --output "$evidence/stale-denied.json" --write-out '%{http_code}' "$endpoint/v1/control")
            [[ $denied == 409 ]] || fail "The interrupted Run was not rejected for publication"
            jq -e '.error | contains("stale Run")' "$evidence/stale-denied.json" >/dev/null
        elif [[ $terminal != cancelled ]] && ! collect "$previous"; then
            printf '%s\n' "Worker evidence was unavailable for $previous; retrying its pinned Spec."
        fi
        checkpoint
        control "${retry[@]}" > "$evidence/retry.tmp" ||
            fail "The control plane refused this retry; see the Studio log and attempt budget"
        recovered_run=$(jq -er --arg previous "$previous" '.data|select(.predecessor==$previous)|.id' "$evidence/retry.tmp")
        mv -f "$evidence/retry.tmp" "$evidence/retry.json"
        cp "$evidence/retry.json" "$evidence/studio-attempts/retry-$recovered_run.json"
        candidate=''
        step "$work_stage"
        continue
    fi
    break
done

last="$evidence/worker/runs/$recovered_run"
case "$terminal" in
    cancelled)
        # A queued cancellation has no process or Worker packet. An acquired
        # Run needs the recorded exit and uploaded partial evidence.
        cp "$run_file" "$evidence/cancellation.json"
        if jq -e '.events | length > 0' "$run_file" >/dev/null; then
            step remote_cancelling
            collect "$recovered_run" || fail "The Worker stopped, but its cancellation evidence could not be downloaded"
        fi
        stage=cancelled
        exit 0
        ;;
    awaiting_approval|approved|publishing|published)
        collect "$recovered_run" || fail "Worker evidence is unavailable. Check --upload-evidence and the HTTPS route"
        ;;
    *)
        if [[ $terminal != cancelled ]] && collect "$recovered_run"; then
            if [[ -f $last/evidence.json ]]; then jq -r '.error // empty' "$last/evidence.json" >&2; fi
            if [[ -f $evidence/transfer/$recovered_run/supervisor/worker-error.txt ]]; then
                cat "$evidence/transfer/$recovered_run/supervisor/worker-error.txt" >&2
            fi
        fi
        fail "The remote Run ended in $terminal before passing acceptance and review"
        ;;
esac

if jq -e '.reverify' "$run_file" >/dev/null; then
    predecessor=$(jq -er '.predecessor' "$run_file")
    preserved=$(git -C "$operator" rev-parse "refs/aep/runs/$predecessor") ||
        fail "The predecessor candidate is missing from the saved evidence"
    [[ $candidate == "$preserved" ]] || fail "Recovery changed the pinned candidate"
    if [[ -n $worker ]]; then
        predecessor_worker=$(jq -er .worker "$evidence/run-$predecessor.json" 2>/dev/null || jq -er --arg run "$predecessor" '.data.attempts[] | select(.id==$run) | .worker' "$evidence/control-status.json")
        [[ $worker == "$predecessor_worker" ]] || fail "Recovery changed the Worker holding the candidate"
    fi
fi
jq -e --arg sha "$candidate" --arg spec "$spec" --arg task "$task" '
    .candidate_sha==$sha and .spec_sha==$spec and .task==$task and .check_exit_code==0 and
    .review_status=="findings_recorded" and .blocking_findings==[]
' "$last/evidence.json" >/dev/null || fail "The evidence does not match the passing Run"
[[ $(git -C "$operator" rev-parse "refs/aep/runs/$recovered_run") == "$candidate" ]] ||
    fail "The transferred candidate differs from the passing Run"
git -C "$operator" diff --check "$spec" "$candidate"
git -C "$operator" diff --no-ext-diff "$spec" "$candidate" > "$evidence/candidate.diff"

if [[ $terminal == published ]]; then
    jq '{data:.}' "$run_file" > "$evidence/publication.tmp"
else
    pr=''
    checkpoint
    step publish_confirmation
    confirm "publish $candidate"
    step publication
    scope=(--ticket "$ticket" --run "$recovered_run" --spec "$spec" --candidate "$candidate")
    if [[ $terminal == awaiting_approval ]]; then
        control approve "${scope[@]}" > "$evidence/approval.tmp" || fail "The control plane could not record publication approval"
    else
        jq '{data:.}' "$run_file" > "$evidence/approval.tmp"
    fi
    mv -f "$evidence/approval.tmp" "$evidence/approval.json"
    jq -e --arg spec "$spec" --arg candidate "$candidate" --arg actor "$actor" '
        .data.approval | .spec==$spec and .candidate==$candidate and .actor==$actor
    ' "$evidence/approval.json" >/dev/null || fail "Publication approval belongs to a different actor or candidate"
    control publish "${scope[@]}" > "$evidence/publication.tmp" ||
        fail "Publication did not complete. Retry to reconcile the exact candidate and PR"
fi
pr=$(jq -er --arg spec "$spec" --arg candidate "$candidate" --arg actor "$actor" '
    .data | select(.stage=="published" and .approval.spec==$spec and .approval.candidate==$candidate and .approval.actor==$actor) | .pull_request
' "$evidence/publication.tmp")
[[ $pr =~ ^[1-9][0-9]*$ ]] || fail "Missing GitHub PR receipt"
mv -f "$evidence/publication.tmp" "$evidence/publication.json"
head_branch="aep/pr/$task/$recovered_run"
checkpoint
pr_view > "$evidence/github-resume.json"
if [[ $(jq -er .state "$evidence/github-resume.json") != MERGED ]]; then
    check_pr "$evidence/github-resume.json"
    step ci
    if [[ $mode == retry ]] && failed_rollup "$evidence/github-resume.json"; then
        checkpoint
        retry_ci "$evidence/github-resume.json"
        wait_ci 60
    else
        wait_ci
    fi
    step merge_confirmation
    confirm "merge $pr $candidate"
    pr_view > "$evidence/github-before-merge.json"
    check_pr "$evidence/github-before-merge.json"
    ready_rollup "$evidence/github-before-merge.json" || fail "GitHub checks or review are no longer ready"
    [[ $(approval_context "$evidence/github-ci.json") == "$(approval_context "$evidence/github-before-merge.json")" ]] ||
        fail "GitHub changed during merge confirmation; review the new state before merging"
    step merge
    gh pr merge "$pr" -R "$repository" --merge --match-head-commit "$candidate" ||
        fail "GitHub did not confirm the merge. Retry to reconcile the exact PR"
fi
step cleanup
checkpoint
sync_branch_refs
jq -n --arg sha "$merge_sha" --arg branch "$branch" \
    '{merge_sha:$sha,target:$branch,target_synced:true,candidate_branch_removed:true}' > "$evidence/cleanup.json"
stage=completed
