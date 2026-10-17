#!/usr/bin/env bash
# Run from the repository machine after the GCP runbook prerequisites.
set -euo pipefail
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^pub const CHART_VERSION: &str = "\([^"]*\)";/\1/p' crates/control/lib/src/release_chart.rs)
test -n "$version"
output="tests/bench/platform-$version.json"
if [[ -e "$output" ]]; then
  echo "Refusing to overwrite $output" >&2
  exit 1
fi

# shellcheck source=/dev/null
source "$HOME/.cache/wamn-install/platform.sh"
database_url=$(db_url receiving)
database=${database_url##*/}
# Read counts through kubectl exec. A port-forward exits when a tunneled connection resets.
sql() { kubectl -n platform exec wamn-pg-1 -c postgres -- psql -U postgres -d "$database" -XAt -v ON_ERROR_STOP=1 -c "$1"; }
cloud=(--project wamn-dev --zone us-central1-a --quiet)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gcloud compute scp "$G/operator-pat.json" wamn-bench-client:pat/operator-pat.json "${cloud[@]}"
printf '%s' "$database_url" | gcloud compute ssh wamn-bench-client "${cloud[@]}" \
  --command 'umask 077; cat > ~/pat/db-url'

for call in get update; do
  sql "SELECT pg_stat_statements_reset(0, (SELECT oid FROM pg_database WHERE datname = current_database()))" >/dev/null
  gcloud compute ssh wamn-bench-client "${cloud[@]}" -- \
    -o ExitOnForwardFailure=yes -R 15432:localhost:15432 \
    "cd wamn && target/release/wamn-bench --host receiving.wamn.dev --pat-file ~/pat/operator-pat.json \
      --call $call --concurrency 16 --duration-secs 60 --tier tier-2 \
      --db-url \"\$(cat ~/pat/db-url)\" --output /tmp/wamn-bench-$call.json" >"$work/$call.json"
  sql "SELECT calls FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())" \
    >"$work/$call.pgss"
done

python3 - "$work" "$output" <<'PY'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
result = {"concurrency": 16}
for call in ("get", "update"):
    run = json.loads((work / f"{call}.json").read_text())
    result[call] = {key: run[key] for key in (
        "throughput_per_sec", "p50_ms", "p95_ms", "p99_ms", "calls", "errors"
    )}
    # Round each statement's ratio to exclude setup and background polling.
    result[call]["pg_stat_statements_calls_per_request"] = sum(
        round(int(calls) / run["calls"])
        for calls in (work / f"{call}.pgss").read_text().splitlines()
    )
with open(sys.argv[2], "x") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
print(sys.argv[2])
PY
