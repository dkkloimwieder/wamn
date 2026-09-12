set -euo pipefail
record_command() {
  local argument
  printf '%s ' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$commands_record"
  for argument in "$@"; do
    printf '%q ' "$argument" >>"$commands_record"
  done
  printf '\n' >>"$commands_record"
}

run() {
  record_command "$@"
  "$@"
}

run_sensitive() {
  local label=$1
  shift
  record_command "$label" '<credential-bearing arguments redacted>'
  "$@"
}


  postcommit_probe_exit=0
  postcommit_probe_status=$(run curl --disable --noproxy '*' \
    --silent --show-error --connect-timeout 5 --max-time 15 \
    --retry 15 --retry-delay 1 --retry-connrefused --retry-max-time 45 \
    --dump-header "$evidence_dir/flow-http-probe.headers" \
    --output "$evidence_dir/flow-http-probe.body" --write-out '%{http_code}' \
    --header "Host: $route_host" "$postcommit_endpoint/no-such-route" \
    2>"$evidence_dir/flow-http-probe.stderr") || postcommit_probe_exit=$?
  jq -n --arg endpoint "$postcommit_endpoint" --arg host "$route_host" \
    --arg status "$postcommit_probe_status" --argjson exit_code "$postcommit_probe_exit" \
    '{transport:"owned-nodeport",endpoint:$endpoint,host:$host,
      http_status:$status,curl_exit_code:$exit_code}' \
    >"$evidence_dir/flow-http-probe-endpoint.json"
  postcommit_content_type=$(sed -n 's/^[Cc]ontent-[Tt]ype:[[:space:]]*//p' \
    "$evidence_dir/flow-http-probe.headers" | tr -d '\r' | tail -n 1)
  [[ "$postcommit_probe_exit" == 0 && "$postcommit_probe_status" == 404 &&
    "$postcommit_content_type" == application/json ]]
  cmp -s <(printf '%s' '{"error":{"code":"route-not-found"}}') \
    "$evidence_dir/flow-http-probe.body"
  printf 'RECEIVING_HTTP_PROBE {"status":404,"content_type":"application/json","host":"%s","body":{"error":{"code":"route-not-found"}}}\n' \
    "$route_host" >"$evidence_dir/flow-http-probe.log"

if [[ $(grep -Fc 'RECEIVING_HTTP_PROBE ' "$evidence_dir/flow-http-probe.log") -ne 1 ]]; then
  echo "flow-http probe did not emit exactly one typed-refusal receipt" >&2
  exit 1
fi
sed -n 's/^RECEIVING_HTTP_PROBE //p' "$evidence_dir/flow-http-probe.log" \
  >"$evidence_dir/flow-http-response.json"
jq -e --arg host "$route_host" '
  . == {status:404,content_type:"application/json",host:$host,
        body:{error:{code:"route-not-found"}}}
' >/dev/null "$evidence_dir/flow-http-response.json"


