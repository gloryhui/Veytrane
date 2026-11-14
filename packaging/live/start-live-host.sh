#!/bin/sh
set -eu
live_repo=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
live_mtx=${MEDIAMTX_BIN:-"$live_repo/packaging/live/mediamtx"}
live_gateway="$live_repo/target/release/live-gateway"
live_data="$live_repo/live-private/gateway"
if [ ! -x "$live_mtx" ]; then
  echo 'Set MEDIAMTX_BIN to the official MediaMTX v1.21.2 executable.' >&2
  exit 1
fi
if [ ! -f "$live_data/gateway.json" ]; then
  echo 'Initialize live-gateway in live-private/gateway first; see README.md.' >&2
  exit 1
fi
"$live_mtx" "$live_repo/packaging/live/mediamtx.yml" &
live_child=$!
trap 'kill "$live_child" 2>/dev/null || true; wait "$live_child" 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM
sleep 1
kill -0 "$live_child"
"$live_gateway" --data-dir "$live_data"
