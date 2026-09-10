#!/usr/bin/env bash
set -euo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-003
readonly lane=/home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910
cd "$lane"
export RUSTC_WRAPPER=
export WAMN_IDENTITY_BINARY="$lane/target/debug/wamn-identity"
export WAMN_SESSION_EXCHANGE_CTL_BIN="$lane/target/debug/wamn-ctl"
test -x "$WAMN_IDENTITY_BINARY"
test -x "$WAMN_SESSION_EXCHANGE_CTL_BIN"

run_suite() (
  set -euo pipefail
  local suite=$1 package=$2 test_name=$3 env_prefix=$4
  local container="wamn-ctc8-20-${suite}-20260910-003"
  if docker container inspect "$container" >/dev/null 2>&1; then
    printf 'Refuse existing container: %s\n' "$container" >&2
    exit 2
  fi
  docker run --detach --name "$container" \
    --label wamn.proof=ctc8-20-native-003 \
    -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_system \
    -p 127.0.0.1::5432 postgres:18 >"$evidence_dir/$suite.container"
  cleanup() {
    docker logs "$container" >"$evidence_dir/$suite.postgres.log" 2>&1
    docker rm --force --volumes "$container" >"$evidence_dir/$suite.cleanup.log" 2>&1
  }
  trap cleanup EXIT
  local port
  port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container")
  [[ "$port" =~ ^[0-9]+$ ]]
  local url="postgresql://postgres:probe@127.0.0.1:$port/wamn_system"
  local ready=0
  for attempt in {1..60}; do
    if psql "$url" -X -v ON_ERROR_STOP=1 -Atqc 'SELECT 1' >"$evidence_dir/$suite.ready.log" 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  test "$ready" = 1
  printf 'suite=%s package=%s test=%s database=wamn_system\n' "$suite" "$package" "$test_name"
  set +e
  env "${env_prefix}_PG_URL=$url" "${env_prefix}_ALLOW_SCHEMA_RESET=1" \
    cargo test --locked --offline -p "$package" --test "$test_name" \
    -- --include-ignored --nocapture --test-threads=1 >"$evidence_dir/$suite.log" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" >"$evidence_dir/$suite.exit"
  printf 'suite=%s exit=%s\n' "$suite" "$status"
  exit "$status"
)

run_suite service wamn-identity pat_issuance WAMN_PAT_ISSUANCE
run_suite bootstrap wamn-ctl pat_bootstrap_live WAMN_PAT_BOOTSTRAP
run_suite grants wamn-control-provision identity_issuer_live WAMN_IDENTITY_ISSUER
run_suite grants-cli wamn-ctl identity_issuer_live WAMN_IDENTITY_ISSUER_CLI
run_suite session wamn-identity session_exchange WAMN_SESSION_EXCHANGE
run_suite jwks wamn-identity https_surface WAMN_IDENTITY_SERVICE
