# Sourced by local.sh after its pinned PostgreSQL container is ready.
command -v pgbench >/dev/null
(( concurrency <= 1000 )) || { echo "The 1000-order seed needs at most 1000 clients" >&2; exit 2; }
cargo build --offline --release -p wamn-control --example pg_floor_database
database_url="postgresql://postgres:probe@127.0.0.1:$pg_port/wamn_system"
"$bin/examples/pg_floor_database" "$database_url" "$repository/apps/wamn_receiving" >"$work/database.log" 2>&1 || {
  cat "$work/database.log" >&2
  exit 1
}
seed_receiving 1000
psql "$database_url" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements' >"$work/extension.log" 2>&1
pgbench "$database_url" -i -s 10 >"$work/pgbench-init.log" 2>&1
operation=$(python3 -c 'import json; print(json.load(open("apps/target/wamn/wamn_receiving/contracts/purchase_order/update.operation.json"))["operation"])')
run=$(date +%s)
for call in select-only simple-update get update; do
  echo "Postgres floor: $call (c$concurrency, ${duration}s)"
  url=$database_url
  case "$call" in
    select-only|simple-update) script=(-b "$call") ;;
    *)
      script=(-f "$repository/tools/bench/pg-floor/$call.sql")
      url="$database_url?options=-csearch_path%3Dreceiving,public%20-cstatement_timeout%3D5000%20-crole%3Dwamn_app"
      ;;
  esac
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c 'SELECT pg_stat_statements_reset()' >"$work/$call.reset.log"
  start_database_profile "$call"
  taskset -c 6-7 pgbench "$url" -n -M prepared -c "$concurrency" -j 2 \
    -T "$duration" -r -l --log-prefix="$work/$call.latencies" \
    -D clients="$concurrency" -D sequence=0 -D run="$run" \
    -D operation="$operation" -D claim_operation="${operation%@*}" \
    -D statement_timeout=5000 -D schema=receiving \
    -D principal=770df186-ac15-579e-b46b-c297cae2011b "${script[@]}" >"$work/$call.log" 2>&1 || {
      cat "$work/$call.log" >&2
      exit 1
    }
  stop_database_profile "$call"
done
psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c \
  'SELECT sum(row_version - 1), (SELECT count(*) FROM platform.write_log WHERE result IS NOT NULL) FROM receiving.purchase_order' \
  >"$work/update-counts"
python3 -B tools/bench/pg-floor/result.py "$work" "$output" "$concurrency"
