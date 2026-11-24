#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
repo=$(cd -- "$script_dir/../.." && pwd -P)
codex_auth=${CODEX_HOME:-$HOME/.codex}/auth.json
claude_auth=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json
model=gpt-6.1-sol
output=''
studio_session=''
while (( $# )); do
    case $1 in
        --help|-h)
            cat <<'USAGE'
Usage: deploy/pilot/live.sh [--output-dir DIRECTORY] [--codex-model MODEL]
                            [--codex-auth FILE] [--claude-auth FILE]

Run the real Codex, Claude and GitHub HTTPS pilot on a fresh test branch.
Run in Studio or in a terminal for the publication and merge confirmations.
USAGE
            exit 0 ;;
        --output-dir|--codex-model|--codex-auth|--claude-auth|--studio-session)
            [[ $# -ge 2 && -n $2 && $2 != --* ]] || { echo "$1 requires a value" >&2; exit 2; }
            case $1 in
                --output-dir) output=$2 ;;
                --codex-model) model=$2 ;;
                --codex-auth) codex_auth=$2 ;;
                --claude-auth) claude_auth=$2 ;;
                --studio-session) studio_session=$2 ;;
            esac
            shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

stage=preflight
repository='' task='' branch='' candidate='' old_run='' recovered_run='' pr='' failure=''
run_dir='' archive=''

fail() {
    failure=$*
    printf 'HTTPS pilot: %s\n' "$failure" >&2
    exit 1
}

studio_report() {
    [[ -n $studio_session ]] || return 0
    jq -n --arg stage "$stage" --arg repo "$repository" --arg task "$task" --arg branch "$branch" \
        --arg candidate "$candidate" --arg old_run "$old_run" --arg recovered_run "$recovered_run" \
        --arg pr "$pr" --arg archive "$archive" --arg reason "$failure" --argjson exit_code "${1:-null}" \
        '{stage:$stage,repository:$repo,task:$task,branch:$branch,candidate:$candidate,old_run:$old_run,
          recovered_run:$recovered_run,pr:(if $pr == "" then null else ($pr|tonumber) end),
          archive:($archive!=""),reason:$reason,exit_code:$exit_code}' \
        > "$studio_session/state.tmp"
    mv -f "$studio_session/state.tmp" "$studio_session/state.json"
}

step() {
    stage=$1
    studio_report
}

retry_read() {
    local attempt
    for attempt in 1 2 3 4; do
        if "$@"; then return; fi
        if (( attempt < 4 )); then sleep "$((attempt * 3))"; fi
    done
    return 1
}
confirm() {
    local expected=$1 answer
    if [[ -z $studio_session ]]; then
        printf 'Type "%s" to confirm, or press Enter to stop: ' "$expected" >&2
    else
        # Studio uses the full candidate SHA over the pipe it owns.
        expected="${expected% *} $candidate"
    fi
    if ! IFS= read -r answer <&3 || [[ $answer != "$expected" ]]; then
        printf 'Stopped at the operator gate. Evidence: %s\n' "$evidence" >&2
        exit 3
    fi
    printf '%s\t%s\n' "$(date -u +%FT%TZ)" "$expected" >> "$evidence/confirmations.tsv"
}

if [[ -n $studio_session ]]; then
    [[ -z $output && -d $studio_session && -p /dev/stdin ]] ||
        fail "Studio requires a private session directory and a dedicated approval pipe"
    studio_session=$(cd -- "$studio_session" && pwd -P)
    output=$studio_session
    exec 3<&0 </dev/null
else
    if ! { exec 3</dev/tty; } 2>/dev/null; then fail "A terminal is required for the publication and merge approvals"; fi
fi
for tool in cargo curl docker git gh jq openssl tar; do
    command -v "$tool" >/dev/null || fail "Install $tool"
