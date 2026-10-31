#!/usr/bin/env bash
# Owner-run M2M calls. Never a test target.
# Credentials JSON: {"account_id":"1234567-sb1","client_id":"...",
#   "certificate_id":"...","algorithm":"PS256","private_key_file":"/private/key.pem"}
# account_id is the account hostname label. algorithm is PS256 or ES256.
# A relative private_key_file path resolves from the current directory.
# Requires jq, psql, WAMN_CONTROL_URL, WAMN_TOKEN, WAMN_PG_ADMIN_URL.
# The application must enable the manual workflows record_get and suiteql_query.
# Usage: live.sh CREDENTIALS_JSON PROJECT ENV APPLICATION HOST_URL RECORD_ID QUERY_FILE
set -eu
set +x
umask 077
[ "$#" -eq 7 ] || { sed -n '2,9p' "$0" >&2; exit 2; }
credentials=$1 project=$2 env=$3 application=$4 host_url=$5 record_id=$6 query_file=$7
: "${WAMN_CONTROL_URL:?Set the organization control host URL}"
: "${WAMN_TOKEN:?Set the caller bearer token}"
: "${WAMN_PG_ADMIN_URL:?Set the deployed project admin database URL}"
export WAMN_TOKEN
ctl=${WAMN_CTL:-wamn-ctl}
jq -e '
  ([.account_id,.client_id,.certificate_id,.private_key_file]
    | all(.[]; type == "string" and length > 0))
  and (.account_id | test("^[A-Za-z0-9-]+$"))
  and (.algorithm == "PS256" or .algorithm == "ES256")
' "$credentials" >/dev/null
work=$(mktemp -d "${TMPDIR:-/var/tmp}/wamn-netsuite-live.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
jq -n --arg project "$project" \
  '{project:$project,name:"netsuite",integration_type:"http",auth_type:"oauth2_client_credentials_jwt"}' \
  > "$work/integration.json"
jq --arg project "$project" --arg env "$env" '
  ("https://" + .account_id + ".suitetalk.api.netsuite.com") as $origin |
  {project:$project,env:$env,name:"netsuite",parameters:{
    "primary-authority":$origin,"tls-verification":"verify-authority",
    auth:{client_id:.client_id,token_url:($origin + "/services/rest/auth/oauth2/v1/token"),
      scopes:["rest_webservices"],key_id:.certificate_id,algorithm:.algorithm,scope_separator:","}}}
' "$credentials" > "$work/connection.json"
key_file=$(jq -er '.private_key_file' "$credentials")
jq -n --rawfile private_key "$key_file" '{private_key:$private_key}' > "$work/credential.json"
"$ctl" integration write --file "$work/integration.json"
"$ctl" connection write --file "$work/connection.json" --credential-file "$work/credential.json"
jq -n --arg id "$record_id" '{id:$id}' > "$work/record_get.json"
jq -n --rawfile q "$query_file" '{q:$q}' > "$work/suiteql_query.json"
for wiring in record_get suiteql_query; do
  run_id=$("$ctl" workflow start --host-url "$host_url" --application "$application" \
    --workflow-id "$wiring" --idempotency-key "netsuite-live:$(basename "$work"):$wiring" \
    --input "$work/$wiring.json")
  deadline=$((SECONDS + 120))
  while :; do
    result=$(psql -XAt "$WAMN_PG_ADMIN_URL" -v ON_ERROR_STOP=1 -v run="$run_id" <<'SQL'
SELECT jsonb_build_object('run_id',run_id,'status',status,'result',result_json,'reason',terminal_reason)
FROM wamn_run.runs WHERE run_id=:'run';
SQL
)
    status=$(printf '%s' "$result" | jq -er '.status')
    case $status in
      completed) printf '%s: %s\n' "$wiring" "$result"; break ;;
      failed|infrastructure-failure|cancelled|expired)
        printf '%s: %s\n' "$wiring" "$result" >&2; exit 1 ;;
    esac
    if [ "$SECONDS" -ge "$deadline" ]; then
      printf 'Run %s did not finish within 120 seconds. Inspect it before another call.\n' "$run_id" >&2
      exit 1
    fi
    sleep 1
  done
done
