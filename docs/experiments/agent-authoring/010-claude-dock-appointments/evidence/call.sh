#!/usr/bin/env bash
# call.sh <route-path> <json-envelope> — one request against the held release.
set -euo pipefail
BASE="$(cat "$WAMN_PILOT_RUN_DIR/evidence/base_url")"
TOKEN="$(jq -r '.stringData.token' "$WAMN_ROUTE_CALLER_PAT_FILE")"
curl --silent --show-error \
  --header "Host: $WAMN_ROUTE_HOST" \
  --header "Authorization: Bearer $TOKEN" \
  --header 'content-type: application/json' \
  --data "$2" \
  --write-out '\nHTTP %{http_code}\n' \
  "$BASE$1"
