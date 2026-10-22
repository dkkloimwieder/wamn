#!/usr/bin/env bash
# Owner-run calls through the deployed NetSuite wirings. Never a test target.
set -euo pipefail
set +x
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage:
  live.sh prepare CREDENTIALS_JSON ORG OUTPUT_DIRECTORY
  live.sh run CREDENTIALS_JSON ORG TENANT ENVIRONMENT RELEASE_DIGEST PRINCIPAL RECORD_ID QUERY_FILE

prepare writes a connection definition and host credentials for the owner to deploy.
run requires WAMN_PG_ADMIN_URL, WAMN_CONTROL_DATABASE_URL, and WAMN_CREDENTIALS_FILE.
WAMN_CREDENTIALS_FILE must name the source of the deployed host credential mount.
Set WAMN_CTL to the CLI path if it is not target/debug/wamn-ctl.
USAGE
  exit 2
}
[[ $# -ge 3 ]] || usage
mode=$1 credentials=$2 org=$3
jq -e '
  [.account_host,.realm,.consumer_key,.consumer_secret,.token_id,.token_secret]
  | all(.[]; type == "string" and length > 0)
' "$credentials" >/dev/null
host=$(jq -er '.account_host' "$credentials")
[[ $host =~ ^[a-zA-Z0-9-]+\.suitetalk\.api\.netsuite\.com$ ]] || {
  echo 'account_host must be the account SuiteTalk hostname, without a scheme or path.' >&2
  exit 2
}

render_credentials() {
  jq --arg org "$org" '{projects:{},orgs:{($org):{netsuite:({oauth1:{
    "consumer-key":.consumer_key,"consumer-secret":.consumer_secret,
    token:.token_id,"token-secret":.token_secret,realm:.realm
  }}|tojson)}}}' "$credentials"
}

if [[ $mode == prepare ]]; then
  [[ $# == 4 ]] || usage
  output=$4
  mkdir -p "$output"
  # Refuse to replace an existing file that can hold another deployment's keys.
  set -o noclobber
  render_credentials > "$output/credentials.json"
  jq -n --arg authority "https://$host" \
    '{"primary-authority":$authority,"tls-verification":"verify-authority"}' > "$output/definition.json"
  printf 'Prepared %s/credentials.json and %s/definition.json. Deploy them before run.\n' "$output" "$output"
  exit 0
fi
[[ $mode == run && $# == 9 ]] || usage
: "${WAMN_PG_ADMIN_URL:?Set the deployed project admin database URL}"
: "${WAMN_CONTROL_DATABASE_URL:?Set the control database URL}"
: "${WAMN_CREDENTIALS_FILE:?Set the source of the deployed host credential mount}"
tenant=$4 environment=$5 digest=$6 principal=$7 record_id=$8 query_file=$9
ctl=${WAMN_CTL:-target/debug/wamn-ctl}
work=$(mktemp -d "${TMPDIR:-/var/tmp}/wamn-netsuite-live.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
render_credentials > "$work/expected.json"
jq -en --arg org "$org" --slurpfile expected "$work/expected.json" --slurpfile mounted "$WAMN_CREDENTIALS_FILE" \
  '($expected[0].orgs[$org].netsuite|fromjson) == ($mounted[0].orgs[$org].netsuite|fromjson)' >/dev/null || {
  echo 'The supplied keys differ from the deployed credential source.' >&2
  exit 1
}
authority=$(psql -XAt "$WAMN_CONTROL_DATABASE_URL" -v ON_ERROR_STOP=1 -v org="$org" <<'SQL'
SELECT definition->>'primary-authority' FROM registry.org_connections WHERE org=:'org' AND name='netsuite' AND type='http';
SQL
)
[[ $authority == "https://$host" ]] || { echo 'The org connection differs from account_host. Declare and apply it first.' >&2; exit 1; }
jq -n --arg id "$record_id" '{id:$id}' > "$work/record_get.json"
jq -n --rawfile q "$query_file" '{q:$q}' > "$work/suiteql_query.json"
for wiring in record_get suiteql_query; do
  run_id=$("$ctl" workflow start --tenant "$tenant" --environment "$environment" \
    --release-digest "$digest" --package-id netsuite --wiring-id "$wiring" --wiring-version 1 \
    --service-principal-id "$principal" --idempotency-key "netsuite-live:$(basename "$work"):$wiring" \
    --input "$work/$wiring.json")
  deadline=$((SECONDS + 120))
  while :; do
    result=$(psql -XAt "$WAMN_PG_ADMIN_URL" -v ON_ERROR_STOP=1 -v run="$run_id" -v tenant="$tenant" <<'SQL'
SELECT jsonb_build_object('run_id',run_id,'status',status,'result',result_json,'reason',terminal_reason)
FROM wamn_run.runs WHERE run_id=:'run' AND tenant_id=:'tenant';
SQL
)
    status=$(jq -er '.status' <<< "$result")
    case $status in
      completed) printf '%s: %s\n' "$wiring" "$result"; break ;;
      failed|infrastructure-failure|effect-uncertain|cancelled|expired)
        printf '%s: %s\n' "$wiring" "$result" >&2; exit 1 ;;
    esac
    if (( SECONDS >= deadline )); then
      printf 'Run %s did not finish within 120 seconds. Inspect it before another call.\n' "$run_id" >&2
      exit 1
    fi
    sleep 1
  done
done
