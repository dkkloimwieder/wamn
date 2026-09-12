#!/usr/bin/env bash
# Run one pre-applied mutant. Never edit or restore the source from this script.
set -euo pipefail
[[ $# == 4 ]] || { echo 'usage: mutation.sh SOURCE_TREE EXACT_HEAD mutation-NNN ASSERTION_TEXT' >&2; exit 2; }
source_tree=$1
expected=$2
run_name=$3
assertion=$4
[[ $expected =~ ^[0-9a-f]{40}$ && $run_name =~ ^mutation-[0-9]{3}$ ]]
[[ -n $assertion && $assertion != *$'\n'* ]]
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
mkdir -m 0700 -- "$evidence/$run_name"
output=$evidence/$run_name
container_id=
container_name=
cleanup() {
  local status=$?
  trap - EXIT
  set +e
  if [[ -n $container_id ]]; then
    docker logs "$container_id" >"$output/postgres.log" 2>&1 || status=1
    docker inspect --format '{{.Id}} {{.Image}} {{.Name}} {{index .Config.Labels "wamn.proof.owner"}}' \
      "$container_id" >"$output/cleanup-identity" 2>&1 || status=1
    if [[ $(docker inspect --format '{{.Name}}' "$container_id") == "/$container_name" &&
          $(docker inspect --format '{{index .Config.Labels "wamn.proof.owner"}}' "$container_id") == ctc8-12-mutation ]]; then
      docker rm -f "$container_id" >>"$output/cleanup.log" 2>&1 || status=1
    else
      echo 'refuse cleanup: the container ownership does not match' >>"$output/cleanup.log"
      status=1
    fi
  else
    echo 'no owned container was started' >>"$output/cleanup.log"
  fi
  date -u >"$output/finished"
  printf '%s\n' "$status" >"$output/exit"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM
exec >"$output/script.log" 2>&1
printf '%q ' "$0" "$@" >"$output/invocation"
printf '\n' >>"$output/invocation"
printf '%s\n' "$expected" >"$output/expected-head"
printf '%s\n' "$assertion" >"$output/expected-assertion"
date -u >"$output/started"
cd -- "$source_tree"
source_tree=$(pwd -P)
git rev-parse HEAD >"$output/source-head"
git status --porcelain=v1 --untracked-files=no >"$output/source-status"
git diff --binary HEAD >"$output/source.diff"
git diff --no-renames --name-only -z >"$output/unstaged-paths"
git diff --cached --no-renames --name-only -z >"$output/staged-paths"
[[ $(<"$output/source-head") == "$expected" ]] || { echo 'source HEAD does not match'; exit 1; }
identity=crates/identity/platform/src/lib.rs
route=crates/platform/runtime/src/plugins/flow_http_routing.rs
for paths in "$output/unstaged-paths" "$output/staged-paths"; do
  while IFS= read -r -d '' path; do
    case $path in
      "$identity"|"$route") ;;
      *) printf 'refuse dirty tracked path: %s\n' "$path"; exit 1 ;;
    esac
  done <"$paths"
done
[[ -s $output/source.diff ]] || { echo 'refuse a source tree without a mutant diff'; exit 1; }
sha256sum "$identity" "$route" Cargo.lock tests/integration/src/route_authentication_live.rs >"$output/source.sha256"
container_name=wamn-ctc8-12-$run_name-$$
if docker inspect "$container_name" >/dev/null 2>&1; then
  echo "refuse reuse of existing container: $container_name"
  exit 1
fi
docker_command=(docker run -d --name "$container_name" --label wamn.proof.owner=ctc8-12-mutation
  -e POSTGRES_PASSWORD=probe -p 127.0.0.1::5432 postgres:18)
printf '%q ' "${docker_command[@]}" >"$output/postgres-command"
printf '\n' >>"$output/postgres-command"
"${docker_command[@]}" >"$output/container-id" 2>"$output/postgres-start-error"
container_id=$(<"$output/container-id")
[[ $container_id =~ ^[0-9a-f]{64}$ ]]
port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container_id")
[[ $port =~ ^[0-9]+$ ]]
[[ $(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostIp}}' "$container_id") == 127.0.0.1 ]]
url=postgresql://postgres:probe@127.0.0.1:$port/postgres
ready=false
for attempt in {1..60}; do
  printf 'attempt=%s\n' "$attempt" >>"$output/postgres-ready.log"
  if psql "$url" -X -v ON_ERROR_STOP=1 -Atqc 'select 1' >>"$output/postgres-ready.log" 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
[[ $ready == true ]] || { echo 'PostgreSQL did not accept a published-port query'; exit 1; }
docker inspect --format '{{.Id}} {{.Image}} {{.Name}}' "$container_id" >"$output/postgres-container"
psql "$url" -X -v ON_ERROR_STOP=1 -Atqc 'select system_identifier from pg_control_system(); select version()' >"$output/postgres-identity"
psql "$url" -X -v ON_ERROR_STOP=1 -Atqc 'show server_version_num' >"$output/postgres-version"
[[ $(<"$output/postgres-version") =~ ^18[0-9]{4}$ ]] || { echo 'the owned server is not PostgreSQL 18'; exit 1; }
test_name=route_authentication_live::production_route_caller_authentication_and_operation_authorization
command=(env RUSTC_WRAPPER=sccache "CARGO_TARGET_DIR=$source_tree/target" "WAMN_ROUTE_AUTH_PG18_URL=$url"
  cargo test -p wamn-proof-integration --lib --locked --offline "$test_name"
  -- --ignored --exact --nocapture --test-threads=1)
printf '%q ' "${command[@]}" >"$output/command"
printf '\n' >>"$output/command"
cargo_status=0
"${command[@]}" >"$output/cargo.log" 2>&1 || cargo_status=$?
printf '%s\n' "$cargo_status" >"$output/cargo.exit"
[[ $cargo_status == 101 ]] || { echo 'the mutant did not produce Cargo test exit 101'; exit 1; }
rg -Fxq 'running 1 test' "$output/cargo.log"
rg -Fxq "    $test_name" "$output/cargo.log"
rg -q '^test result: FAILED\. 0 passed; 1 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "$output/cargo.log"
rg -Fq -- "$assertion" "$output/cargo.log" || { echo 'the expected assertion did not fail'; exit 1; }
printf 'mutation-killed: %s\n' "$test_name" >"$output/result"
