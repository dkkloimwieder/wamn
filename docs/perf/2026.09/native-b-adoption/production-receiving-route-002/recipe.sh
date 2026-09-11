set -euo pipefail
umask 077

RECEIVING_ROUTE_ROOT="$(pwd -P)"
RECEIVING_ROUTE_SCRATCH="$WAMN_CAPTURE_SCRATCH"
RECEIVING_ROUTE_PROJECT="$WAMN_CAPTURE_PROJECT"
RECEIVING_ROUTE_COMPOSE="$RECEIVING_ROUTE_ROOT/test-support/infrastructure/std-virtualization.compose.yaml"
RECEIVING_ROUTE_PG_PORT=54332
RECEIVING_ROUTE_REGISTRY_PORT=5004
RECEIVING_ROUTE_AUTHORITY="127.0.0.1:${RECEIVING_ROUTE_REGISTRY_PORT}"
RECEIVING_ROUTE_USERNAME=wamn-receiving-route
RECEIVING_ROUTE_PASSWORD="$(openssl rand -hex 32)"
RECEIVING_ROUTE_HTPASSWD="$RECEIVING_ROUTE_SCRATCH/htpasswd"
RECEIVING_ROUTE_DOCKER_AUTH="$RECEIVING_ROUTE_SCRATCH/.dockerconfigjson"
RECEIVING_ROUTE_CURL_AUTH="$RECEIVING_ROUTE_SCRATCH/curl.conf"
RECEIVING_ROUTE_HOST=receiving.localhost
RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY="$RECEIVING_ROUTE_SCRATCH/host-secrets"
RECEIVING_ROUTE_CALLER_SECRET_OUTPUT="$RECEIVING_ROUTE_SCRATCH/route-caller-pat.json"
RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY="$RECEIVING_ROUTE_SCRATCH/wasmtime-cache"
RECEIVING_ROUTE_SECRET_NAMESPACE=wamn-receiving-route
RECEIVING_ROUTE_JOURNEY_DOCUMENT="$RECEIVING_ROUTE_SCRATCH/journey.json"
install -d -m 0700 "$RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY" \
  "$RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY"

receiving_route_cleanup() {
  docker compose --profile receiving-route -p "$RECEIVING_ROUTE_PROJECT" \
    -f "$RECEIVING_ROUTE_COMPOSE" down --volumes --remove-orphans \
    >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.log" 2>&1 \
    && printf "0\n" >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.exit" \
    || printf "%s\n" "$?" >"$RECEIVING_ROUTE_SCRATCH/compose-cleanup.exit"
  # The capture wrapper scans private output, then removes this exact scratch directory.
}
trap receiving_route_cleanup EXIT

RECEIVING_ROUTE_COMPONENTS="$RECEIVING_ROUTE_ROOT/target/virtualized/std-empty-environment"
# `cc4b407f` moved every guest to the release profile. It left this path
# behind, so the `test -s` below has refused since 2026-09-04.
RECEIVING_ROUTE_FLOW_HTTP="$RECEIVING_ROUTE_ROOT/target/wasm32-wasip2/release/http_route.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/receiving.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/client_acme_receiving.wasm"
test -s "$RECEIVING_ROUTE_FLOW_HTTP"

# The Gate the proof spawns. Built into the default target directory, the one
# the test build below also uses.
RECEIVING_ROUTE_GATE_BIN="$RECEIVING_ROUTE_ROOT/target/debug/wamn-scenario-worker"
RECEIVING_ROUTE_IDENTITY_BIN="$RECEIVING_ROUTE_ROOT/target/debug/wamn-identity"
test -x "$RECEIVING_ROUTE_GATE_BIN"
test -x "$RECEIVING_ROUTE_IDENTITY_BIN"
# The spawned Gate binds this exact port. A stray listener is a hard failure,
# not a fallback to an ephemeral one.
test -z "$(ss -Hltn 'sport = :18089')"

printf '%s\n' "$RECEIVING_ROUTE_PASSWORD" \
  | docker run --rm -i --entrypoint htpasswd httpd:2-alpine \
      -Bni "$RECEIVING_ROUTE_USERNAME" >"$RECEIVING_ROUTE_HTPASSWD"
