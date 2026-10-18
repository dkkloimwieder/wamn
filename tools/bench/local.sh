#!/usr/bin/env bash
# Local Receiving timing requires release binaries and release guests.
set -euo pipefail
cd "$(dirname "$0")/../.."
repository=$PWD
duration=60
concurrency=16
smoke=false
profile=false
edge=true
pg_floor=false
app_path=false
while (($#)); do
  case "$1" in
    --app-path) app_path=true; shift ;;
    --pg-floor) pg_floor=true; shift ;;
    --profile) profile=true; shift ;;
    --no-edge) edge=false; shift ;;
    --duration-secs|--concurrency)
      [[ ${2:-} =~ ^[1-9][0-9]*$ ]] || { echo "$1 needs a positive integer" >&2; exit 2; }
      if [[ $1 == --duration-secs ]]; then duration=$2; else concurrency=$2; fi
      smoke=true
      shift 2
      ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
if $app_path; then
  if $pg_floor; then
    echo "--app-path cannot use --pg-floor" >&2
    exit 2
  fi
  edge=false
fi
if $pg_floor && $profile; then
  echo "--pg-floor cannot use --profile" >&2
  exit 2
fi
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
result_name="local-$version.json"
if $pg_floor; then result_name="local-pg-$version.json"; fi
if $app_path; then result_name="local-app-$version.json"; fi
output="tests/bench/$result_name"
profile_call=get
if $app_path; then profile_call=app-path; fi
flamegraph="tests/bench/flamegraph-local-$profile_call-c$concurrency-$version.svg"
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
  output="$work/$result_name"
  flamegraph="$work/flamegraph-local-$profile_call-c$concurrency-$version.svg"
