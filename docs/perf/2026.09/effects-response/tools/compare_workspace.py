#!/usr/bin/env python3
"""Compare the combined sweep with the final cutover failure identities."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[5]
current_path = root / "docs/perf/2026.09/effects-response/combined-workspace-001/workspace-results.json"
baseline_path = root / "docs/perf/2026.09/wasmcloud-2-9-cutover/workspace-integration-001/workspace-results.json"
current = json.loads(current_path.read_text())
baseline = json.loads(baseline_path.read_text())


def identity(failure):
    return tuple(failure[key] for key in ("package", "cargo_target", "name"))


old = {identity(failure): failure for failure in baseline["failures"]}
new = {identity(failure): failure for failure in current["failures"]}
added = []
for key in sorted(new.keys() - old.keys()):
    failure = new[key]
    reasons = [row for row in failure["diagnostic_excerpt"] if row["text"].startswith("Error:")]
    missing = any("must arm" in row["text"] or "must name" in row["text"]
                  or "set WAMN_JOURNEY_DOCUMENT" in row["text"] for row in reasons)
    added.append({"identity": key,
                  "classification": "new_missing_live_fixture" if missing else "requires_source_inspection",
                  "diagnostic_excerpt": failure["diagnostic_excerpt"]})

changed_causes = []
for key in sorted(new.keys() & old.keys()):
    before, after = old[key], new[key]
    if (before["classification"] != after["classification"]
            or before["matched_baseline_reason_lines"] != after["matched_baseline_reason_lines"]):
        changed_causes.append(key)

result = {
    "schema": "wamn-effects-combined-workspace-comparison/v1",
    "inputs": [{"path": str(path.relative_to(root)),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
               for path in (baseline_path, current_path)],
    "baseline_failures": len(old),
    "current_failures": len(new),
    "matching_identities_and_classified_causes": len(old.keys() & new.keys()) - len(changed_causes),
    "changed_classified_causes": changed_causes,
    "absent_baseline_failures": sorted(old.keys() - new.keys()),
    "added_failures": added,
    "limitations": ["Missing fixture inputs do not prove live behavior.",
                    "Read the retained diagnostics and separate deployed proofs.",
                    "This comparison does not change the recorded failing sweep result."],
}
output = current_path.with_name("final-baseline-comparison.json")
with output.open("x") as stream:
    json.dump(result, stream, indent=2)
    stream.write("\n")
print(json.dumps({key: value for key, value in result.items() if key != "added_failures"}, indent=2))
