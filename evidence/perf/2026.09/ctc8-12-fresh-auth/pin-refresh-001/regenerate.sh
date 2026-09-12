#!/usr/bin/env bash
# Regenerate only the overlay against its own fresh base-plus-overlay database.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_tree=$(git -C "$evidence" rev-parse --show-toplevel)
cd -- "$source_tree"
export RUSTC_WRAPPER=sccache
container_name=wamn-ctc8-12-pin-refresh-pg-20260908-$$
container_id=
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n $container_id ]]; then
    if [[ $(docker inspect --format '{{.Name}}' "$container_id") == "/$container_name" &&
          $(docker inspect --format '{{index .Config.Labels "wamn.proof.owner"}}' "$container_id") == ctc8-12-pin-refresh ]]; then
      docker rm -f "$container_id" >"$evidence/cleanup.log" 2>&1 || status=1
    else
      echo 'refuse cleanup: container ownership does not match' >&2
      status=1
    fi
  fi
  printf '%s\n' "$status" >"$evidence/exit"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM
run() {
  printf '%q ' "$@" >>"$evidence/commands.log"
  printf '\n' >>"$evidence/commands.log"
  "$@"
}
[[ ! -e $evidence/commands.log ]]
git rev-parse HEAD >"$evidence/source-head"
date -u
sha256sum target/virtualized/std-empty-environment/receiving.wasm >"$evidence/base-component.sha256"
[[ $(cut -d ' ' -f 1 "$evidence/base-component.sha256") == 8057a076d949d21effa45dfff27812f995f5ac101c38f52ac6d66108f1b15b60 ]]
if docker inspect "$container_name" >/dev/null 2>&1; then
  echo "refuse existing container: $container_name" >&2
  exit 1
fi
container_id=$(docker run -d --name "$container_name" \
  --label wamn.proof.owner=ctc8-12-pin-refresh \
  -e POSTGRES_PASSWORD=probe -p 127.0.0.1::5432 postgres:18)
port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container_id")
[[ $port =~ ^[0-9]+$ ]]
database_url=postgresql://postgres:probe@127.0.0.1:$port/postgres
ready=false
for _ in {1..60}; do
  if psql "$database_url" -Atqc 'select 1' >"$evidence/ready.log" 2>"$evidence/ready-error.log"; then
    ready=true
    break
  fi
  sleep 1
done
[[ $ready == true ]]
docker inspect --format '{{.Id}} {{.Image}} {{.Name}}' "$container_id" >"$evidence/container.log"
run psql "$database_url" -v ON_ERROR_STOP=1 -c 'SELECT version(); CREATE SCHEMA receiving;'
run psql "$database_url" -v ON_ERROR_STOP=1 -f packages/receiving/migrations/0001_initial.sql
run psql "$database_url" -v ON_ERROR_STOP=1 -f packages/client_acme_receiving/migrations/0001_add_inspection_required.sql
run psql "$database_url" -v ON_ERROR_STOP=1 -f packages/client_acme_receiving/migrations/0002_quality_inspection.sql
export WAMN_SCHEMA_INTROSPECTION_PG_URL=$database_url
run cargo run -p wamn-schema-generator --example materialize_package --locked --offline -- write packages/client_acme_receiving
run cargo run -p wamn-schema-generator --example materialize_package --locked --offline -- check packages/client_acme_receiving
run cargo run -p wamn-schema-generator --example materialize_package --locked --offline -- check packages/client_acme_receiving
run cargo test -p wamn-proof-integration --lib --locked --offline acme_overlay_publication -- --include-ignored --nocapture
run cargo test -p wamn-catalog --lib --locked --offline component_dependency_closure_is_exact_and_acyclic -- --include-ignored --nocapture
run cargo test -p wamn-ctl --lib --locked --offline component_dependencies_expand_the_exact_release_closure_and_refuse_cycles -- --include-ignored --nocapture
git diff --check
date -u
