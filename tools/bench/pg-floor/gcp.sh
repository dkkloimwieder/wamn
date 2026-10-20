#!/usr/bin/env bash
# The Postgres floor on GCP (wamn-c8a5v.1): the replays of local.sh --pg-floor
# against wamn-pg-1, with pgbench in the Job gcp-client.yaml on pool bench.
# update-no-triggers is the update replay with both record-history triggers
# disabled (perf-arch.md M4). It runs only in the database wamn_pg_floor,
# never in an environment database.
#
# The caller exports CLOUDSDK_CONFIG and KUBECONFIG for wamn-dev, keeps the
# PostgreSQL forward of gcp.md section 4.5 on 127.0.0.1:15432, and sets
# CARGO_TARGET_DIR to the target that holds a debug build of
# `cargo build -p wamn-control --example pg_floor_database`. Run `wamn build
# apps/wamn_receiving` first: the replay reads its operation contract.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
repository=$PWD
concurrency=${CONCURRENCY:-16}
duration=${DURATION:-60}
version=$(sed -n 's/^pub const CHART_VERSION: &str = "\([^"]*\)";/\1/p' crates/control/lib/src/release_chart.rs)
test -n "$version"
umask 077
work=$(mktemp -d "${TMPDIR:-/var/tmp}/gcp-pg.XXXXXXXX")
output="$repository/tests/bench/gcp-pg-$version.json"
if ((concurrency != 16 || duration != 60)); then output="$work/gcp-pg-$version.json"; fi
if [[ -e $output ]]; then
  echo "Refusing to overwrite $output" >&2
  exit 1
fi
echo "Run directory: $work"
git rev-parse HEAD >"$work/revision"
contract=apps/target/wamn/wamn_receiving/contracts/purchase_order/update.operation.json
operation=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["operation"])' "$contract")

