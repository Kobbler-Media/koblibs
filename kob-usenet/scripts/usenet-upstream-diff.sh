#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
pin=$(awk '/^Pin:/{print $2}' "$here/UPSTREAM.md")
fork=$(awk '/^SuperFork audited:/{print $3}' "$here/UPSTREAM.md")
paths=(crates/core crates/yenc crates/nntp crates/stream crates/archive crates/par2 crates/testkit crates/queue/src/import.rs crates/queue/src/import crates/queue/src/names.rs crates/queue/src/nzb.rs crates/vfs/src/reader.rs)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone -q https://github.com/infinidysk/infinidysk.git "$work/upstream"
git clone -q https://github.com/johoja12/infinidysk.git "$work/fork"
echo "== upstream since $pin"
git -C "$work/upstream" log --oneline "$pin"..HEAD -- "${paths[@]}"
echo "== SuperFork since $fork"
git -C "$work/fork" log --oneline "$fork"..HEAD -- "${paths[@]}"