materializer_update_body='[{"request_id":"materializer-order","id":"00000000-0000-0000-0000-000000000304","expected_row_version":"1","change":{"acme_inspection_required":true,"acme_quality_status":"pending"}}]'
materializer_receipt_body='[{"request_id":"materializer-receipt","value":{"idempotency_key":"materializer-receipt-command","purchase_order_id":"00000000-0000-0000-0000-000000000304","receipt_reference":"MATERIALIZER-RECEIPT","occurred_at":"2026-08-31T12:34:00.000000Z","line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000504","quantity":"9.0000","location_id":"00000000-0000-0000-0000-000000000201"}]}}]'

  materializer_curl_config=$work_dir/materializer-curl.conf
  (
    umask 077
    jq -r '.stringData.token | "header = " + ("Authorization: Bearer " + . | @json)' \
      "$route_caller_secret" >"$materializer_curl_config"
  )
  [[ $(stat -c '%a' "$materializer_curl_config") == 600 ]]
  # Disable user curl configuration and retries: each POST must run only once.
  materializer_trigger_deadline=$((SECONDS + 90))
  materializer_update_status=$(run_sensitive materializer-http-update \
    curl --disable --config "$materializer_curl_config" --noproxy '*' \
    --silent --show-error --connect-timeout 5 --max-time 60 --retry 0 \
    --output "$work_dir/materializer-update.json" --write-out '%{http_code}' \
    --header "Host: $route_host" --header 'Content-Type: application/json' \
    --header "traceparent: 00-$materializer_update_trace-1111111111111111-01" \
    --data "$materializer_update_body" "$postcommit_endpoint/acme/purchase_order/update" \
    2>"$evidence_dir/materializer-update.stderr")
  [[ "$materializer_update_status" == 200 ]]
  materializer_receipt_timeout=$((materializer_trigger_deadline - SECONDS))
  ((materializer_receipt_timeout > 0))
  if ((materializer_receipt_timeout > 60)); then materializer_receipt_timeout=60; fi
  materializer_receipt_status=$(run_sensitive materializer-http-receipt \
    curl --disable --config "$materializer_curl_config" --noproxy '*' \
    --silent --show-error --connect-timeout 5 --max-time "$materializer_receipt_timeout" --retry 0 \
    --output "$work_dir/materializer-receipt.json" --write-out '%{http_code}' \
    --header "Host: $route_host" --header 'Content-Type: application/json' \
    --header "traceparent: 00-$materializer_receipt_trace-2222222222222222-01" \
    --data "$materializer_receipt_body" "$postcommit_endpoint/acme/receiving/record_receipt" \
    2>"$evidence_dir/materializer-receipt.stderr")
  [[ "$materializer_receipt_status" == 200 ]]
  jq -n --arg endpoint "$postcommit_endpoint" --arg host "$route_host" \
    --arg update "$materializer_update_status" --arg receipt "$materializer_receipt_status" \
    '{transport:"owned-nodeport",endpoint:$endpoint,host:$host,
      update_http_status:$update,receipt_http_status:$receipt}' \
    >"$evidence_dir/materializer-trigger-endpoint.json"
  {
    printf 'RECEIVING_MATERIALIZER_UPDATE '
    tr -d '\n' <"$work_dir/materializer-update.json"
    printf '\nRECEIVING_MATERIALIZER_RECEIPT '
    tr -d '\n' <"$work_dir/materializer-receipt.json"
    printf '\n'
  } >"$evidence_dir/materializer-trigger.log"

sed -n 's/^RECEIVING_MATERIALIZER_UPDATE //p' \
  "$evidence_dir/materializer-trigger.log" >"$evidence_dir/materializer-update.json"
sed -n 's/^RECEIVING_MATERIALIZER_RECEIPT //p' \
  "$evidence_dir/materializer-trigger.log" >"$evidence_dir/materializer-receipt.json"
jq -e '
  type == "array" and length == 1 and
  .[0].request_id == "materializer-order" and
  .[0].value.id == "00000000-0000-0000-0000-000000000304" and
  .[0].value.row_version == "2" and
  .[0].value.acme_inspection_required == true and
  .[0].value.acme_quality_status == "pending"
' >/dev/null "$evidence_dir/materializer-update.json"
jq -e '
  type == "array" and length == 1 and
  .[0].request_id == "materializer-receipt" and
  .[0].value.purchase_order_id == "00000000-0000-0000-0000-000000000304" and
  .[0].value.purchase_order_status == "complete" and
  .[0].value.row_version == "3" and
  .[0].value.acme_inspection_required == true and
  .[0].value.acme_quality_status == "pending" and
  (.[0].value.receipt_id | test("^[0-9a-f-]{36}$"))
' >/dev/null "$evidence_dir/materializer-receipt.json"
materializer_receipt_id=$(jq -er '.[0].value.receipt_id' \
  "$evidence_dir/materializer-receipt.json")