PGPASSWORD=$(kubectl -n platform get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
export PGPASSWORD
# A TLS handshake through kubectl port-forward ends the forward, so every
# forwarded URL disables TLS.
database_url="postgresql://postgres@127.0.0.1:15432/wamn_pg_floor?sslmode=disable"
client_url="postgresql://postgres@wamn-pg-rw.platform.svc:5432/wamn_pg_floor"
cleanup() {
  kubectl -n platform delete job pg-floor-client --ignore-not-found --wait=false >/dev/null
}
trap cleanup EXIT

# The floor database carries the statement statistics of local.sh. The
# environment databases keep the Cluster's track=top.
psql "postgresql://postgres@127.0.0.1:15432/postgres?sslmode=disable" -X -v ON_ERROR_STOP=1 >"$work/database.log" <<'SQL'
DROP DATABASE IF EXISTS wamn_pg_floor WITH (FORCE);
CREATE DATABASE wamn_pg_floor;
ALTER DATABASE wamn_pg_floor SET pg_stat_statements.track = 'all';
ALTER DATABASE wamn_pg_floor SET pg_stat_statements.track_planning = on;
ALTER DATABASE wamn_pg_floor SET track_io_timing = on;
ALTER DATABASE wamn_pg_floor SET track_wal_io_timing = on;
ALTER DATABASE wamn_pg_floor SET log_transaction_sample_rate = 0.01;
ALTER DATABASE wamn_pg_floor SET log_min_duration_sample = 0;
ALTER DATABASE wamn_pg_floor SET log_statement_sample_rate = 0.01;
SQL
"$CARGO_TARGET_DIR/debug/examples/pg_floor_database" \
  "postgresql://postgres:$PGPASSWORD@127.0.0.1:15432/wamn_pg_floor?sslmode=disable" \
  "$repository/apps/wamn_receiving" >>"$work/database.log" 2>&1 || {
    cat "$work/database.log" >&2
    exit 1
  }
source tools/bench/db-profile.sh
# The pod logs of wamn-pg-1 hold the auto_explain plans.
stop_database_profile() {
  local call=$1
  stop_wait_sampler
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 \
    -f tools/bench/database-snapshot.sql >"$work/$call.database-after.json"
  psql "$database_url" -X --csv -v ON_ERROR_STOP=1 -c \
    'SELECT s.*, r.rolname FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC' \
    >"$work/$call.statements.csv"
  kubectl -n platform logs wamn-pg-1 -c postgres >"$work/postgres.log" 2>&1
  python3 tools/bench/database-delta.py "$work" "$call"
}
seed_receiving 1000
psql "$database_url" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements' >"$work/extension.log" 2>&1

kubectl apply -f tools/bench/pg-floor/gcp-client.yaml >/dev/null
until [[ -n $(kubectl -n platform get pod -l job-name=pg-floor-client -o name) ]]; do sleep 2; done
kubectl -n platform wait pod -l job-name=pg-floor-client --for=condition=Ready --timeout=600s >/dev/null
client=$(kubectl -n platform get pod -l job-name=pg-floor-client -o jsonpath='{.items[0].metadata.name}')
kubectl -n platform get pod "$client" -o jsonpath='{.spec.nodeName}' >"$work/client-node"
kubectl -n platform exec "$client" -- mkdir -p /tmp/floor
for script in get update; do
  kubectl -n platform cp "tools/bench/pg-floor/$script.sql" "$client:/tmp/floor/$script.sql"
done
kubectl -n platform exec "$client" -- pgbench "$client_url" -i -s 10 >"$work/pgbench-init.log" 2>&1

for call in select-only simple-update get update update-no-triggers; do
  echo "Postgres floor: $call (c$concurrency, ${duration}s)"
  # Each replay claims its own request IDs.
  run=$(date +%s)
  url=$client_url
  case "$call" in
    select-only|simple-update) script=(-b "$call") ;;
    *)
      script=(-f "/tmp/floor/${call%-no-triggers}.sql")
      url="$client_url?options=-csearch_path%3Dreceiving,public%20-cstatement_timeout%3D5000%20-crole%3Dwamn_app"
      ;;
  esac
  if [[ $call == update-no-triggers ]]; then
    psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c \
      'ALTER TABLE receiving.purchase_order DISABLE TRIGGER wamn_record_history_stamp, DISABLE TRIGGER wamn_record_history_log' \
      >"$work/$call.triggers.log"
  fi
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c 'SELECT pg_stat_statements_reset()' >"$work/$call.reset.log"
  start_database_profile "$call"
  kubectl -n platform exec "$client" -- pgbench "$url" -n -M prepared -c "$concurrency" -j 2 \
    -T "$duration" -r -l --log-prefix="/tmp/floor/$call.latencies" \
    -D clients="$concurrency" -D sequence=0 -D run="$run" \
    -D operation="$operation" -D claim_operation="${operation%@*}" \
    -D statement_timeout=5000 -D schema=receiving \
    -D principal=770df186-ac15-579e-b46b-c297cae2011b "${script[@]}" >"$work/$call.log" 2>&1 || {
      cat "$work/$call.log" >&2
      exit 1
    }
  stop_database_profile "$call"
  if [[ $call == update-no-triggers ]]; then
    psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c \
      'ALTER TABLE receiving.purchase_order ENABLE TRIGGER wamn_record_history_stamp, ENABLE TRIGGER wamn_record_history_log' \
      >>"$work/$call.triggers.log"
  fi
done
kubectl -n platform exec "$client" -- tar -C /tmp/floor -cf - . | tar -C "$work" --wildcards -xf - './*.latencies.*'
psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c \
  'SELECT sum(row_version - 1), (SELECT count(*) FROM app_system.write_log WHERE result IS NOT NULL) FROM receiving.purchase_order' \
  >"$work/update-counts"
python3 -B tools/bench/pg-floor/result.py "$work" "$output" "$concurrency"
