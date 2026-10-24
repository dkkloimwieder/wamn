#!/usr/bin/env bash
# Local Receiving timing requires release binaries and release guests.
set -euo pipefail
cd "$(dirname "$0")/../.."
repository=$PWD
duration=60
concurrency=16
profile=false
edge=true
pg_floor=false
app_path=false
mix=false
contention=false
selected_calls=()
while (($#)); do
  case "$1" in
    --mix) mix=true; shift ;;
    --contention) contention=true; shift ;;
    --call)
      case "${2:-}" in get|update|query|receipt-query|create|record_receipt|app-path) selected_calls+=("$2") ;;
        *) echo 'Unknown --call' >&2; exit 2 ;;
      esac
      shift 2 ;;
    --app-path) app_path=true; shift ;;
    --pg-floor) pg_floor=true; shift ;;
    --profile) profile=true; shift ;;
    --no-edge) edge=false; shift ;;
    --duration-secs|--concurrency)
      [[ ${2:-} =~ ^[1-9][0-9]*$ ]] || { echo "$1 needs a positive integer" >&2; exit 2; }
      if [[ $1 == --duration-secs ]]; then duration=$2; else concurrency=$2; fi
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
    echo 'Profiling requires kernel.perf_event_paranoid <= 1' >&2
    exit 1
  fi
  command -v perf >/dev/null
  command -v wasm-tools >/dev/null
  command -v rustfilt >/dev/null
  for tool in "$HOME/.cargo/bin/inferno-collapse-perf" "$HOME/.cargo/bin/inferno-flamegraph"; do
    [[ -x $tool ]] || { echo "Missing executable: $tool" >&2; exit 1; }
  done
