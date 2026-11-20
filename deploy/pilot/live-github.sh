# Sourced by live.sh. All GitHub mutations are scoped to the run's exact PR and refs.

pr_view() {
    retry_read gh pr view "$pr" -R "$repository" --json \
        number,url,state,headRefName,headRefOid,baseRefName,baseRefOid,mergeable,mergeStateStatus,statusCheckRollup,reviewDecision,latestReviews,mergeCommit,mergedBy
}

check_pr() {
    jq -e --arg candidate "$candidate" --arg branch "$branch" --arg baseline "$baseline" --arg head "$head_branch" \
        '.state == "OPEN" and .headRefOid == $candidate and .headRefName == $head and .baseRefName == $branch and .baseRefOid == $baseline' "$1" >/dev/null ||
        fail "The PR closed or its candidate, target, or baseline changed"
}

ready_rollup() {
    jq -e '
        .mergeable == "MERGEABLE" and .mergeStateStatus == "CLEAN" and
        (.reviewDecision != "CHANGES_REQUESTED") and
        (.statusCheckRollup | type == "array" and length > 0 and
            all(.[]; if .__typename == "CheckRun" then
                .status == "COMPLETED" and ((.conclusion == "SUCCESS") or (.conclusion == "SKIPPED") or (.conclusion == "NEUTRAL"))
            elif .__typename == "StatusContext" then .state == "SUCCESS"
            else false end) and
            any(.[]; .name == "acceptance" and .conclusion == "SUCCESS"))
    ' "$1" >/dev/null
}

failed_rollup() {
    jq -e '
        any(.statusCheckRollup[]?;
            if .__typename == "CheckRun" then
                .status == "COMPLETED" and (.conclusion | IN("SUCCESS", "SKIPPED", "NEUTRAL") | not)
            elif .__typename == "StatusContext" then .state | IN("ERROR", "FAILURE")
            else false end)
    ' "$1" >/dev/null
}

actions_prefix() {
    printf 'https://github.com/%s/actions/runs/' "$repository" | tr '[:upper:]' '[:lower:]'
}

