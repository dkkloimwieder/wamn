"""Summarize pgbench's completed-transaction logs, whose latency unit is us."""
import csv
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from result import write_result

work, output = map(Path, sys.argv[1:3])
result = {}
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
        "throughput_per_sec": float(tps),
        "p50_ms": latencies[(len(latencies) * 50 + 99) // 100 - 1] / 1000,
        "p95_ms": latencies[(len(latencies) * 95 + 99) // 100 - 1] / 1000,
        "p99_ms": latencies[(len(latencies) * 99 + 99) // 100 - 1] / 1000,
        "calls": len(latencies),
        "errors": int(failures),
        "pg_stat_statements_calls_per_request": sum(
            round(int(row["calls"]) / len(latencies))
            for row in csv.DictReader((work / f"{call}.statements.csv").read_text().splitlines())
        ),
    }
updates, finished = map(int, (work / "update-counts").read_text().strip().split("|"))
if updates != result["update"]["calls"] or finished != updates:
    raise SystemExit(f"Update replay mismatch: {updates} changed rows, {finished} finished claims")
write_result(output, int(sys.argv[3]), result)
