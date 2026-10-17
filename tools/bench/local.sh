#!/usr/bin/env bash
# Local Receiving timing requires release binaries and release guests.
set -euo pipefail
cd "$(dirname "$0")/../.."
repository=$PWD
duration=60
concurrency=16
smoke=false
profile=false
while (($#)); do
  case "$1" in
    --profile) profile=true; shift ;;
    --duration-secs|--concurrency)
      [[ ${2:-} =~ ^[1-9][0-9]*$ ]] || { echo "$1 needs a positive integer" >&2; exit 2; }
      if [[ $1 == --duration-secs ]]; then duration=$2; else concurrency=$2; fi
      smoke=true
      shift 2
      ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
if $profile; then
  if (( $(</proc/sys/kernel/perf_event_paranoid) > 1 )); then
    echo 'run `sudo sysctl kernel.perf_event_paranoid=1` first' >&2
    exit 1
  fi
  command -v perf >/dev/null
  for tool in "$HOME/.cargo/bin/inferno-collapse-perf" "$HOME/.cargo/bin/inferno-flamegraph"; do
    [[ -x $tool ]] || { echo "Missing executable: $tool" >&2; exit 1; }
  done
fi
version=$(sed -n 's/^pub const CHART_VERSION: &str = "\([^"]*\)";/\1/p' crates/control/lib/src/release_chart.rs)
test -n "$version"
output="tests/bench/local-$version.json"
flamegraph="tests/bench/flamegraph-local-get-c$concurrency-$version.svg"
if ! $smoke && [[ -e $output ]]; then
  echo "Refusing to overwrite $output" >&2
  exit 1
fi
if ! $smoke && $profile && [[ -e $flamegraph ]]; then
  echo "Refusing to overwrite $flamegraph" >&2
  exit 1
fi
export CARGO_TARGET_DIR="$repository/target"
export TMPDIR=${TMPDIR:-/var/tmp}
umask 077
work=$(mktemp -d "$TMPDIR/wamn-lbench.XXXXXXXX")
root="$work/env"
mkdir "$root"
if $smoke; then
  output="$work/local-$version.json"
  flamegraph="$work/flamegraph-local-get-c$concurrency-$version.svg"
