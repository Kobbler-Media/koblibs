#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
pin=$(awk '/^Pin:/{print $2}' "$here/UPSTREAM.md")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone -q https://github.com/infinidysk/infinidysk.git "$work/infinidysk"
git -C "$work/infinidysk" checkout -q "$pin"
git -C "$work/infinidysk" submodule update -q --init crates/yenc/rapidyenc
git -C "$work/infinidysk" -c user.name=kob -c user.email=kob@invalid -c commit.gpgsign=false am -q "$here"/upstream/patches/*.patch
python3 -I "$here/scripts/vendor.py" "$work/infinidysk" - "$here"