done
trap 'studio_report $?' EXIT
studio_report
test -r "$codex_auth" || fail "Codex authentication file is missing. Sign in or pass --codex-auth."
if [[ ! -r $claude_auth ]]; then
    if [[ $claude_auth != "$HOME/.claude/.credentials.json" || $(uname -s) != Darwin ]]; then
        fail "Claude authentication file is missing. Sign in or pass --claude-auth."
    fi
    security find-generic-password -s 'Claude Code-credentials' >/dev/null 2>&1 ||
        fail "Claude Keychain login is missing. Sign in or pass --claude-auth."
    claude_auth=''
fi

docker info >/dev/null || fail "Start Docker"
origin=$(git -C "$repo" remote get-url origin)
git_name=$(git -C "$repo" config --get user.name)
git_email=$(git -C "$repo" config --get user.email)
repository_info=$(retry_read gh repo view "$origin" --json nameWithOwner,isPrivate,defaultBranchRef)
repository=$(jq -er 'select(.isPrivate == false) | .nameWithOwner' <<<"$repository_info") ||
    fail "The isolated Worker needs a public GitHub repository"
default=$(jq -er .defaultBranchRef.name <<<"$repository_info")
actor=$(retry_read gh api user --jq .login)

id=$(openssl rand -hex 8)
site=aep-https-$id
task=https-drill-$id
branch=test/$task
lease=20
if [[ -z $output ]]; then
    output=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)/aep/https-drills
fi
mkdir -p "$output"
output=$(cd -- "$output" && pwd -P)
run_dir="$output/$task"
mkdir "$run_dir"
evidence="$run_dir/evidence"
mkdir "$evidence"
operator="$run_dir/operator"
private=$(mktemp -d "${TMPDIR:-/tmp}/aep-https-private.XXXXXX")
export AEP_PILOT_NAME=$site AEP_PILOT_PORT=0 AEP_PILOT_LEASE_SECONDS=$lease
export AEP_PILOT_CONTROL_IMAGE="aep-pilot-control:$id"
export AEP_PILOT_WORKER_IMAGE="aep-pilot-worker:$id"
export AEP_PILOT_EGRESS_IMAGE="aep-pilot-egress:$id"
export NO_PROXY="127.0.0.1${NO_PROXY:+,$NO_PROXY}"
unset AEP_CONTROL_DEVICE_TOKEN AEP_CONTROL_OPERATOR_TOKEN
baseline='' spec='' site_created=''

finish() {
    local status=$1 ids=()
    trap - EXIT
    set +e
    if [[ -n $site_created ]]; then
        if docker container inspect "$site-worker" >/dev/null 2>&1; then
            docker stop -t 10 "$site-worker" >> "$run_dir/cleanup.log" 2>&1
        fi
        if (( status != 0 )) && [[ ! -f $evidence/worker/runs.bundle && -n $old_run ]]; then
            ids+=("$old_run")
            if [[ -n $recovered_run ]]; then ids+=("$recovered_run"); fi
            "$script_dir/site.sh" export "$evidence/partial-worker" --partial "${ids[@]}" >> "$run_dir/cleanup.log" 2>&1
        fi
        "$script_dir/site.sh" remove >> "$run_dir/cleanup.log" 2>&1
        local removed=$?
        if (( removed != 0 )); then
            printf 'Could not remove every trial Docker resource. See %s/cleanup.log\n' "$run_dir" >&2
            (( status != 0 )) || status=1
        fi
    fi
    for image in "$AEP_PILOT_CONTROL_IMAGE" "$AEP_PILOT_WORKER_IMAGE" "$AEP_PILOT_EGRESS_IMAGE"; do
        if docker image inspect "$image" >/dev/null 2>&1; then
            docker image rm "$image" >> "$run_dir/cleanup.log" 2>&1
        fi
    done
    rm -f "$private/ca.key" "$private/ca.pem" "$private/ca.srl" "$private/server.csr" "$private/server.key" \
        "$private/server.pem" "$private/site-ca.pem" "$private/operator" "$private/device"
    rmdir "$private"
    jq -n --argjson exit_code "$status" --arg stage "$stage" --arg repo "$repository" --arg branch "$branch" \
        --arg candidate "$candidate" --arg old_run "$old_run" --arg recovered_run "$recovered_run" --arg pr "$pr" \
        '{exit_code:$exit_code,stage:$stage,repository:$repo,branch:$branch,candidate:$candidate,old_run:$old_run,recovered_run:$recovered_run,pr:$pr}' > "$evidence/result.json"
    if ! tar -czf "$run_dir/evidence.tar.gz" -C "$run_dir" evidence ||
        ! gzip -t "$run_dir/evidence.tar.gz"; then
        failure="Could not package evidence"
        printf 'Could not package evidence at %s\n' "$run_dir" >&2
        (( status != 0 )) || status=1
    else
        archive=$run_dir/evidence.tar.gz
    fi
    studio_report "$status"
    printf 'Evidence: %s/evidence.tar.gz\n' "$run_dir"
    exit "$status"
}
trap 'finish $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