jq -n --arg authority "$RECEIVING_ROUTE_AUTHORITY" \
  --arg username "$RECEIVING_ROUTE_USERNAME" \
  --arg password "$RECEIVING_ROUTE_PASSWORD" \
  '{auths:{($authority):{username:$username,password:$password}}}' \
  >"$RECEIVING_ROUTE_DOCKER_AUTH"
printf 'user = "%s:%s"\n' \
  "$RECEIVING_ROUTE_USERNAME" "$RECEIVING_ROUTE_PASSWORD" \
  >"$RECEIVING_ROUTE_CURL_AUTH"
unset RECEIVING_ROUTE_PASSWORD
jq -e --arg authority "$RECEIVING_ROUTE_AUTHORITY" '
  .auths[$authority]
  | (.username | type == "string" and length > 0)
    and (.password | type == "string" and length > 0)
' "$RECEIVING_ROUTE_DOCKER_AUTH" >/dev/null

WAMN_STD_VIRT_PG_PORT="$RECEIVING_ROUTE_PG_PORT"
WAMN_STD_VIRT_REGISTRY_PORT=5003
WAMN_ROUTE_REGISTRY_PORT="$RECEIVING_ROUTE_REGISTRY_PORT"
WAMN_ROUTE_REGISTRY_HTPASSWD="$RECEIVING_ROUTE_HTPASSWD"
export WAMN_STD_VIRT_PG_PORT WAMN_STD_VIRT_REGISTRY_PORT
export WAMN_ROUTE_REGISTRY_PORT WAMN_ROUTE_REGISTRY_HTPASSWD
docker compose --profile receiving-route -p "$RECEIVING_ROUTE_PROJECT" \
  -f "$RECEIVING_ROUTE_COMPOSE" up --detach --wait --wait-timeout 60 \
  receiving-route-postgres authenticated-registry
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/postgres" \
  -Atqc 'select 1' >/dev/null
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/postgres" \
  -v ON_ERROR_STOP=1 -c 'CREATE DATABASE wamn_system'
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://${RECEIVING_ROUTE_AUTHORITY}/v2/")" = 401
test "$(curl --config "$RECEIVING_ROUTE_CURL_AUTH" --silent --show-error \
  --output /dev/null --write-out '%{http_code}' \
  "http://${RECEIVING_ROUTE_AUTHORITY}/v2/")" = 200

# The test's inputs are ONE document, not a dozen environment variables. The
# schema is generated from the Rust struct that reads the file and drift-tested
# there; the writer reads the same schema, so a key this side invents or omits
# is refused here, naming it, rather than by the Rust side after the build.
source tools/journey-document.sh
declare -A journey_spec=(
  [system_pg_url]="postgresql://postgres:probe@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/wamn_system"
  [component_directory]="$RECEIVING_ROUTE_COMPONENTS"
  [compilation_cache_directory]="$RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY"
  [flow_http_wasm]="$RECEIVING_ROUTE_FLOW_HTTP"
  [component_artifact_base]="$RECEIVING_ROUTE_AUTHORITY/wamn/components"
  [release_artifact_base]="$RECEIVING_ROUTE_AUTHORITY/wamn/releases"
  [route_host]="$RECEIVING_ROUTE_HOST"
  [registry_auth_file]="$RECEIVING_ROUTE_DOCKER_AUTH"
  [host_secret_directory]="$RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY"
  [host_secret_namespace]="$RECEIVING_ROUTE_SECRET_NAMESPACE"
  [route_caller_secret_output]="$RECEIVING_ROUTE_CALLER_SECRET_OUTPUT"
)
write_journey_document journey_spec \
  tests/integration/schema/wamn-journey.schema.json "$RECEIVING_ROUTE_JOURNEY_DOCUMENT"
WAMN_JOURNEY_DOCUMENT="$RECEIVING_ROUTE_JOURNEY_DOCUMENT" \
WAMN_JOURNEY_SCENARIO_WORKER_BIN="$RECEIVING_ROUTE_GATE_BIN" \
WAMN_IDENTITY_BINARY="$RECEIVING_ROUTE_IDENTITY_BIN" \
  "$@"

receiving_route_cleanup
trap - EXIT
