#!/usr/bin/env python3
"""Compare a finished HTTP workspace sweep with exact PAT and P3 evidence."""
import argparse
from collections import Counter
import importlib.util
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
MONTH = Path(__file__).resolve().parents[2]
REDUCER = MONTH / "wasmcloud-2-9-cutover/workspace-integration-preparation-001/classify.py"
CAUSES = MONTH / "ctc8-20-pat-service/main-landing-001/compare.py"
CLASSIFIER_BASELINE = MONTH / "wasmcloud-2-9-cutover/validation-001/workspace-results.json"
REFERENCES = {
    "p3": MONTH / "p3-http-cutover/integrated-workspace-001/workspace-results.json",
    "pat": MONTH / "ctc8-20-pat-service/main-landing-001/workspace-results.json",
}


def load_helpers():
    modules = []
    for name, path in (("http_workspace_reducer", REDUCER), ("http_workspace_causes", CAUSES)):
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        modules.append(module)
    return modules


def targets(receipt):
    # Cargo's artifact hash is build-specific, not a target identity.
    return Counter((row["category"], row["description"],
                    re.sub(r"-[0-9a-f]+$", "", Path(row["executable"] or "").name))
                   for row in receipt["test_targets"])


def compare(current, reference, reducer, causes):
    old, old_duplicates = causes.index(reference["failures"], reducer)
    new, new_duplicates = causes.index(current["failures"], reducer)
    exact, changed = [], []
    for key in sorted(old.keys() & new.keys()):
        before, after = causes.cause(old[key], reducer), causes.cause(new[key], reducer)
        if before == after:
            exact.append(list(key))
        else:
            changed.append({"identity": list(key), "before": before, "after": after})
    skip_key = lambda row: (row["target_description"], row["name"])
    old_skips = Counter(map(skip_key, reference["explicit_self_skips"]["entries"]))
    new_skips = Counter(map(skip_key, current["explicit_self_skips"]["entries"]))
    result = {
        "reference_failures": len(old), "current_failures": len(new),
        "exact_identity_and_cause_matches": exact, "changed_causes": changed,
        "added_failures": [{"identity": list(key), "cause": causes.cause(new[key], reducer)}
                           for key in sorted(new.keys() - old.keys())],
        "absent_reference_failures": [list(key) for key in sorted(old.keys() - new.keys())],
        "duplicate_failure_identities": old_duplicates + new_duplicates,
        "reference_unresolved": reference["unresolved"], "current_unresolved": current["unresolved"],
        "explicit_self_skip_names_and_target_multiplicities_equal": old_skips == new_skips,
        "added_explicit_self_skips": [list(key) for key in sorted((new_skips - old_skips).elements())],
        "absent_explicit_self_skips": [list(key) for key in sorted((old_skips - new_skips).elements())],
    }
    result["exact_match_causes"] = [
        {"identity": key, "cause": causes.cause(new[tuple(key)], reducer),
         "reference_log_line": old[tuple(key)]["diagnostic_start_log_line"],
         "current_log_line": new[tuple(key)]["diagnostic_start_log_line"]}
        for key in result["exact_identity_and_cause_matches"]]
    before, after = targets(reference), targets(current)
    result["added_targets"] = [list(key) for key in sorted((after - before).elements())]
    result["absent_reference_targets"] = [list(key) for key in sorted((before - after).elements())]
    result["reference_counts"] = reference["counts"]
    return result


def needs_inspection(comparison):
    return any(comparison[key] for key in (
        "changed_causes", "added_failures", "absent_reference_failures",
        "duplicate_failure_identities", "reference_unresolved", "current_unresolved",
        "added_explicit_self_skips", "absent_explicit_self_skips",
        "added_targets", "absent_reference_targets"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    evidence = parser.parse_args().evidence_dir.resolve(strict=True)
    outputs = [evidence / name for name in ("workspace-results.json", "workspace-comparison.json")]
    if any(path.exists() or path.is_symlink() for path in outputs):
        parser.error("comparison output exists; preserve it and use a new evidence directory")
    reducer, causes = load_helpers()
    inputs = [evidence / name for name in ("workspace.log", "exit-code.txt", "source.txt")]
    environment_path = evidence / "environment-names.json"
    environment = None
    if environment_path.exists():
        environment = json.loads(environment_path.read_text())
        inputs.append(environment_path)
    current = reducer.classify(inputs[0], CLASSIFIER_BASELINE,
                               int(inputs[1].read_text()), environment)
    comparisons = {name: compare(current, json.loads(path.read_text()), reducer, causes)
                   for name, path in REFERENCES.items()}
    inspect = [name for name, comparison in comparisons.items() if needs_inspection(comparison)]
    result = {
        "schema": "wamn-http-workspace-comparison/v1",
        "source": inputs[2].read_text().strip(), "exit_code": current["exit_code"],
        "counts": current["counts"], "combined_reported_counts": current["combined_reported_counts"],
        "comparisons": comparisons, "comparisons_requiring_inspection": inspect,
        "proof_verdict": "not supplied",
        "inputs": [reducer.file_receipt(path) for path in
                   (*inputs, REDUCER, CAUSES, CLASSIFIER_BASELINE,
                    *REFERENCES.values(), Path(__file__))],
        "limitations": [
            "The original Cargo exit is retained; comparison exit 2 means differences need inspection.",
            "Target identity removes only the trailing Cargo artifact hash and retains multiplicity.",
            "Diagnostic normalization is the retained PAT cause helper's exact normalization.",
            "Reported passes and target totals are not an acceptance verdict or an exact live-proof count.",
            "Explicit self-skips are a lower bound; silent early returns remain possible.",
            "An absent failure or target is not evidence of a fix or an executed proof.",
            "Missing environment receipts leave arming unknown; arming names alone prove no execution.",
            "The classifier's historical labels remain separate from both exact baseline comparisons.",
        ],
    }
    for path, value in zip(outputs, (current, result)):
        causes.write_fresh(path, value)
    print(json.dumps({"workspace_exit_code": current["exit_code"],
                      "comparisons_requiring_inspection": inspect,
                      "comparison_output": str(outputs[1])}, separators=(",", ":")))
    return 2 if inspect else 0


if __name__ == "__main__":
    raise SystemExit(main())
