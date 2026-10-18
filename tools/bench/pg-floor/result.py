"""Summarize pgbench's completed-transaction logs, whose latency unit is us."""
import json
from pathlib import Path
import re
import sys

work, output = map(Path, sys.argv[1:3])
result = {"concurrency": int(sys.argv[3]), "duration_secs": int(sys.argv[4]), "scale": 10, "query_mode": "prepared"}
for call in ("select-only", "simple-update", "get", "update"):
    log = (work / f"{call}.log").read_text()
    tps, = re.findall(r"^tps = ([0-9.]+)", log, re.M)
    failures, = re.findall(r"^number of failed transactions: ([0-9]+)", log, re.M)
    if int(failures):
        raise SystemExit(f"{call}: {failures} failed transactions")
    latencies = sorted(
        int(line.split()[2])
        for path in work.glob(f"{call}.latencies.*")
        for line in path.read_text().splitlines()
    )
    if not latencies:
        raise SystemExit(f"{call}: no completed transactions")
    result[call] = {
        "tps": float(tps),
        "mean_ms": sum(latencies) / len(latencies) / 1000,
        "max_ms": latencies[-1] / 1000,
        "p50_ms": latencies[(len(latencies) * 50 + 99) // 100 - 1] / 1000,
        "p99_ms": latencies[(len(latencies) * 99 + 99) // 100 - 1] / 1000,
        "transactions": len(latencies),
        "errors": int(failures),
    }
updates, finished = map(int, (work / "update-counts").read_text().strip().split("|"))
if updates != result["update"]["transactions"] or finished != updates:
    raise SystemExit(f"Update replay mismatch: {updates} changed rows, {finished} finished claims")
result["replay_parameter_queries_per_transaction"] = 1
with output.open("x") as destination:
    json.dump(result, destination, indent=2)
    destination.write("\n")
print(json.dumps(result, indent=2))
print(output)