fi
version=$(sed -n 's/^version: //p' deploy/platform/release/Chart.yaml)
test -n "$version"
result_name="local-$version.json"
if $pg_floor; then result_name="local-pg-$version.json"; fi
if $app_path; then result_name="local-app-$version.json"; fi
export CARGO_TARGET_DIR="$repository/target"
export TMPDIR=${TMPDIR:-/var/tmp}
umask 077
mkdir -p "$TMPDIR"
work=$(mktemp -d "$TMPDIR/local.XXXXXXXX")
root="$work/env"
mkdir "$root"
output="$repository/tests/bench/$result_name"
if $app_path || $profile || $mix || $contention || ! $edge \
  || ((${#selected_calls[@]} || concurrency != 16 || duration != 60)); then
  output="$work/$result_name"
fi
if [[ -e "$output" ]]; then
  echo "Refusing to overwrite $output" >&2
  exit 1
fi
bin="$CARGO_TARGET_DIR/release"
containers=()
dev_pid=
perf_pid=
bench_pid=
source tools/bench/db-profile.sh
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
  if [[ -n $bench_pid ]]; then
    kill -TERM "$bench_pid" 2>/dev/null || true
    wait "$bench_pid" || true
  fi
  stop_wait_sampler
  if docker inspect "$prefix-postgres" >/dev/null 2>&1; then
    docker logs --timestamps "$prefix-postgres" >"$work/postgres.log" 2>&1 || status=1
  fi
  stop_dev || status=1
  if $profile && [[ -f $work/host.pid ]]; then
    host_pid=$(<"$work/host.pid")
    if [[ $(readlink "/tmp/perf-$host_pid.map") == "$work/guest.map" ]]; then
      rm -- "/tmp/perf-$host_pid.map"
    fi
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
  -c wal_level=logical -c shared_preload_libraries=pg_stat_statements,auto_explain "${postgres_parameters[@]}" \
  -c pg_stat_statements.track=all -c pg_stat_statements.track_planning=on \
  -c track_io_timing=on -c track_wal_io_timing=on \
  -c auto_explain.log_min_duration=0 -c auto_explain.log_analyze=on \
  -c auto_explain.log_buffers=on -c auto_explain.log_wal=on \
  -c auto_explain.log_nested_statements=on -c auto_explain.log_timing=off \
  -c auto_explain.sample_rate=0.01 -c auto_explain.log_format=json \
  -c log_transaction_sample_rate=0.01 -c log_min_duration_sample=0 -c log_statement_sample_rate=0.01 \
  -c 'log_line_prefix=%m [%p] user=%u db=%d xid=%v '
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
# Match the Dockerfile packages and features. Keep the profiling artifacts
# separate, and retain frame pointers without changing release optimization.
host_bin="$bin/wamn-host"
if $profile; then
  profile_target=/var/tmp/wamn-h2r2l/host-target
  CARGO_TARGET_DIR="$profile_target" CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    RUSTFLAGS="-C force-frame-pointers=yes -C link-arg=-fuse-ld=mold" \
    /usr/bin/time -f '%e' -o "$work/host-build.seconds" \
    cargo build --offline --locked --release -p wamn-host -p wamn-runtime \
      --bin wamn-host --bin docker-credential-wamn
  host_bin="$profile_target/release/wamn-host"
else
  cargo build --offline --locked --release -p wamn-host -p wamn-runtime \
    --bin wamn-host --bin docker-credential-wamn
fi
sha256sum "$host_bin" >"$work/host.sha256"
git rev-parse HEAD >"$work/revision"
git diff -- tools/bench apps/wamn_receiving crates/platform/engine/src/engine.rs >"$work/source.diff"
cargo build --offline --release -p wamn-ctl --bin wamn \
  -p wamn-identity --bin wamn-identity \
  -p wamn-cdc-reader --bin wamn-cdc-reader -p wamn-bench --bin wamn-bench
cargo build --offline --release -p wamn-test-infrastructure --example event_broker_files
guest_target=$CARGO_TARGET_DIR
guest_flags=${RUSTFLAGS:-}
if $profile; then
  guest_target="$profile_target/guests"
  guest_flags+=" -C strip=none"
fi
# The route guest must match its pin (compose_control_entry in
# crates/platform/component-composer/src/lib.rs), so it keeps no symbols in profile mode.
route_target=$CARGO_TARGET_DIR
CARGO_TARGET_DIR="$route_target" RUSTFLAGS="$(RUSTFLAGS="${RUSTFLAGS:-}" tools/guest-rustflags)" cargo build --offline --release \
  --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p http-route
CARGO_TARGET_DIR="$guest_target" RUSTFLAGS="$(RUSTFLAGS="$guest_flags" tools/guest-rustflags)" cargo build --offline --release \
  --manifest-path apps/Cargo.toml --target wasm32-wasip2 -p materializer
http_route_sha256=$(sha256sum "$route_target/wasm32-wasip2/release/http_route.wasm" | cut -d ' ' -f 1)
materializer_sha256=$(sha256sum "$guest_target/wasm32-wasip2/release/materializer.wasm" | cut -d ' ' -f 1)
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
DATABASE_URL="postgresql://postgres:probe@127.0.0.1:$pg_port/wamn_system" \
  "$bin/wamn" build "$repository/apps/wamn_receiving" >"$work/build.log" 2>&1 || {
    cat "$work/build.log" >&2
    exit 1
  }
"$bin/examples/event_broker_files" "$work" "nats://127.0.0.1:$event_port" \
  "$org" "$project" "$environment" "$tenant" 1 \
  "$repository/apps/target/wamn/wamn_receiving/wamn.json" >"$work/event-broker.log"
event_memory=$(python3 - "$work" "$prefix-events" <<'CONFIGURATION'
import pathlib
import re
import sys
import textwrap

work = pathlib.Path(sys.argv[1])
deployment = pathlib.Path("deploy/gcp/nats-jetstream.yaml").read_text()
configuration, = re.findall(r"^  nats.conf: \|\n(.*?)(?=^---$)", deployment, re.M | re.S)
configuration = textwrap.dedent(configuration).replace("$POD_NAME", sys.argv[2])
(work / "events.conf").write_text(configuration)

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

# Keep CPU pinning here; activation reads the release chart's runtime.env,
# including all three WASMTIME_POOLING_*_KEEP_RESIDENT values.
python3 - "$host_bin" "$work/host.pid" "$profile" "$work/guest.map" >"$work/host" <<'PY'
import shlex
import sys

print("#!/bin/bash")
print("set -euo pipefail")
print(f'echo "$$" > {shlex.quote(sys.argv[2])}')
if sys.argv[3] == "true":
    print(f'ln -s {shlex.quote(sys.argv[4])} "/tmp/perf-$$.map"')
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
  --flow-http-component "$route_target/wasm32-wasip2/release/http_route.wasm" \
  --materializer-component "$guest_target/wasm32-wasip2/release/materializer.wasm" \
  >"$work/up.log" 2>&1 || { cat "$work/up.log" >&2; exit 1; }
CARGO_TARGET_DIR="$guest_target" RUSTFLAGS="$guest_flags" "$bin/wamn" dev --config "$root/dev.json" \
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
        "web": "",
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
# Print the host's utime + stime in clock ticks, fields 14 and 15 of its stat.
host_cpu_ticks() {
  local stat fields
  stat=$(<"/proc/$(<"$work/host.pid")/stat")
  read -ra fields <<<"${stat##*) }"
  echo $((fields[11] + fields[12]))
}
seed_scale=1000
if $app_path; then seed_scale=1; fi
seed_receiving "$seed_scale"
sql 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements' >/dev/null
calls=(get update query record_receipt mix)
if $app_path; then calls=(app-path); fi
if ((${#selected_calls[@]})); then calls=("${selected_calls[@]}"); fi
bench_args=()
if $contention; then
  if ((${#selected_calls[@]} == 0)); then calls=(update); fi
  bench_args+=(--contention)
fi
if $mix; then calls=(mix); fi
for call in "${calls[@]}"; do
  graphs=(none)
  if $profile; then graphs=(fp dwarf); fi
  for graph in "${graphs[@]}"; do
    label=$call
    if [[ $graph == dwarf ]]; then
      # A missing or short stack makes caller attribution incomplete.
      if ! python3 - "$work/$call.fp.summary.json" <<'UNWIND'
import json, sys
result = json.load(open(sys.argv[1]))
sys.exit(0 if max(result["shallow_percent"], result["stacks_with_unknown_percent"]) > 10 else 1)
UNWIND
      then break; fi
      label="$call-dwarf"
    fi
    bench_call=$call
    call_args=("${bench_args[@]}")
    if [[ $call == mix ]]; then bench_call=get; call_args+=(--mix); fi
    taskset -c 6-7 "$bin/wamn-bench" --base-url "$bench_url" --host "$route_host" \
      --pat-file "$root/operator-pat.json" --call "$bench_call" "${call_args[@]}" --concurrency "$concurrency" \
      --duration-secs "$duration" --tier local --db-url "$database_url" \
      --output "$work/$label.json" >"$work/$label.log" 2>&1 &
    bench_pid=$!
    until grep -q '^benchmark-start ' "$work/$label.log"; do
      if ! kill -0 "$bench_pid" 2>/dev/null; then
        wait "$bench_pid" || true
        bench_pid=
        cat "$work/$label.log" >&2
        exit 1
      fi
      sleep 0.05
    done
    cpu_start=$(host_cpu_ticks)
    start_database_profile "$label"
    if $profile; then
      host_pid=$(<"$work/host.pid")
      call_graph=$graph
      if [[ $graph == dwarf ]]; then call_graph=dwarf,16384; fi
      perf record --no-buildid-cache -e cpu-clock:u -F 99 --call-graph "$call_graph" -p "$host_pid" \
        -o "$work/$call.$graph.perf.data" >"$work/$call.$graph.perf.log" 2>&1 &
      perf_pid=$!
    fi
    wait "$bench_pid" || { cat "$work/$label.log" >&2; exit 1; }
    bench_pid=
    echo "$cpu_start $(host_cpu_ticks) $(getconf CLK_TCK)" >"$work/$label.host-cpu"
    if [[ -n $perf_pid ]]; then
      kill -INT "$perf_pid" 2>/dev/null || true
      record_status=0
      wait "$perf_pid" || record_status=$?
      if (( record_status != 0 && record_status != 130 )); then
        cat "$work/$call.$graph.perf.log" >&2
        exit "$record_status"
      fi
      perf_pid=
    fi
    stop_database_profile "$label"
    if $profile; then
      env -u DEBUGINFOD_URLS perf script --no-inline -F +period -i "$work/$call.$graph.perf.data" >"$work/$call.$graph.stacks"
      python3 tools/bench/name-guests.py "$guest_target/wasm32-wasip2/release" "$work/$call.$graph.stacks"
      "$HOME/.cargo/bin/inferno-collapse-perf" <"$work/$call.$graph.stacks" \
        | "$HOME/.cargo/bin/inferno-flamegraph" >"$work/$call.$graph.svg"
      env -u DEBUGINFOD_URLS perf report --stdio --no-inline --no-children --call-graph none \
        --percent-limit 0 -i "$work/$call.$graph.perf.data" >"$work/$call.$graph.self.txt"
      env -u DEBUGINFOD_URLS perf report --stdio --no-inline --children --call-graph none \
        --percent-limit 0 -i "$work/$call.$graph.perf.data" >"$work/$call.$graph.inclusive.txt"
      python3 tools/bench/profile-summary.py "$work/$call.$graph.stacks" "$work/$call.$graph.summary.json"
    fi
  done
done
if $app_path && ! $mix; then
  # Retain only the sampler variables of the host that served this measurement.
  python3 - "$work" <<'SAMPLER'
import json
from pathlib import Path
import sys
work = Path(sys.argv[1])
pid = int((work / "host.pid").read_text())
environment = dict(entry.split(b"=", 1) for entry in Path(f"/proc/{pid}/environ").read_bytes().split(b"\0") if entry)
sampler = {name: environment[name.encode()].decode() for name in ("OTEL_TRACES_SAMPLER", "OTEL_TRACES_SAMPLER_ARG")}
(work / "host-telemetry.json").write_text(json.dumps(sampler) + "\n")
SAMPLER
  # The host flushes its exporter on shutdown. Then flush the collector's batch.
  stop_dev
  docker stop "$prefix-collector" >"$work/collector-stop.log"
  python3 tools/bench/app-result.py "$work" "$output" \
    "$http_route_sha256" "$materializer_sha256" "$edge" "$seed_scale"
  exit
fi
python3 - "$work/metadata.json" "$concurrency" "$http_route_sha256" "$materializer_sha256" "$edge" <<'PY'
import json
import sys

with open(sys.argv[1], "x") as output:
    json.dump({
        "concurrency": int(sys.argv[2]),
        "http_route_sha256": sys.argv[3],
        "materializer_sha256": sys.argv[4],
        "edge": sys.argv[5] == "true",
    }, output, indent=2)
    output.write("\n")
PY
python3 tools/bench/result.py "$work" "$output" "$concurrency"
