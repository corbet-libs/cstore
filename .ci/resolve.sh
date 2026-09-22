#!/usr/bin/env bash
set -euo pipefail
: "${CARGO_HOME:?CI must supply its persistent Cargo cache}"
: "${CI_COMMIT_SHA:?CI must identify the source}"
export CARGO_NET_OFFLINE=false
cargo generate-lockfile
lock_digest=$(sha256sum Cargo.lock | cut -d ' ' -f 1)
artifact_dir="$CARGO_HOME/ccid-artifacts/cstore/$CI_COMMIT_SHA"
mkdir -p "$artifact_dir"
artifact_path="$artifact_dir/$lock_digest.Cargo.lock"
if [ -e "$artifact_path" ]; then
  cmp Cargo.lock "$artifact_path"
else
  cp Cargo.lock "$artifact_path"
fi
rustc --version
cargo --version
date -u +%FT%TZ
printf 'CSTORE_LOCKFILE %s %s\n' "$artifact_path" "$lock_digest"