verify_workflow() {
    local acceptance run_id job_id prefix
    prefix=$(actions_prefix)
    acceptance=$(jq -er '[.statusCheckRollup[] | select(.name == "acceptance" and .conclusion == "SUCCESS") | .detailsUrl | ascii_downcase] | if length == 1 then .[0] else empty end' "$1")
    [[ $acceptance == "$prefix"* ]] || fail "Unexpected acceptance job URL"
    acceptance=${acceptance#"$prefix"}
    [[ $acceptance =~ ^([0-9]+)/job/([0-9]+)$ ]] || fail "Expected an Actions workflow and job ID"
    run_id=${BASH_REMATCH[1]}
    job_id=${BASH_REMATCH[2]}
    retry_read gh api "repos/$repository/actions/runs/$run_id" > "$evidence/github-workflow.json"
    jq -e --arg candidate "$candidate" --arg head "$head_branch" --argjson pr "$pr" '
        (.path | split("@")[0]) == ".github/workflows/pilot.yml" and
        .head_sha == $candidate and .head_branch == $head and .event == "pull_request" and
        .status == "completed" and .conclusion == "success" and
        (.pull_requests | length == 0 or any(.[]; .number == $pr and .head.sha == $candidate))
    ' "$evidence/github-workflow.json" >/dev/null || fail "The successful acceptance run is not for the pinned PR"
    retry_read gh run view "$run_id" -R "$repository" --json jobs > "$evidence/github-jobs.json"
    jq -e --argjson job "$job_id" '
        .jobs as $jobs |
        all(["protocol","desktop","acceptance"][]; . as $name | any($jobs[]; .name == $name and .conclusion == "success")) and
        any($jobs[]; .databaseId == $job and .name == "acceptance" and .conclusion == "success")
    ' "$evidence/github-jobs.json" >/dev/null || fail "The PR workflow did not pass each required job"
    retry_read gh run view "$run_id" -R "$repository" --job "$job_id" --log > "$evidence/github-acceptance.log"
    grep -Fq "Checks passed at $candidate." "$evidence/github-acceptance.log" || fail "The independent acceptance log does not confirm the candidate"
}

retry_ci() {
    local snapshot=$1 check run seen=' ' accepted=no
    local prefix
    prefix=$(actions_prefix)
    while IFS= read -r check; do
        [[ $check == "$prefix"* ]] || continue
        check=${check#"$prefix"}
        [[ $check =~ ^([0-9]+)/job/[0-9]+$ ]] || continue
        run=${BASH_REMATCH[1]}
        [[ $seen != *" $run "* ]] || continue
        seen+="$run "
        retry_read gh api "repos/$repository/actions/runs/$run" > "$evidence/github-retry-$run.json"
        if ! jq -e --arg head "$head_branch" --arg candidate "$candidate" --argjson pr "$pr" '
            (.path | split("@")[0]) == ".github/workflows/pilot.yml" and
            .head_sha==$candidate and .head_branch==$head and
            (.event=="push" or (.event=="pull_request" and any(.pull_requests[]; .number==$pr and .head.sha==$candidate)))
        ' "$evidence/github-retry-$run.json" >/dev/null; then
            continue
        fi
        if jq -e '.status=="completed" and .conclusion!="success"' "$evidence/github-retry-$run.json" >/dev/null; then
            checkpoint
            gh run rerun "$run" -R "$repository" --failed || fail "GitHub refused to rerun the failed workflow"
        fi
        accepted=yes
    done < <(jq -r '.statusCheckRollup[] | select(.__typename=="CheckRun" and .status=="COMPLETED" and (.conclusion | IN("SUCCESS","SKIPPED","NEUTRAL") | not)) | .detailsUrl // "" | ascii_downcase' "$snapshot")
    [[ $accepted == yes ]] || fail "The failed checks cannot be rerun for this pinned PR"
}

wait_ci() {
    local deadline=$((SECONDS + 2700)) retry_grace=$((SECONDS + ${1:-0})) snapshot pending previous=''
    snapshot="$evidence/github-ci.json"
    while (( SECONDS < deadline )); do
        if declare -F checkpoint >/dev/null; then checkpoint; fi
        pr_view > "$snapshot.tmp"
        mv -f "$snapshot.tmp" "$snapshot"
        check_pr "$snapshot"
        if (( SECONDS >= retry_grace )) && failed_rollup "$snapshot"; then
            jq -r '.statusCheckRollup[] | [.name // .context, .conclusion // .state, .detailsUrl // .targetUrl] | @tsv' "$snapshot" >&2
            fail "GitHub CI failed"
        fi
        pending=$(jq -r '[.statusCheckRollup[] | "\(.name // .context) \(if .conclusion == "" then .status else .conclusion // .state end)"] | join(", ")' "$snapshot")
        if [[ $pending != "$previous" ]]; then
            printf 'GitHub CI: %s\n' "$pending"
            previous=$pending
        fi
        if ready_rollup "$snapshot"; then
            verify_workflow "$snapshot"
            return
        fi
        sleep 15
    done
    fail "Timed out waiting for GitHub CI"
}

approval_context() {
    jq -cS '{headRefOid,baseRefOid,reviewDecision,latestReviews,statusCheckRollup: (.statusCheckRollup | sort_by(.detailsUrl // .targetUrl // .name // .context))}' "$1"
}

sync_branch_refs() {
    local merged remote
    pr_view > "$evidence/github-merged.json"
    jq -e --arg candidate "$candidate" --arg target "$branch" --arg head "$head_branch" '
        .state == "MERGED" and .headRefOid == $candidate and .baseRefName == $target and .headRefName == $head and (.mergeCommit.oid | type == "string")
    ' "$evidence/github-merged.json" >/dev/null || fail "GitHub merge receipt disagrees with the confirmed candidate"
    merged=$(jq -r .mergeCommit.oid "$evidence/github-merged.json")
    git -C "$operator" fetch -q origin "refs/heads/$branch"
    git -C "$operator" merge -q --ff-only FETCH_HEAD
    [[ $(git -C "$operator" rev-parse HEAD) == "$merged" ]] || fail "The synced target differs from the merge receipt"
    git -C "$operator" merge-base --is-ancestor "$candidate" "$merged"
    remote=$(git -C "$operator" ls-remote --refs origin "refs/heads/$head_branch" | cut -f1)
    if [[ -n $remote ]]; then
        [[ $remote == "$candidate" ]] || fail "The remote candidate moved before cleanup"
        git -C "$operator" push --force-with-lease="refs/heads/$head_branch:$candidate" origin ":refs/heads/$head_branch"
    fi
    [[ -z $(git -C "$operator" ls-remote --refs origin "refs/heads/$head_branch") ]]
    merge_sha=$merged
}

sync_trial_branch() {
    local run_path merge_sha
    sync_branch_refs
    run_path=$(jq -er .worktree "$evidence/worker/runs/$recovered_run/evidence.json")
    docker run --rm --network none --read-only --user 10001:10001 \
        --mount "type=volume,src=$site-workspace,dst=/pilot,readonly" \
        --entrypoint /usr/bin/git "$AEP_PILOT_WORKER_IMAGE" -C /pilot/repo worktree list --porcelain > "$evidence/worktrees.txt"
    if grep -Fxq "worktree $run_path" "$evidence/worktrees.txt"; then fail "The completed Worktree is still registered"; fi
    jq -n --arg sha "$merge_sha" --arg branch "$branch" \
        '{merge_sha:$sha,target:$branch,target_synced:true,candidate_branch_removed:true,completed_worktree_removed:true}' > "$evidence/cleanup.json"
}
