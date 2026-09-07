#!/usr/bin/env bash
set -uo pipefail
W=/home/kaalin/.cache/wamn-pilot/runs/012-claude-dock-appointments/work
BASE="$(sed -n 's/^run served: \([^ ]*\) .*$/\1/p' "$W/hold.out" | tail -1)"
TOKEN="$(jq -r '.stringData.token' "$WAMN_ROUTE_CALLER_PAT_FILE")"
# the log is appended to by the caller

call() {  # call <label> <path> <json body>
  local label="$1" path="$2" body="$3"
  local out status payload
  out="$(curl --silent --show-error --max-time 60 \
    --header "Host: $WAMN_ROUTE_HOST" \
    --header "Authorization: Bearer $TOKEN" \
    --header 'content-type: application/json' \
    --data "$body" --write-out '\n%{http_code}' "$BASE$path")"
  status="$(tail -1 <<<"$out")"
  payload="$(sed '$d' <<<"$out")"
  {
    printf '### %s\n' "$label"
    printf 'POST %s\n' "$path"
    printf 'request:  %s\n' "$body"
    printf 'status:   %s\n' "$status"
    printf 'response: %s\n\n' "$payload"
  } >> "$W/http.log"
  printf '%s' "$payload"
}