step preparation
printf 'Trial %s starts on %s. Evidence: %s\n' "$task" "$branch" "$evidence"
if ! "$script_dir/build.sh" > "$run_dir/build.log" 2>&1; then tail -50 "$run_dir/build.log" >&2; fail "Pilot images could not be built"; fi
cargo build --quiet --locked --manifest-path "$repo/Cargo.toml" --target-dir "$repo/target" --bin aep
aep="$repo/target/debug/aep"

git clone -q --no-tags --single-branch --branch "$default" "$origin" "$operator"
git -C "$operator" config user.name "$git_name"
git -C "$operator" config user.email "$git_email"
baseline=$(git -C "$operator" rev-parse HEAD)
git -C "$operator" switch -qc "$branch" "$baseline"
spec_path=specs/$task
fixture="$repo/deploy/pilot/fixture"
mkdir -p "$operator/$spec_path"
sed "s/{task}/$task/g" "$script_dir/live-spec.md" > "$operator/$spec_path/spec.md"
cp "$fixture/normalize.rs" "$fixture/acceptance.rs" "$operator/$spec_path/"
check='if test -f /etc/aep-pilot-container; then while test -f /pilot/drill/hold-checks; do sleep 1; done; fi; binary=$(mktemp); trap "rm -f -- \"$binary\"" EXIT; rustc --edition=2024 --test "$1/acceptance.rs" -o "$binary"; "$binary"'
jq -n --arg path "$spec_path" --arg command "$check" '
    {title:"Convert pilot fixture seconds to milliseconds",allowed_paths:[($path+"/normalize.rs")],
     check:["sh","-ec",$command,"pilot-acceptance",$path],criteria:["AC-1","AC-2"]}
' > "$operator/$spec_path/task.json"
git -C "$operator" add -- "$spec_path"
git -C "$operator" commit -qm "Pin HTTPS pilot fixture $task"
spec=$(git -C "$operator" rev-parse HEAD)
cp -R "$operator/$spec_path" "$evidence/spec"
target_ref="refs/heads/$branch"
dispatch_ref="refs/heads/aep-task/$task"
git -C "$operator" push --atomic --no-follow-tags \
    --force-with-lease="$target_ref:" --force-with-lease="$dispatch_ref:" \
    origin "$baseline:$target_ref" "$spec:$dispatch_ref"
git -C "$operator" switch -q --detach "$baseline"
git -C "$operator" branch -f "$branch" "$baseline"
git -C "$operator" switch -q "$branch"
jq -n --arg repo "$repository" --arg actor "$actor" --arg task "$task" --arg target "$branch" \
    --arg baseline "$baseline" --arg spec "$spec" --argjson lease "$lease" \
    '{repository:$repo,actor:$actor,task:$task,target:$target,baseline:$baseline,spec:$spec,lease_seconds:$lease}' > "$evidence/plan.json"

