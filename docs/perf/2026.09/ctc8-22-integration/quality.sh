#!/usr/bin/env bash
# Capture focused tests. Each stateful binary owns a fresh PostgreSQL server.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_tree=${1:?source worktree required}
expected=${2:?expected source commit required}
run_name=${3:?quality-NNN required}
[[ $run_name =~ ^quality-[0-9]{3}$ ]]
cd -- "$source_tree"
[[ $(git rev-parse HEAD) == "$expected" ]]
[[ -z $(git status --porcelain) ]]
mkdir -m 0700 -- "$evidence/$run_name"
output=$evidence/$run_name
export RUSTC_WRAPPER=sccache
container_id=
container_name=
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n $container_id ]]; then
    if [[ $(docker inspect --format '{{.Name}}' "$container_id") == "/$container_name" &&
          $(docker inspect --format '{{index .Config.Labels "wamn.proof.owner"}}' "$container_id") == ctc8-12-quality ]]; then
      docker rm -f "$container_id" >>"$output/cleanup.log" 2>&1 || status=1
    else
      echo "refuse cleanup: the container ownership does not match" >&2
      status=1
    fi
  fi
  printf '%s\n' "$status" >"$output/exit"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM
run_test() {
  local label=$1
  shift
  printf '%q ' "$@" >>"$output/commands.log"
  printf '\n' >>"$output/commands.log"
  local status=0
  "$@" >"$output/$label.log" 2>&1 || status=$?
  printf '%s\n' "$status" >"$output/$label.exit"
  if (( status != 0 )); then
    tail -80 "$output/$label.log"
    return "$status"
  fi
  if ! rg -q 'test result: ok\. [1-9][0-9]* passed; 0 failed;' "$output/$label.log"; then
    echo "the test filter did not execute a passing test: $label" >&2
    return 1
  fi
  tail -4 "$output/$label.log"
}
run_postgres_test() {
  local label=$1 variable=$2
  shift 2
  container_name=wamn-ctc8-12-$run_name-$label-$$
  if docker inspect "$container_name" >/dev/null 2>&1; then
    echo "refuse reuse of existing container: $container_name" >&2
    return 1
  fi
  container_id=$(docker run -d --name "$container_name" \
    --label wamn.proof.owner=ctc8-12-quality \
    -e POSTGRES_PASSWORD=probe -p 127.0.0.1::5432 postgres:18)
  local port url ready=false
  port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container_id")
  [[ $port =~ ^[0-9]+$ ]]
  url=postgresql://postgres:probe@127.0.0.1:$port/postgres
  for _ in {1..60}; do
    if psql "$url" -Atqc 'select 1' >"$output/$label.ready" 2>"$output/$label.ready-error"; then
      ready=true
      break
    fi
    sleep 1
  done
  [[ $ready == true ]] || { echo "PostgreSQL did not accept a published-port query" >&2; return 1; }
  docker inspect --format '{{.Id}} {{.Image}} {{.Name}}' "$container_id" >"$output/$label.container"
  psql "$url" -Atqc 'select version()' >"$output/$label.postgres-version"
  run_test "$label" env "$variable=$url" "$@"
  docker rm -f "$container_id" >>"$output/cleanup.log"
  container_id=
}
git rev-parse HEAD >"$output/source-head"
date -u >"$output/started"
uptime >"$output/load-start"
run_test identity-unit cargo test -p wamn-platform-identity --lib --locked --offline -- --include-ignored
run_postgres_test identity-live WAMN_PLATFORM_IDENTITY_PG_URL \
  cargo test -p wamn-platform-identity --test identity_live --locked --offline -- --include-ignored --nocapture --test-threads=1
run_postgres_test pat-live WAMN_PLATFORM_IDENTITY_PG_URL \
  cargo test -p wamn-platform-identity --test pat_live --locked --offline -- --include-ignored --nocapture --test-threads=1
run_postgres_test route-auth WAMN_ROUTE_AUTH_PG18_URL \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  route_authentication_live::production_route_caller_authentication_and_operation_authorization \
  -- --ignored --exact --nocapture --test-threads=1
run_test nested-permission cargo test -p wamn-execution-host --lib --locked --offline \
  router_driver::tests::nested_permission_denial_survives_the_real_component_boundary -- --exact --include-ignored
run_test connection-http cargo test -p wamn-runtime --lib --locked --offline plugins::connection_http::tests:: -- --include-ignored
run_test blobstore-candidates cargo test -p wamn-runtime --lib --locked --offline plugins::wamn_blobstore::plugin::tests:: -- --include-ignored
date -u >"$output/finished"
uptime >"$output/load-end"