fi
bin="$CARGO_TARGET_DIR/release"
containers=()
dev_pid=
perf_pid=
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n $perf_pid ]]; then
    kill -INT "$perf_pid" 2>/dev/null || true
    wait "$perf_pid" || true
  fi
  if [[ -n $dev_pid ]]; then
    kill -TERM "$dev_pid" 2>/dev/null || true
    wait "$dev_pid" || true
  fi
  if [[ -f $root/identity-process.json ]]; then
    "$bin/wamn" dev down --root "$root" >>"$work/down.log" 2>&1 || {
      cat "$work/down.log" >&2
      status=1
    }
  fi
  if ((${#containers[@]})); then
    docker rm -fv "${containers[@]}" >/dev/null || status=1
  fi
  echo "Local run files: $work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Use only cached images. Never contact a registry.
for image in postgres:18 nats:2.10-alpine grafana/tempo:2.6.1; do
  docker image inspect "$image" >/dev/null
done
cargo build --offline --release -p wamn-ctl --bin wamn \
  -p wamn-host --bin wamn-host -p wamn-identity --bin wamn-identity \
  -p wamn-cdc-reader --bin wamn-cdc-reader -p wamn-bench --bin wamn-bench
for guest in http-route materializer; do
  RUSTFLAGS="$(tools/guest-rustflags)" cargo build --offline --release \
    --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p "$guest"
done

# Choose distinct free loopback ports before publishing each explicit mapping.
read -r pg_port scheduler_port event_port tempo_port otlp_port < <(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(5)]
for sock in sockets:
    sock.bind(("127.0.0.1", 0))
print(*(sock.getsockname()[1] for sock in sockets))
PY
)
prefix="wamn-lbench-${work##*.}"
start_container() {
  local id
  id=$(docker run --pull=never -d "$@")
  containers+=("$id")
}
start_container --name "$prefix-postgres" --cpuset-cpus 4-5 \
  -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_system \
  -p "127.0.0.1:$pg_port:5432" postgres:18 \
  -c wal_level=logical -c shared_preload_libraries=pg_stat_statements
start_container --name "$prefix-scheduler" -p "127.0.0.1:$scheduler_port:4222" \
  nats:2.10-alpine --jetstream --store_dir=/data --http_port=8222

# The disposable event broker has separate logins for each development role.
printf '%s' probe >"$work/event-password"
cat >"$work/events.conf" <<'NATS'
port: 4222
http_port: 8222
jetstream { store_dir: /data }
authorization {
  users: [
    {user: runtime, password: probe},
    {user: provisioning, password: probe},
    {user: publisher, password: probe},
    {user: materializer, password: probe}
  ]
}
NATS
start_container --name "$prefix-events" -p "127.0.0.1:$event_port:4222" \
  --mount "type=bind,src=$work/events.conf,dst=/etc/nats/nats.conf,readonly" \
  nats:2.10-alpine --config=/etc/nats/nats.conf
start_container --name "$prefix-tempo" \
  -p "127.0.0.1:$tempo_port:3200" -p "127.0.0.1:$otlp_port:4317" \
  --mount "type=bind,src=$repository/test-support/infrastructure/tempo-dev.yaml,dst=/etc/tempo/tempo.yaml,readonly" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
ready=false
for ((attempt=0; attempt<60; attempt++)); do
  if docker exec "$prefix-postgres" sh -c \
      'test "$(cat /proc/1/comm)" = postgres && pg_isready -h 127.0.0.1 -U postgres -d wamn_system' >/dev/null 2>&1 \
    && docker exec "$prefix-scheduler" wget -qO- http://127.0.0.1:8222/healthz >/dev/null 2>&1 \
    && docker exec "$prefix-events" wget -qO- http://127.0.0.1:8222/healthz >/dev/null 2>&1 \
    && curl -fsS "http://127.0.0.1:$tempo_port/ready" >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
if ! $ready; then
  for container in "${containers[@]}"; do docker logs "$container" >&2; done
  echo 'Local services did not become ready' >&2
  exit 1
fi

# Activation clears its environment. Export limits inside the host wrapper,
# reading their values from the deployment that owns them.
python3 - "$bin/wamn-host" "$work/host.pid" >"$work/host" <<'PY'
import pathlib
import re
import shlex
import sys

deployment = pathlib.Path("deploy/gcp/host-group-receiving.yaml").read_text()
print("#!/bin/bash")
print("set -euo pipefail")
for name in ("WAMN_COMPONENT_POOL_SIZE", "WAMN_PG_GUEST_POOL_MAX",
             "WAMN_PG_PLATFORM_POOL_MAX", "WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT"):
    value, = re.findall(r"^- name: " + name + r"\n  value: '([0-9]+)'$", deployment, re.M)
    print(f"export {name}={value}")
print(f'echo "$$" > {shlex.quote(sys.argv[2])}')
print(f'exec /usr/bin/taskset -c 0-3 {shlex.quote(sys.argv[1])} "$@"')
PY
chmod +x "$work/host"
"$bin/wamn" dev up --package "$repository/apps/wamn_receiving" \
  --system-database-url "postgresql://postgres:probe@127.0.0.1:$pg_port/wamn_system" \
  --root "$root" --nats-url "nats://127.0.0.1:$scheduler_port" \
  --event-nats-url "nats://127.0.0.1:$event_port" \
  --event-nats-username runtime --event-nats-password-file "$work/event-password" \
  --event-provisioning-username provisioning --event-provisioning-password-file "$work/event-password" \
  --event-publisher-username publisher --event-publisher-password-file "$work/event-password" \
  --event-materializer-username materializer --event-materializer-password-file "$work/event-password" \
  --stream-replicas 1 --dup-window-secs 120 \
  --tempo-query-url "http://127.0.0.1:$tempo_port" \
  --otel-exporter-otlp-endpoint "http://127.0.0.1:$otlp_port" \
  --route-host receiving.localhost --platform-domain example.invalid \
  --host-binary "$work/host" --cdc-reader-binary "$bin/wamn-cdc-reader" \
  --flow-http-component "$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  --materializer-component "$CARGO_TARGET_DIR/wasm32-wasip2/release/materializer.wasm" \
  >"$work/up.log" 2>&1 || { cat "$work/up.log" >&2; exit 1; }
"$bin/wamn" dev --config "$root/dev.json" \
  --overlay-root "$repository/apps/wamn_receiving" --hold >"$work/dev.log" 2>&1 &
dev_pid=$!
served=
for ((attempt=0; attempt<900; attempt++)); do
  served=$(sed -n 's/^run served: \([^ ]*\) host=\([^ ]*\) .*/\1 \2/p' "$work/dev.log")
  [[ -z $served ]] || break
  kill -0 "$dev_pid" 2>/dev/null || break
  sleep 1
done
if [[ -z $served ]]; then
  cat "$work/dev.log" >&2
  echo 'Development host did not serve' >&2
  exit 1
fi
read -r served_url route_host <<<"$served"
database_url=$(python3 - "$root/dev.json" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1]))["target_database_url"])
PY
)
sql() { psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c "$1"; }
psql "$database_url" -X -v ON_ERROR_STOP=1 -v scale=1000 \
  -f apps/wamn_receiving/tests/fixtures/receiving-seed.sql >"$work/seed.log" 2>&1