if "$aep" --repo "$operator" verify --task "$task" --base "$spec" --candidate "$spec" --run-id baseline > "$evidence/baseline.log" 2>&1; then
    fail "The unimplemented fixture unexpectedly passes acceptance"
fi
baseline_dir="$operator/.git/aep/runs/baseline"
jq -e '.status=="failed" and .check_exit_code==101' "$baseline_dir/evidence.json" >/dev/null
cp "$baseline_dir/evidence.json" "$evidence/baseline.json"
cp "$baseline_dir/checks.log" "$evidence/baseline-checks.log"

step https
site_created=yes
"$script_dir/site.sh" serve "$private"
endpoint="https://$(docker port "$site-control" 8443/tcp)"
export AEP_CONTROL_OPERATOR_TOKEN=$(cat "$private/operator")
control() { "$aep" --repo "$operator" control --url "$endpoint" --ca-cert "$private/ca.pem" "$@"; }
wait_https() {
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if control status > "$1" 2>/dev/null; then return; fi
        sleep 1
    done
    fail "The HTTPS control plane is unavailable"
}
control_http() {
    local token=$1 operation=$2 response=$3
    printf 'header = "Authorization: Bearer %s"\n' "$token" |
        curl --config - --silent --show-error --noproxy '*' --max-time 6 --cacert "$private/ca.pem" \
            --header 'Content-Type: application/json' --data "$operation" --output "$response" --write-out '%{http_code}' "$endpoint/v1/control"
}

wait_https "$evidence/tls-status.json"
if "$aep" control --url "$endpoint" status > "$evidence/untrusted-ca.log" 2>&1; then fail "HTTPS accepted the private site without its CA"; fi
[[ $(control_http "$(cat "$private/device")" '{"action":"status"}' "$evidence/device-denied.json") == 401 ]] ||
    fail "The device credential could read operator state"
openssl x509 -in "$private/server.pem" -noout -subject -issuer -dates -fingerprint -sha256 > "$evidence/certificate.txt"
for attempt in 1 2 3 4; do
    if "$script_dir/site.sh" workspace "https://github.com/$repository.git" "$branch" "$git_name" "$git_email" >> "$evidence/workspace.log" 2>&1; then
        break
    fi
    if (( attempt == 4 )); then
        cat "$evidence/workspace.log" >&2
        fail "The Worker could not clone the isolated branch through its egress proxy"
    fi
    sleep "$((attempt * 3))"
done
"$script_dir/site.sh" credential codex < "$codex_auth"
if [[ -n $claude_auth ]]; then
    "$script_dir/site.sh" credential claude < "$claude_auth"
else
    security find-generic-password -s 'Claude Code-credentials' -w | "$script_dir/site.sh" credential claude
fi

step candidate
dispatch=(submit --ticket "$task" --task "$task" --spec "$spec" --target "$branch" --baseline "$baseline" --max-attempts 2 --timeout 600)
retry_read control "${dispatch[@]}" > "$evidence/submission.json"
old_run=$(jq -er '.data | select(.stage=="queued") | .id' "$evidence/submission.json")
studio_report
retry_read control "${dispatch[@]}" > "$evidence/duplicate.json"
cmp "$evidence/submission.json" "$evidence/duplicate.json"
"$script_dir/site.sh" gate hold
"$script_dir/site.sh" worker --once --codex-model "$model" > "$evidence/worker-first.log" 2>&1 &
worker=$!
deadline=$((SECONDS + 660))
while (( SECONDS < deadline )); do
    if ! kill -0 "$worker" 2>/dev/null; then
        wait "$worker" || true
        control status > "$evidence/first-worker-exited.json"
        fail "The first Worker exited before independent verification"
    fi
    control status > "$evidence/before-disconnect.json"
    candidate=$(jq -r --arg run "$old_run" '.data.attempts[] | select(.id==$run and .stage=="running") | .events[-1].progress | select(.phase=="checking") | .candidate // empty' "$evidence/before-disconnect.json")
    [[ -z $candidate ]] || break
    sleep 2
