#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
cd "$root"

digest() {
  skip=$1
  shift
  {
    git ls-tree -r HEAD -- "$@"
    git diff HEAD -- "$@" 2>/dev/null || true
  } | grep -Ev "$skip" | shasum -a 256 | cut -c 1-16
}

api=$(digest '^$' api agent shared migrations skills deploy Cargo.toml Cargo.lock desktop/compat.json)
web=$(digest '^$' frontend desktop/compat.json)
runner=$(digest '[[:space:]]a?/?agent/templates/' agent shared skills deploy Cargo.toml Cargo.lock)
shell=$(digest '[[:space:]]a?/?desktop/(compat\.json|tests/)' desktop)
schema=$(ls migrations | sed -n 's/^\([0-9][0-9]*\)_.*\.sql$/\1/p' | sort -n | tail -n 1 | sed 's/^0*//')

printf '{"api":"%s","web":"%s","runner":"%s","shell":"%s","schema":%s,"levels":%s}\n' \
  "$api" "$web" "$runner" "$shell" "${schema:-0}" "$(tr -d ' \n' < desktop/compat.json)"
