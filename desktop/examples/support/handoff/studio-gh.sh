#!/bin/sh
set -eu
fixture=$(dirname "$0")/..
test "$1" = api
test "$2" = --hostname
test "$3" = github.com
case "$4" in
  graphql)
    head=$(cat "$fixture/pr-head")
    if [ -f "$fixture/merged-head" ]; then
      head_sha=$(cat "$fixture/merged-head")
    else
      head_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse "refs/heads/$head")
    fi
    base_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse refs/heads/main)
    mode=pending
    if [ -f "$fixture/ci-state" ]; then mode=$(cat "$fixture/ci-state"); fi
    check_status=COMPLETED
    conclusion='"SUCCESS"'
    review='"APPROVED"'
    state=OPEN
    merge_commit=null
    merged_at=null
    merged_by=null
    case "$mode" in
      pending) check_status=IN_PROGRESS; conclusion=null ;;
      failed) conclusion='"FAILURE"' ;;
      changes) review='"CHANGES_REQUESTED"' ;;
    esac
    if [ -f "$fixture/merged-sha" ]; then
      state=MERGED
      merge_commit="{\"oid\":\"$(cat "$fixture/merged-sha")\"}"
      merged_at='"2026-11-19T00:00:02Z"'
      merged_by='{"login":"aep-reviewer"}'
    fi
    cat <<JSON
{"data":{"repository":{
  "viewerPermission":"WRITE","mergeCommitAllowed":true,
  "pullRequest":{
    "title":"AEP Studio fixture","state":"$state","isDraft":false,"mergeable":"MERGEABLE",
    "mergeStateStatus":"CLEAN","reviewDecision":$review,
    "headRefOid":"$head_sha","baseRefOid":"$base_sha","baseRefName":"main","headRefName":"$head",
    "mergeCommit":$merge_commit,"mergedAt":$merged_at,"mergedBy":$merged_by,
    "commits":{"nodes":[{"commit":{"oid":"$head_sha","statusCheckRollup":{"contexts":{
      "pageInfo":{"hasNextPage":false},"nodes":[{
        "__typename":"CheckRun","id":"test-check","databaseId":19,"name":"acceptance","status":"$check_status",
        "conclusion":$conclusion,"detailsUrl":"https://github.com/example/aep-fixture/actions/runs/300/job/19",
        "checkSuite":{"workflowRun":{"workflow":{"name":"AEP pilot"}}}
      }]
    }}}}]},
    "reviews":{"totalCount":1,"nodes":[{
      "id":"review1","body":"Please keep the overflow regression test.","state":$review,"submittedAt":"2026-11-19T00:00:01Z",
      "url":"https://github.com/example/aep-fixture/pull/17#pullrequestreview-1","author":{"login":"reviewer"}
    }]},
    "comments":{"totalCount":1,"nodes":[{"id":"comment1","body":"Review fixture comment.","createdAt":"2026-11-19T00:00:00Z","author":{"login":"reviewer"}}]},
    "reviewThreads":{"totalCount":0,"nodes":[]}
  }
}}}
JSON
    ;;
  repos/example/aep-fixture/check-runs/19)
    printf '%s\n' '{"output":{"title":"Acceptance failed","summary":"Check the boundary conversion."}}'
    ;;
  repos/example/aep-fixture/check-runs/19/annotations?per_page=30)
    printf '%s\n' '[{"path":"normalize.rs","start_line":2,"title":"Overflow","message":"Test expected a widened result."}]'
    ;;
  repos/example/aep-fixture/actions/jobs/19/logs)
    printf '%s\n' 'assertion failed: upper bound' 'Process completed with exit code 1.'
    ;;
  repos/example/aep-fixture/pulls/17/merge)
    shift 4
    test "$1" = --method
    test "$2" = PUT
    test "$(cat "$fixture/ci-state")" = passed
    head=$(cat "$fixture/pr-head")
    head_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse "refs/heads/$head")
    test "$4" = "sha=$head_sha"
    test "$6" = merge_method=merge
    base_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse refs/heads/main)
    tree=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse "$head_sha^{tree}")
    merge=$(/usr/bin/git --git-dir="$fixture/remote.git" -c user.name='AEP Fixture' \
      -c user.email=fixture@example.invalid -c commit.gpgsign=false \
      commit-tree "$tree" -p "$base_sha" -p "$head_sha" -m 'Merge smoke candidate')
    /usr/bin/git --git-dir="$fixture/remote.git" update-ref refs/heads/main "$merge" "$base_sha"
    printf 'PUT\n' >> "$fixture/pr-merges"
    printf '%s\n' "$head_sha" > "$fixture/merged-head"
    printf '%s\n' "$merge" > "$fixture/merged-sha"
    printf '{"merged":true,"sha":"%s"}\n' "$merge"
    ;;
  repos/example/aep-fixture)
    printf '%s\n' '{"default_branch":"main"}'
    ;;
  user)
    printf '%s\n' '{"login":"aep-reviewer"}'
    ;;
  repos/example/aep-fixture/pulls)
    shift 4
    test "$1" = --method
    if [ "$2" = GET ]; then
      if [ -f "$fixture/pull.json" ]; then
        printf '['
        cat "$fixture/pull.json"
        printf ']\n'
      else
        printf '[]\n'
      fi
    else
      test "$2" = POST
      printf 'POST\n' >> "$fixture/pr-posts"
      if [ -f "$fixture/bin/fail-pr" ]; then
        echo 'simulated GitHub outage' >&2
        exit 1
      fi
      for arg in "$@"; do
        case "$arg" in
          head=*) head=${arg#head=} ;;
          base=*) base=${arg#base=} ;;
        esac
      done
      head_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse "refs/heads/$head")
      base_sha=$(/usr/bin/git --git-dir="$fixture/remote.git" rev-parse "refs/heads/$base")
      printf '{"number":17,"state":"open","head":{"sha":"%s"},"base":{"ref":"%s","sha":"%s"}}' \
        "$head_sha" "$base" "$base_sha" > "$fixture/pull.json"
      printf '%s\n' "$head" > "$fixture/pr-head"
      cat "$fixture/pull.json"
    fi
    ;;
  *) exit 2 ;;
esac