done
[[ $candidate =~ ^[a-f0-9]{40}$ ]] || fail "No candidate reached the gated acceptance phase"
docker exec --user 10001:10001 --workdir /pilot/repo "$site-worker" /bin/sh -ec '
    test "$(id -u)" = 10001
    test ! -r /run/aep-worker/device && test ! -r /proc/1/environ && test ! -w /etc
    test ! -e /var/run/docker.sock
    grep -Eq "^CapEff:[[:space:]]+0+$" /proc/self/status
    grep -Eq "^NoNewPrivs:[[:space:]]+1$" /proc/self/status
    git -c credential.helper= ls-remote --exit-code origin HEAD >/dev/null
    if git -c credential.helper= push --dry-run origin "HEAD:refs/heads/$1" >/dev/null 2>&1; then exit 1; fi
    denied=$(curl --silent --proxy "$HTTPS_PROXY" --max-time 6 --output /dev/null --write-out "%{http_connect}" https://example.com 2>/dev/null || true)
    test "$denied" = 403
    if curl --silent --noproxy "*" --max-time 3 https://192.0.2.1 >/dev/null 2>&1; then exit 1; fi
    printf "{\"worker_uid\":10001,\"device_token_readable\":false,\"anonymous_github_fetch\":true,\"github_push\":false,\"unapproved_connect\":403,\"direct_egress\":false}\n"
' sandbox "test/$task-read-only" > "$evidence/sandbox.json"
printf 'Candidate %s is pinned. Disconnecting the HTTPS ingress.\n' "$candidate"

step outage
docker stop -t 3 "$site-https" >/dev/null
if curl --silent --noproxy '*' --max-time 3 --cacert "$private/ca.pem" "$endpoint/v1/control" >/dev/null 2>&1; then
    fail "The HTTPS ingress remained reachable during the outage"
fi
sleep "$((lease + 3))"
docker start "$site-https" >/dev/null
wait_https "$evidence/expired.json"
jq -e --arg run "$old_run" '.data.attempts[] | select(.id==$run) | .stage=="interrupted"' "$evidence/expired.json" >/dev/null
step reconnect
stale=$(jq -nc --arg ticket "$task" --arg run "$old_run" --arg spec "$spec" --arg candidate "$candidate" '
    {action:"begin_publication",ticket:$ticket,run:$run,spec:$spec,candidate:$candidate,report_sha256:("0"*64)}
')
[[ $(control_http "$AEP_CONTROL_OPERATOR_TOKEN" "$stale" "$evidence/stale-denied.json") == 409 ]] ||
    fail "The expired Run was not rejected"
jq -e '.error|contains("stale Run")' "$evidence/stale-denied.json" >/dev/null
wait "$worker"
control status > "$evidence/reconnected.json"
jq -e --arg run "$old_run" --arg candidate "$candidate" '
    .data.attempts[] | select(.id==$run) | .stage=="interrupted" and
    (.events | (map(.sequence)==[range(1;length+1)]) and
        .[-1].progress.type=="exit" and .[-1].progress.outcome=="cancelled" and .[-1].progress.candidate==$candidate)
' "$evidence/reconnected.json" >/dev/null || fail "The Worker did not stop and replay the stale Run"
"$script_dir/site.sh" gate release

control retry --ticket "$task" --reverify > "$evidence/retry.json"
recovered_run=$(jq -er --arg previous "$old_run" '.data | select(.predecessor==$previous and .reverify) | .id' "$evidence/retry.json")
step recovery
printf 'Reverifying %s in Run %s with Claude.\n' "$candidate" "$recovered_run"
"$script_dir/site.sh" worker --once --codex-model "$model" > "$evidence/worker-recovered.log" 2>&1
control status > "$evidence/recovered.json"
jq -e --arg run "$recovered_run" --arg candidate "$candidate" '
    .data.attempts[] | select(.id==$run) | .stage=="awaiting_approval" and (.events[-1].progress | .outcome=="passed" and .candidate==$candidate)
' "$evidence/recovered.json" >/dev/null || fail "Recovery did not verify the exact candidate"
"$script_dir/site.sh" export "$evidence/worker" "$old_run" "$recovered_run"
first="$evidence/worker/runs/$old_run"
last="$evidence/worker/runs/$recovered_run"
jq -e --arg candidate "$candidate" '.candidate_sha==$candidate and .status=="cancelled" and .phase=="checking" and .usage.implementer.output_tokens>0' "$first/evidence.json" >/dev/null
jq -e --arg candidate "$candidate" --arg spec "$spec" '
    .candidate_sha==$candidate and .spec_sha==$spec and .check_exit_code==0 and .implementation_source=="candidate" and
    .review_status=="findings_recorded" and .blocking_findings==[] and .usage.reviewer.output_tokens>0 and
    any(.acceptance[]; .status=="passed" and .criteria==["AC-1","AC-2"])
' "$last/evidence.json" >/dev/null
test -s "$evidence/worker/memory.sqlite3"
git -C "$operator" fetch -q "$evidence/worker/runs.bundle" \
    "refs/aep/runs/$old_run:refs/aep/runs/$old_run" "refs/aep/runs/$recovered_run:refs/aep/runs/$recovered_run"
cp -R "$evidence/worker/runs/." "$operator/.git/aep/runs/"
[[ $(git -C "$operator" rev-parse "refs/aep/runs/$old_run") == "$candidate" ]]
[[ $(git -C "$operator" rev-parse "refs/aep/runs/$recovered_run") == "$candidate" ]]
git -C "$operator" diff --check "$spec" "$candidate"
git -C "$operator" diff --no-ext-diff "$spec" "$candidate" > "$evidence/candidate.diff"

step publish_confirmation
printf 'Review %s/candidate.diff, %s/checks.log and %s/review.txt\n' "$evidence" "$last" "$last"
confirm "publish ${candidate:0:12}"
step publication
scope=(--ticket "$task" --run "$recovered_run" --spec "$spec" --candidate "$candidate")
control approve "${scope[@]}" > "$evidence/approval.json"
control publish "${scope[@]}" > "$evidence/publication.json"
pr=$(jq -er --arg candidate "$candidate" --arg actor "$actor" '.data | select(.stage=="published" and .approval.candidate==$candidate and .approval.actor==$actor) | .pull_request' "$evidence/publication.json")
[[ $pr =~ ^[0-9]+$ ]] || fail "No GitHub PR receipt"
head_branch="aep/pr/$task/$recovered_run"
printf 'Published https://github.com/%s/pull/%s\n' "$repository" "$pr"

# shellcheck source=live-github.sh
source "$script_dir/live-github.sh"
step ci
wait_ci
step merge_confirmation
printf 'Review GitHub CI in %s/github-ci.json and the PR at https://github.com/%s/pull/%s\n' "$evidence" "$repository" "$pr"
confirm "merge $pr ${candidate:0:12}"
pr_view > "$evidence/github-before-merge.json"
check_pr "$evidence/github-before-merge.json"
ready_rollup "$evidence/github-before-merge.json" || fail "GitHub checks or review are no longer ready"
[[ $(approval_context "$evidence/github-ci.json") == "$(approval_context "$evidence/github-before-merge.json")" ]] ||
    fail "The candidate, reviews or CI changed during manual confirmation"
step merge
gh pr merge "$pr" -R "$repository" --merge --match-head-commit "$candidate"
step cleanup
sync_trial_branch
stage=completed
printf 'HTTPS pilot passed and merged to %s.\n' "$branch"
