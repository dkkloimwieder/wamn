#!/usr/bin/env python3
"""gaps.py <trace.json>... : name every gap of a request.

Containers (handle_http_request, invoke_component_handler, http.handle,
wamn.component.invoke) are not counted as coverage; every other span is a
named leaf. Per trace: the leaf-covered time, the gaps between consecutive
leaves inside handle_http_request with the spans on either side, and the
sum. Then an average over the traces given.
"""
import json
import sys
from collections import defaultdict

CONTAINERS = {
    "handle_http_request",
    "invoke_component_handler",
    "http.handle",
    # the request side waiting for the head: a waiter, not work
    "http.await_head",
    "wamn.component.invoke",
    "wamn.route.authenticate",
    "wamn.auth.permissions",
    "wamn.component.linker_setup",
    "wamn.linker.scope",
    "wamn.linker.pending_scope",
    "wamn.postgres",
}


def spans(path):
    d = json.load(open(path))
    out = []
    for b in d.get("batches", []):
        for ss in b.get("scopeSpans", []):
            for s in ss.get("spans", []):
                out.append((s["name"], int(s["startTimeUnixNano"]), int(s["endTimeUnixNano"])))
    return out


def analyse(path):
    sp = spans(path)
    root = [x for x in sp if x[0] == "handle_http_request"][0]
    r0, r1 = root[1], root[2]
    leaves = sorted(
        (max(s, r0), min(e, r1), n)
        for n, s, e in sp
        if n not in CONTAINERS and e > r0 and s < r1
    )
    # merge overlapping leaves into covered intervals, remembering the names
    covered = []
    for s, e, n in leaves:
        if covered and s <= covered[-1][1]:
            covered[-1][1] = max(covered[-1][1], e)
            covered[-1][2].append(n)
        else:
            covered.append([s, e, [n]])
    gaps = []
    prev_end, prev_names = r0, ["<request start>"]
    for s, e, names in covered:
        if s > prev_end:
            gaps.append((s - prev_end, prev_names[-1], names[0]))
        prev_end, prev_names = e, names
    if r1 > prev_end:
        gaps.append((r1 - prev_end, prev_names[-1], "<request end>"))
    total = r1 - r0
    cov = sum(e - s for s, e, _ in covered)
    per_span = defaultdict(int)
    for n, s, e in sp:
        if n not in CONTAINERS:
            per_span[n] += e - s
    handle = [x for x in sp if x[0] == "http.handle"]
    handle_ms = (handle[0][2] - handle[0][1]) / 1e6 if handle else None
    return total, cov, gaps, per_span, handle_ms


def main(paths):
    agg_gaps = defaultdict(list)
    agg_total = agg_cov = 0.0
    agg_span = defaultdict(float)
    for p in paths:
        total, cov, gaps, per_span, handle_ms = analyse(p)
        agg_total += total
        agg_cov += cov
        for n, v in per_span.items():
            agg_span[n] += v
        print(f"== {p.split('/')[-1]}  total={total/1e6:.3f} ms  covered={cov/1e6:.3f}  "
              f"unnamed={(total-cov)/1e6:.3f}  http.handle={handle_ms}")
        for g, a, b in gaps:
            if g >= 50_000:  # >= 0.05 ms
                print(f"   gap {g/1e6:7.3f} ms  after {a:32s} before {b}")
            agg_gaps[(a, b)].append(g)
    n = len(paths)
    print(f"\n== average over {n} traces: total={agg_total/n/1e6:.3f} ms  "
          f"covered={agg_cov/n/1e6:.3f}  unnamed={(agg_total-agg_cov)/n/1e6:.3f} "
          f"({(agg_total-agg_cov)/agg_total*100:.1f}%)")
    print("   named spans (ms/req, summed per request):")
    for name, v in sorted(agg_span.items(), key=lambda kv: -kv[1]):
        print(f"      {name:36s} {v/n/1e6:7.3f}")
    print("   gaps present in every trace (ms/req):")
    for (a, b), gs in sorted(agg_gaps.items(), key=lambda kv: -sum(kv[1])):
        if len(gs) == n:
            print(f"      {sum(gs)/n/1e6:7.3f}  after {a:32s} before {b}")


if __name__ == "__main__":
    main(sys.argv[1:])
