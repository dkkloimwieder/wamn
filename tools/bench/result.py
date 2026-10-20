"""Write the common platform and local benchmark baseline format."""
import json
from pathlib import Path
import sys


def write_result(output, concurrency, measurements):
    result = {"concurrency": concurrency}
    for call, run in measurements.items():
        result[call] = {key: run[key] for key in (
            "throughput_per_sec", "p50_ms", "p95_ms", "p99_ms", "calls", "errors",
            "pg_stat_statements_calls_per_request",
        )}
        # The record_receipt floor replay adds its per-row times (perf-arch.md M7).
        result[call].update({key: run[key] for key in (
            "lines", "statement_ms_per_row", "trigger_ms_per_row", "trigger_share",
        ) if key in run})
    with Path(output).open("x") as destination:
        json.dump(result, destination, indent=2)
        destination.write("\n")
    print(json.dumps(result, indent=2))
    print(output)


if __name__ == "__main__":
    work, output = map(Path, sys.argv[1:3])
    measurements = {}
    for call in ("get", "update", "query", "receipt-query", "create", "record_receipt", "mix", "app-path"):
        path = work / f"{call}.json"
        if not path.exists():
            continue
        run = json.loads(path.read_text())
        counts = work / f"{call}.pgss"
        if counts.exists():
            # Round each statement's ratio to exclude setup and background polling.
            statements = sum(round(int(count) / run["calls"])
                             for count in counts.read_text().splitlines())
        else:
            statements = run["statements_per_request"]
        run["pg_stat_statements_calls_per_request"] = statements
        measurements[call] = run
    write_result(output, int(sys.argv[3]), measurements)
