# Sourced by local.sh. The caller owns database_url, work, and prefix.
wait_pid=
start_database_profile() {
  local call=$1
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 \
    -f tools/bench/database-snapshot.sql >"$work/$call.database-before.json"
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 >"$work/$call.wait-events.log" 2>&1 <<'SQL' &
SELECT json_build_object('at', clock_timestamp(), 'sessions', coalesce(json_agg(json_build_object('pid', pid, 'user', usename, 'backend_type', backend_type, 'state', state, 'wait_event_type', wait_event_type, 'wait_event', wait_event, 'query_id', query_id, 'query', query)), '[]')) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid();
\watch 0.1
SQL
  wait_pid=$!
}
stop_wait_sampler() {
  if [[ -n $wait_pid ]]; then
    kill -TERM "$wait_pid" 2>/dev/null || true
    wait "$wait_pid" || true
    wait_pid=
  fi
}
stop_database_profile() {
  local call=$1
  stop_wait_sampler
  psql "$database_url" -XAt -v ON_ERROR_STOP=1 \
    -f tools/bench/database-snapshot.sql >"$work/$call.database-after.json"
  psql "$database_url" -X --csv -v ON_ERROR_STOP=1 -c \
    'SELECT s.*, r.rolname FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC' \
    >"$work/$call.statements.csv"
  docker logs --timestamps "$prefix-postgres" >"$work/postgres.log" 2>&1
  python3 tools/bench/database-delta.py "$work" "$call"
}
seed_receiving() {
  psql "$database_url" -X -v ON_ERROR_STOP=1 -v scale="$1" \
    -c 'SET auto_explain.sample_rate=0; SET log_transaction_sample_rate=0; SET log_statement_sample_rate=0' \
    -f apps/wamn_receiving/tests/fixtures/receiving-seed.sql >"$work/seed.log" 2>&1
}
