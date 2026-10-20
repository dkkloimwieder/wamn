"""Summarize pgbench's completed-transaction logs, whose latency unit is us."""
import csv
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from result import write_result

work, output = map(Path, sys.argv[1:3])
result = {}
# Only gcp.sh runs the update replay without triggers (perf-arch.md M4) and
# the record_receipt replay (M7), and its CALLS can name a subset.
calls = [call for call in ("select-only", "simple-update", "get", "update", "update-no-triggers")
         if (work / f"{call}.log").exists()]
calls += sorted((path.stem for path in work.glob("receipt-*.log")
                 if "." not in path.stem), key=lambda call: int(call.split("-")[1]))
for call in calls:
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
    if call.startswith("receipt-"):
        lines = int(call.split("-")[1])
        receipts, receipt_lines = map(int, (work / f"{call}.receipt-counts").read_text().strip().split("|"))
        if receipts != len(latencies) or receipt_lines != receipts * lines:
            raise SystemExit(f"{call}: {receipts} receipts and {receipt_lines} lines for {len(latencies)} calls")
        statement = sum(
            float(row["total_exec_time"])
            for row in csv.DictReader((work / f"{call}.statements.csv").read_text().splitlines())
            if row["toplevel"] == "t" and row["query"].startswith("WITH requested AS")
        )
        # The generated trigger functions of the Receiving relations.
        triggers = sum(
            float(row["total_time"])
            for row in csv.DictReader((work / f"{call}.functions.csv").read_text().splitlines())
            if row["schemaname"] == "wamn_history" and row["funcname"].startswith("receiving__")
        )
        result[call]["lines"] = lines
        result[call]["statement_ms_per_row"] = statement / (receipts * lines)
        result[call]["trigger_ms_per_row"] = triggers / (receipts * lines)
        result[call]["trigger_share"] = triggers / statement
if any(call.startswith("update") for call in calls):
    updates, finished = map(int, (work / "update-counts").read_text().strip().split("|"))
    replayed = sum(result[call]["calls"] for call in calls if call.startswith("update"))
    if updates != replayed or finished != updates:
        raise SystemExit(f"Update replay mismatch: {updates} changed rows, {finished} finished claims")
write_result(output, int(sys.argv[3]), result)