sql 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements' >/dev/null
for call in get update; do
  sql 'SELECT pg_stat_statements_reset(0, (SELECT oid FROM pg_database WHERE datname = current_database()))' >/dev/null
  if $profile && [[ $call == get ]]; then
    host_pid=$(<"$work/host.pid")
    perf record -F 99 --call-graph dwarf -p "$host_pid" \
      -o "$work/get.perf.data" -- sleep "$duration" >"$work/perf.log" 2>&1 &
    perf_pid=$!
  fi
  taskset -c 6-7 "$bin/wamn-bench" --base-url "$served_url" --host "$route_host" \
    --pat-file "$root/operator-pat.json" --call "$call" --concurrency "$concurrency" \
    --duration-secs "$duration" --tier local --db-url "$database_url" \
    --output "$work/$call.json" >"$work/$call.log" 2>&1 || { cat "$work/$call.log" >&2; exit 1; }
  if [[ -n $perf_pid ]]; then
    wait "$perf_pid" || { cat "$work/perf.log" >&2; exit 1; }
    perf_pid=
    (
      set -o noclobber
      perf script -i "$work/get.perf.data" | "$HOME/.cargo/bin/inferno-collapse-perf" \
        | "$HOME/.cargo/bin/inferno-flamegraph" >"$flamegraph"
    )
    echo "$flamegraph"
  fi
  sql 'SELECT calls FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())' \
    >"$work/$call.pgss"
done
python3 - "$work" "$output" "$concurrency" <<'PY'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
result = {"concurrency": int(sys.argv[3])}
for call in ("get", "update"):
    run = json.loads((work / f"{call}.json").read_text())
    result[call] = {key: run[key] for key in (
        "throughput_per_sec", "p50_ms", "p95_ms", "p99_ms", "calls", "errors"
    )}
    # Match tier.sh: round each ratio to exclude setup and background polling.
    result[call]["pg_stat_statements_calls_per_request"] = sum(
        round(int(calls) / run["calls"])
        for calls in (work / f"{call}.pgss").read_text().splitlines()
    )
with open(sys.argv[2], "x") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
print("local: " + "; ".join(
    f"{call} {run['throughput_per_sec']:.1f}/s p50={run['p50_ms']:.3f}ms "
    f"errors={run['errors']} statements/request={run['pg_stat_statements_calls_per_request']}"
    for call, run in result.items() if call != "concurrency"
) + f"; {sys.argv[2]}")
PY