fi
bin="$CARGO_TARGET_DIR/release"
containers=()
dev_pid=
perf_pid=
stop_dev() {
  if [[ -n $dev_pid ]]; then
    kill -TERM "$dev_pid" 2>/dev/null || true
    wait "$dev_pid" || true
    dev_pid=
  fi
  if [[ -f $root/identity-process.json ]]; then
    "$bin/wamn" dev down --root "$root" >>"$work/down.log" 2>&1 || {
      cat "$work/down.log" >&2
      return 1
    }
  fi
}
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n $perf_pid ]]; then
    kill -INT "$perf_pid" 2>/dev/null || true
    wait "$perf_pid" || true
  fi
  stop_dev || status=1
  if ((${#containers[@]})); then
    docker rm -fv "${containers[@]}" >/dev/null || status=1
  fi
  echo "Local run files: $work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Choose distinct free loopback ports before publishing each explicit mapping.
read -r pg_port scheduler_port event_port otlp_port < <(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(4)]
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
docker image inspect postgres:18 >/dev/null
python3 - "$work" <<'PARAMETERS'
import pathlib
import re
import sys
work = pathlib.Path(sys.argv[1])
postgres = pathlib.Path("deploy/gcp/cnpg-cluster.yaml").read_text()
parameters = []
for name in ("max_slot_wal_keep_size", "pg_stat_statements.max", "pg_stat_statements.track"):
    value, = re.findall(r'^      ' + re.escape(name) + r': "([^"]+)"$', postgres, re.M)
    parameters.extend(("-c", f"{name}={value}"))
(work / "postgres-parameters").write_text("\n".join(parameters) + "\n")
PARAMETERS
mapfile -t postgres_parameters <"$work/postgres-parameters"
start_container --name "$prefix-postgres" --cpuset-cpus 4-5 \
  -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_system \
  -p "127.0.0.1:$pg_port:5432" postgres:18 \
  -c wal_level=logical -c shared_preload_libraries=pg_stat_statements "${postgres_parameters[@]}"
ready=false
for ((attempt=0; attempt<60; attempt++)); do
  if docker exec "$prefix-postgres" sh -c \
      'test "$(cat /proc/1/comm)" = postgres && pg_isready -h 127.0.0.1 -U postgres -d wamn_system' >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
if ! $ready; then
  docker logs "$prefix-postgres" >&2
  exit 1
fi
if $pg_floor; then
  source tools/bench/pg-floor/run.sh
  exit
fi

# Use only cached images. Never contact a registry.
if $edge; then
  command -v helm >/dev/null
  docker image inspect nginx:1.27-alpine >/dev/null
fi
collector_image=$(tools/dev-collector "$root")
for image in postgres:18 nats:2.10-alpine "$collector_image"; do
  docker image inspect "$image" >/dev/null
done
# Match the Dockerfile host build without unifying control-tool features.
cargo build --offline --locked --release -p wamn-host -p wamn-runtime \
  --bin wamn-host --bin docker-credential-wamn
cargo build --offline --release -p wamn-ctl --bin wamn \
  -p wamn-identity --bin wamn-identity \
  -p wamn-cdc-reader --bin wamn-cdc-reader -p wamn-bench --bin wamn-bench
cargo build --offline --release -p wamn-test-infrastructure --example event_broker_files
for guest in http-route materializer; do
  RUSTFLAGS="$(tools/guest-rustflags)" cargo build --offline --release \
    --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p "$guest"
done
http_route_sha256=$(sha256sum "$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" | cut -d ' ' -f 1)
materializer_sha256=$(sha256sum "$CARGO_TARGET_DIR/wasm32-wasip2/release/materializer.wasm" | cut -d ' ' -f 1)
echo "http_route_sha256=$http_route_sha256"
echo "materializer_sha256=$materializer_sha256"

# Use the deployed permission generator with the development identity's owner.
read -r org project environment tenant < <(python3 - <<'IDENTITY'
import pathlib
import re

source = pathlib.Path("crates/control/lib/src/dev/environment.rs").read_text()
values = []
for name in ("ORG", "PROJECT", "ENVIRONMENT", "TENANT"):
    value, = re.findall(r'^pub const ' + name + r': &str = "([^"]+)";$', source, re.M)
    values.append(value)
print(*values)
IDENTITY
)
"$bin/examples/event_broker_files" "$work" "nats://127.0.0.1:$event_port" \
  "$org" "$project" "$environment" "$tenant" 1 \
  "$repository/apps/target/wamn/wamn_receiving/wamn.json" >"$work/event-broker.log"
event_memory=$(python3 - "$work" "$prefix-events" <<'CONFIGURATION'
import json
import pathlib
import re
import sys
import textwrap

work = pathlib.Path(sys.argv[1])
deployment = pathlib.Path("deploy/gcp/nats-jetstream.yaml").read_text()
configuration, = re.findall(r"^  nats.conf: \|\n(.*?)(?=^---$)", deployment, re.M | re.S)
configuration = textwrap.dedent(configuration).replace("$POD_NAME", sys.argv[2])
(work / "events.conf").write_text(configuration)

# The local provisioner also creates the shared tap, using its deployed rights.
authorization_path = work / "event-nats/authorization.conf"
authorization = json.loads(authorization_path.read_text().split(":", 1)[1])
users = authorization["users"]
tap_admin = next(user for user in users if user["user"] == "tap-admin")
provisioner = next(user for user in users if user["user"].startswith("provisioning_"))
provisioner["permissions"]["publish"]["allow"].extend(
    subject for subject in tap_admin["permissions"]["publish"]["allow"]
    if subject.startswith("$JS.API.STREAM.")
)
authorization["users"].remove(tap_admin)
authorization_path.write_text("authorization: " + json.dumps(authorization) + "\n")

broker, = (document for document in deployment.split("\n---\n") if "\nkind: StatefulSet\n" in document)
memory, = re.findall(r'^            limits: \{ memory: "([^"]+)" \}$', broker, re.M)
amount, unit = re.fullmatch(r"([0-9]+)(Ki|Mi|Gi)", memory).groups()
print(int(amount) * {"Ki": 1024, "Mi": 1024**2, "Gi": 1024**3}[unit])
CONFIGURATION
)
start_container --name "$prefix-scheduler" -p "127.0.0.1:$scheduler_port:4222" \
  nats:2.10-alpine --jetstream --store_dir=/data --http_port=8222
start_container --name "$prefix-events" --memory "$event_memory" -p "127.0.0.1:$event_port:4222" \
  --mount "type=bind,src=$work/events.conf,dst=/etc/nats-config/nats.conf,readonly" \
  --mount "type=bind,src=$work/event-nats/authorization.conf,dst=/etc/nats-authorization/authorization.conf,readonly" \
  nats:2.10-alpine --config=/etc/nats-config/nats.conf
start_container --name "$prefix-collector" \
  --user "$(id -u):$(id -g)" \
  -p "127.0.0.1:$otlp_port:4317" \
  --env "CLUSTER=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["services"]["receiving-dev-collector"]["environment"]["CLUSTER"])' "$root/local-artifacts/otel/compose.json")" \
  --mount "type=bind,src=$root/local-artifacts/otel,dst=/var/lib/otelcol" \
  "$collector_image" --config=/var/lib/otelcol/config.yaml
ready=false
for ((attempt=0; attempt<60; attempt++)); do
  if docker exec "$prefix-postgres" sh -c \
      'test "$(cat /proc/1/comm)" = postgres && pg_isready -h 127.0.0.1 -U postgres -d wamn_system' >/dev/null 2>&1 \
    && docker exec "$prefix-scheduler" wget -qO- http://127.0.0.1:8222/healthz >/dev/null 2>&1 \
    && docker exec "$prefix-events" wget -qO- http://127.0.0.1:8222/healthz >/dev/null 2>&1 \
    && (echo >"/dev/tcp/127.0.0.1/$otlp_port") 2>/dev/null; then
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

# Keep CPU pinning here; activation owns host settings.
python3 - "$bin/wamn-host" "$work/host.pid" >"$work/host" <<'PY'
import shlex
import sys

print("#!/bin/bash")
print("set -euo pipefail")
print(f'echo "$$" > {shlex.quote(sys.argv[2])}')
print(f'exec /usr/bin/taskset -c 0-3 {shlex.quote(sys.argv[1])} "$@"')
PY
chmod +x "$work/host"
"$bin/wamn" dev up --package "$repository/apps/wamn_receiving" \
  --system-database-url "postgresql://postgres:probe@127.0.0.1:$pg_port/wamn_system" \
  --root "$root" --nats-url "nats://127.0.0.1:$scheduler_port" \
  --event-nats-url "nats://127.0.0.1:$event_port" \
  --event-nats-username "$(<"$work/event-nats/runtime-username")" --event-nats-password-file "$work/event-nats/runtime-password" \
  --event-provisioning-username "$(<"$work/event-nats/provisioning-username")" --event-provisioning-password-file "$work/event-nats/provisioning-password" \
  --event-publisher-username "$(<"$work/event-nats/publisher-username")" --event-publisher-password-file "$work/event-nats/publisher-password" \
  --event-materializer-username "$(<"$work/event-nats/materializer-username")" --event-materializer-password-file "$work/event-nats/materializer-password" \
  --stream-replicas 1 --dup-window-secs 120 \
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
bench_url=$served_url
if $edge; then
  read -r edge_port identity_ca < <(python3 - "$root/dev.json" "$work/edge-values.json" "$served_url" "$route_host" <<'PY'
import json
import socket
import sys

configuration = json.load(open(sys.argv[1]))
identity = configuration["session_identity"]
with open(sys.argv[2], "w") as output:
    json.dump({
        "applications": [{"host": sys.argv[4], "api": sys.argv[3], "bucketPath": "local"}],
        "identity": identity["issuer"],
        "tlsSecret": "local",
        "identityCaConfigMap": "local",
        "neg": True,
    }, output)
with socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    print(listener.getsockname()[1], identity["ca"])
PY
  )
  helm template "local-edge-1" deploy/platform/edge \
    --show-only templates/configmap.yaml -f "$work/edge-values.json" >"$work/edge.yaml"
  python3 - "$work/edge.yaml" "$work/nginx.conf" "$edge_port" <<'PY'
import pathlib
import sys
import textwrap

rendered = pathlib.Path(sys.argv[1]).read_text()
configuration = textwrap.dedent(rendered.split("  nginx.conf: |\n", 1)[1])
# Only the public listener differs locally. Routing remains chart-owned.
assert configuration.count("listen 8443 ssl;") == 1
configuration = configuration.replace("listen 8443 ssl;", f"listen 127.0.0.1:{sys.argv[3]};")
configuration = "\n".join(line for line in configuration.splitlines()
    if not line.strip().startswith(("ssl_certificate ", "ssl_certificate_key "))) + "\n"
pathlib.Path(sys.argv[2]).write_text(configuration)
PY
  # Host networking lets nginx reach the host's loopback-only listener.
  start_container --name "$prefix-edge" --network host \
    --mount "type=bind,src=$work/nginx.conf,dst=/etc/nginx/nginx.conf,readonly" \
    --mount "type=bind,src=$identity_ca,dst=/etc/wamn-edge/identity-ca/ca.crt,readonly" \
    nginx:1.27-alpine
  ready=false
  for ((attempt=0; attempt<30; attempt++)); do
    if curl -fsS -H "Host: $route_host" "http://127.0.0.1:$edge_port/healthz" >/dev/null 2>&1; then
      ready=true
      break
    fi
    sleep 1
  done
  if ! $ready; then
    docker logs "$prefix-edge" >&2
    echo 'Local edge did not become ready' >&2
    exit 1
  fi
  bench_url="http://127.0.0.1:$edge_port/api"
fi

database_url=$(python3 - "$root/dev.json" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1]))["target_database_url"])
PY
)
sql() { psql "$database_url" -XAt -v ON_ERROR_STOP=1 -c "$1"; }
seed_scale=1000
if $app_path; then seed_scale=1; fi
psql "$database_url" -X -v ON_ERROR_STOP=1 -v scale="$seed_scale" \
  -f apps/wamn_receiving/tests/fixtures/receiving-seed.sql >"$work/seed.log" 2>&1
