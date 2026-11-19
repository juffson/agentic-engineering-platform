#!/bin/sh
set -eu
case "$*" in
  'remote get-url origin'|'remote get-url --push --all origin')
    printf '%s\n' 'https://github.com/example/aep-fixture.git'
    ;;
  *) exec /usr/bin/git "$@" ;;
esac
