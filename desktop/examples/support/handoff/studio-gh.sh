#!/bin/sh
set -eu
fixture=$(dirname "$0")/..
test "$1" = api
test "$2" = --hostname
test "$3" = github.com
case "$4" in
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
      cat "$fixture/pull.json"
    fi
    ;;
  *) exit 2 ;;
esac