sql 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements' >/dev/null
calls=(get update)
if $app_path; then calls=(app-path); fi
for call in "${calls[@]}"; do
  sql 'SELECT pg_stat_statements_reset(0, (SELECT oid FROM pg_database WHERE datname = current_database()))' >/dev/null
  if $profile && [[ $call == "$profile_call" ]]; then
    host_pid=$(<"$work/host.pid")
    perf record -F 999 --call-graph dwarf -p "$host_pid" \
      -o "$work/$call.perf.data" -- sleep "$duration" >"$work/perf.log" 2>&1 &
    perf_pid=$!
  fi
  taskset -c 6-7 "$bin/wamn-bench" --base-url "$bench_url" --host "$route_host" \
    --pat-file "$root/operator-pat.json" --call "$call" --concurrency "$concurrency" \
    --duration-secs "$duration" --tier local --db-url "$database_url" \
    --output "$work/$call.json" >"$work/$call.log" 2>&1 || { cat "$work/$call.log" >&2; exit 1; }
  if [[ -n $perf_pid ]]; then
    wait "$perf_pid" || { cat "$work/perf.log" >&2; exit 1; }
    perf_pid=
    (
      set -o noclobber
      # Use local symbols instead of inherited remote debuginfod lookups.
      env -u DEBUGINFOD_URLS perf script -i "$work/$call.perf.data" | "$HOME/.cargo/bin/inferno-collapse-perf" \
        | "$HOME/.cargo/bin/inferno-flamegraph" >"$flamegraph"
    )
    env -u DEBUGINFOD_URLS perf report --stdio --no-children --call-graph none \
      --percent-limit 0.5 -i "$work/$call.perf.data" \
      >"$work/$call.perf-report.txt"
    echo "$flamegraph"
  fi
  sql 'SELECT calls FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())' \
    >"$work/$call.pgss"
  psql "$database_url" -X --csv -v ON_ERROR_STOP=1 -c 'SELECT query, calls, rows, total_exec_time FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())' >"$work/$call.statements.csv"
done
if $app_path; then
  # The host flushes its exporter on shutdown. Then flush the collector's batch.
  stop_dev
  docker stop "$prefix-collector" >"$work/collector-stop.log"
  python3 tools/bench/app-result.py "$work" "$output" \
    "$http_route_sha256" "$materializer_sha256" "$edge" "$seed_scale"
  exit
fi
python3 - "$work" "$output" "$concurrency" "$http_route_sha256" "$materializer_sha256" "$edge" <<'PY'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
result = {
    "concurrency": int(sys.argv[3]),
    "edge": sys.argv[6] == "true",
    "http_route_sha256": sys.argv[4],
    "materializer_sha256": sys.argv[5],
}
for call in ("get", "update"):
    run = json.loads((work / f"{call}.json").read_text())
    result[call] = {key: run[key] for key in (
        "throughput_per_sec", "p50_ms", "p95_ms", "p99_ms", "max_ms", "slow", "calls", "errors"
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
    f"p99={run['p99_ms']:.3f}ms max={run['max_ms']:.3f}ms slow={len(run['slow'])} "
    f"errors={run['errors']} statements/request={run['pg_stat_statements_calls_per_request']}"
    for call, run in result.items() if call in ("get", "update")
) + f"; {sys.argv[2]}")
PY
