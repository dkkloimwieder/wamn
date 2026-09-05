#!/usr/bin/env python3
"""compare.py <before summary.json> <after summary.json> : per layer, per step."""
import json
import sys

b = json.load(open(sys.argv[1]))
a = json.load(open(sys.argv[2]))


def rows(s):
    return {(r["layer"], r["concurrency"]): r for r in s["results"]}


rb, ra = rows(b), rows(a)
print("| layer | c | req/s before → after | Δ | p50 ms before → after | p99 ms before → after | host CPU ms/req before → after |")
print("|---|---:|---:|---:|---:|---:|---:|")
for layer in [l["layer"] for l in b["index"]["layers"]]:
    for c in b["index"]["concurrency"]:
        x, y = rb.get((layer, c)), ra.get((layer, c))
        if not x or not y:
            continue
        d = (y["requests_per_second"] / x["requests_per_second"] - 1) * 100
        print(f"| `{layer}` | {c} | {x['requests_per_second']:.0f} → {y['requests_per_second']:.0f} | {d:+.0f} % "
              f"| {x['p50_ms']:.2f} → {y['p50_ms']:.2f} | {x['p99_ms']:.2f} → {y['p99_ms']:.2f} "
              f"| {x['server']['host_cpu_ms_per_request']:.1f} → {y['server']['host_cpu_ms_per_request']:.1f} |")
print()
print("| layer | knee before → after | peak req/s before → after |")
print("|---|---:|---:|")
for vb, va in zip(b["verdicts"], a["verdicts"]):
    kb = vb["knee"]["concurrency"] if vb["knee"] else "none"
    ka = va["knee"]["concurrency"] if va["knee"] else "none"
    print(f"| `{vb['layer']}` | {kb} → {ka} | {vb['peak']['requests_per_second']:.0f} (c={vb['peak']['concurrency']}) → "
          f"{va['peak']['requests_per_second']:.0f} (c={va['peak']['concurrency']}) |")
