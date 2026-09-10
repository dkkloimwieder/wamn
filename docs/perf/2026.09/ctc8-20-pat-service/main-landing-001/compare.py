#!/usr/bin/env python3
"""Reduce the completed workspace log and compare failure causes with P3."""

from collections import Counter
import importlib.util
import json
from pathlib import Path
import re
import shlex
import sys


EVIDENCE = Path(__file__).resolve().parent
MONTH = EVIDENCE.parent.parent
CUTOVER = MONTH / "wasmcloud-2-9-cutover"
P3 = MONTH / "p3-http-cutover/integrated-workspace-001"
CLASSIFIER = CUTOVER / "workspace-integration-preparation-001/classify.py"
BASELINE = CUTOVER / "validation-001/workspace-results.json"
PANIC = re.compile(r"^thread .+ panicked at ")
KLOG_CLOCK_PID = re.compile(r"\b[EWI]\d{4} \d{2}:\d{2}:\d{2}\.\d+\s+\d+ (?=\S+\.go:\d+\])")
BACKTRACE_NOTE = "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"


def normalize(text):
    return KLOG_CLOCK_PID.sub("<klog-clock-pid> ", text.strip())


def cause(failure, reducer):
    lines = [row["text"] for row in failure["diagnostic_excerpt"]]
    causal = []
    for line in lines:
        case = reducer.CASE.match(line)
        text = (case[2] if case else line).strip()
        if not text or text in {"ok", "FAILED", "ignored", BACKTRACE_NOTE} or PANIC.match(text):
            continue
        causal.append(normalize(text))
    return {
        "failure_signatures": [normalize(text) for text in reducer.failure_signatures(lines)],
        "referenced_arming_inputs": sorted(failure["referenced_arming_inputs"]),
        "causal_lines": causal,
    }


def index(failures, reducer):
    keys = [reducer.identity(failure) for failure in failures]
    duplicates = [list(key) for key, count in Counter(keys).items() if count != 1]
    return dict(zip(keys, failures)), duplicates


def write_fresh(path, data):
    with path.open("x") as output:
        json.dump(data, output, indent=2)
        output.write("\n")


def main():
    if not (EVIDENCE / "workspace.exit").is_file():
        raise SystemExit("Wait for workspace.exit before classifying the workspace run.")
    outputs = [EVIDENCE / name for name in (
        "workspace-results.json", "workspace-p3-comparison.json", "workspace-p3-summary.json")]
    if any(path.exists() for path in outputs):
        raise SystemExit("Comparison output exists. Preserve it and use a new evidence directory.")
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("workspace_reducer", CLASSIFIER)
    reducer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reducer)
    current = reducer.classify(
        EVIDENCE / "workspace.log", BASELINE,
        int((EVIDENCE / "workspace.exit").read_text().strip()),
    )
    reference_path = P3 / "workspace-results.json"
    reference = json.loads(reference_path.read_text())
    reference_run = json.loads((P3 / "result.json").read_text())
    command = shlex.split((EVIDENCE / "workspace.command").read_text().strip())
    old, old_duplicates = index(reference["failures"], reducer)
    new, new_duplicates = index(current["failures"], reducer)
    matches, changed, added = [], [], []
    for key, failure in new.items():
        current_cause = cause(failure, reducer)
        if key not in old:
            added.append({"identity": list(key), "cause": current_cause, "failure": failure})
            continue
        prior_cause = cause(old[key], reducer)
        differences = {
            field: {"reference": prior_cause[field], "current": current_cause[field]}
            for field in prior_cause if prior_cause[field] != current_cause[field]
        }
        if differences:
            changed.append({"identity": list(key), "differences": differences,
                            "reference_failure": old[key], "current_failure": failure})
        else:
            matches.append({"identity": list(key), "cause": current_cause,
                            "reference_log_line": old[key]["diagnostic_start_log_line"],
                            "current_log_line": failure["diagnostic_start_log_line"]})
    absent = [list(key) for key in sorted(old.keys() - new.keys())]
    summary = {
        "workspace_exit_code": current["exit_code"],
        "command_matches_p3": command == reference_run["command"],
        "p3_failure_count": len(old),
        "current_failure_count": len(new),
        "exact_identity_and_cause_matches": len(matches),
        "changed_causes": len(changed),
        "new_failure_identities": len(added),
        "p3_failures_not_observed": len(absent),
        "current_unresolved_count": len(current["unresolved"]),
        "reference_unresolved_count": len(reference["unresolved"]),
        "duplicate_failure_identities": len(old_duplicates) + len(new_duplicates),
        "proof_verdict": "not supplied",
    }
    comparison = {
        "schema": "wamn-pat-workspace-comparison-v1",
        "summary": summary,
        "inputs": {
            "classifier": reducer.file_receipt(CLASSIFIER),
            "classifier_baseline": reducer.file_receipt(BASELINE),
            "p3_failures": reducer.file_receipt(reference_path),
            "p3_run": reducer.file_receipt(P3 / "result.json"),
            "current_log": reducer.file_receipt(EVIDENCE / "workspace.log"),
            "current_exit": reducer.file_receipt(EVIDENCE / "workspace.exit"),
            "current_command": reducer.file_receipt(EVIDENCE / "workspace.command"),
            "current_source": reducer.file_receipt(EVIDENCE / "workspace.source"),
            "wrapper": reducer.file_receipt(Path(__file__)),
        },
        "commands": {"p3": reference_run["command"], "current": command},
        "exact_matches": matches,
        "changed_causes": changed,
        "new_failure_identities": added,
        "p3_failures_not_observed": absent,
        "unresolved": {"p3": reference["unresolved"], "current": current["unresolved"],
                       "p3_duplicate_identities": old_duplicates,
                       "current_duplicate_identities": new_duplicates},
        "normalization": [
            "Strip surrounding whitespace from diagnostic lines.",
            "Remove blank lines, libtest status lines, and the standard backtrace advice line.",
            "Represent panic headers as the reducer's Rust panic signature, without thread IDs or source locations.",
            "Replace only the Kubernetes log timestamp and process ID before its Go source location.",
            "Preserve every other causal line in order and compare its text exactly.",
        ],
        "limitations": [
            "Referenced arming names describe diagnostics, not evidence that live tests ran.",
            "Neither comparison run supplies an environment receipt to the reducer.",
            "An absent prior failure is not evidence of a fix or an executed proof.",
            "Failure identities and causes drive this comparison, not target counts.",
            "The prior missing native NATS binary remains a failure, not a passing live proof.",
            "The reducer retains its historical classifications against validation-001 separately.",
            "This comparison supplies no merge or acceptance verdict.",
        ],
    }
    for path, data in zip(outputs, (current, comparison, summary)):
        write_fresh(path, data)
    print(json.dumps(summary, separators=(",", ":")))
    needs_inspection = (changed or added or absent or current["unresolved"]
                        or reference["unresolved"] or old_duplicates or new_duplicates
                        or not summary["command_matches_p3"])
    return 2 if needs_inspection else 0


if __name__ == "__main__":
    raise SystemExit(main())
