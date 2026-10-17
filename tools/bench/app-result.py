"""Measure span own times inside sampled receipt requests from local OTLP JSON."""
from collections import defaultdict
import json
import re
from pathlib import Path
import sys

work, output = map(Path, sys.argv[1:3])
run = json.loads((work / "app-path.json").read_text())
if run["errors"]:
    raise SystemExit(f"Application path returned {run['errors']} errors")
configuration = (work / "env/local-artifacts/otel/config.yaml").read_text()
sampling, = re.findall(r"sampling_percentage: ([0-9.]+)", configuration)
run["sampling_percentage"] = float(sampling)
traces = defaultdict(dict)
for line in (work / "env/local-artifacts/otel/traces.jsonl").open():
    for resource in json.loads(line).get("resourceSpans", []):
        for scope in resource.get("scopeSpans", []):
            for span in scope.get("spans", []):
                traces[span["traceId"]][span["spanId"]] = span

parts = defaultdict(list)
totals = []
for spans in traces.values():
    children = defaultdict(list)
    for span in spans.values():
        children[span.get("parentSpanId")].append(span)
    roots = [span for span in spans.values()
             if span["name"] == "invoke_component_handler"
             and run["started_at_unix_nanos"] <= int(span["startTimeUnixNano"]) < run["ended_at_unix_nanos"]]
    for root in roots:
        selected = []
        pending = [root]
        while pending:
            span = pending.pop()
            selected.append(span)
            pending.extend(children[span["spanId"]])
        if not any(attribute["key"] == "wamn.operation" and "receipt/get@" in attribute["value"].get("stringValue", "")
                   for span in selected for attribute in span.get("attributes", [])):
            continue
        totals.append((int(root["endTimeUnixNano"]) - int(root["startTimeUnixNano"])) / 1e6)
        for span in selected:
            start, end = int(span["startTimeUnixNano"]), int(span["endTimeUnixNano"])
            # Subtract the union of direct child intervals, clipped to the parent.
            intervals = sorted((max(start, int(child["startTimeUnixNano"])), min(end, int(child["endTimeUnixNano"])))
                               for child in children[span["spanId"]])
            covered, cursor = 0, start
            for left, right in intervals:
                covered += max(0, right - max(left, cursor))
                cursor = max(cursor, right)
            parts[span["name"]].append((end - start - covered) / 1e6)
if not totals:
    raise SystemExit("No sampled receipt request traces in the measured interval")
run["sampled_trace_count"] = len(totals)
run["edge"] = sys.argv[5] == "true"
run["seed_scale"] = int(sys.argv[6])
run["http_route_sha256"] = sys.argv[3]
run["materializer_sha256"] = sys.argv[4]
run["host_mean_ms"] = sum(totals) / len(totals)
run["span_parts"] = dict(sorted((
    (name, {"mean_ms": sum(values) / len(totals), "span_count": len(values),
            "per_span_mean_ms": sum(values) / len(values)})
    for name, values in parts.items()
), key=lambda part: -part[1]["mean_ms"]))
run["postgres_mean_ms"] = sum(part["mean_ms"] for name, part in run["span_parts"].items()
                              if name == "wamn.postgres" or name.startswith("wamn.postgres."))
with output.open("x") as destination:
    json.dump(run, destination, indent=2)
    destination.write("\n")
print(json.dumps({key: run[key] for key in ("throughput_per_sec", "p50_ms", "p99_ms", "max_ms", "sampled_trace_count", "span_parts")}, indent=2))
print(output)
