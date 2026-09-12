#!/usr/bin/env python3
"""Offline libtest reducer using validation-001's identities and counting rules."""
import argparse
from collections import Counter
import datetime
import hashlib
import json
from pathlib import Path
import re

ANSI = re.compile(r"\x1b\[[0-9;]*m")
HEADER = re.compile(r"^\s+(?:Running (?P<description>.+) \((?P<executable>[^)]+)\)|Doc-tests (?P<doc>\S+))$")
SUMMARY = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out; finished in ([0-9.]+)s$")
CASE = re.compile(r"^test (.+?) \.\.\. (.*)$")
RERUN = re.compile(r"^error: (?:test|doctest) failed, to rerun pass `-p (\S+) (.+)`$")
ARM = re.compile(r"\b(?:WAMN_[A-Z0-9_]+|DB_URL|DATABASE_URL|PG[A-Z0-9_]+)\b")
STARTUP = ("wamn-proof-integration", "--test startup_burst_live",
           "production_http_start_burst_keeps_native_host_progress")
HOST = ("wamn-host", "--test native_lifecycle_live",
        "rebuilt_host_probes_signals_and_scheduler_recovery")


def redact(text):
    text = re.sub(r"(\b[a-zA-Z][a-zA-Z0-9+.-]*://)[^\s/\"']+@", r"\1[REDACTED]@", text)
    return re.sub(r"(?i)\bBearer\s+[^\s\"']+", "Bearer [REDACTED]", text)


def identity(failure):
    return tuple(failure[k] for k in ("package", "cargo_target", "name"))


def failure_signatures(diagnostic_lines):
    signatures = []
    for line in diagnostic_lines:
        case = CASE.match(line)
        text = case[2] if case else line.strip()
        if re.match(r"^thread .+ panicked at ", text):
            signatures.append("Rust panic")
        elif text.startswith(("Error:", "error:", "assertion ")):
            signatures.append(redact(text))
    return signatures


def file_receipt(path):
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest()}


def classify(log_path, baseline_path, exit_code, environment=None):
    baseline = json.loads(baseline_path.read_text())
    old = {identity(f): f for f in baseline["failures"]}
    lines = [ANSI.sub("", line) for line in log_path.read_text(errors="replace").splitlines()]
    headers = [(i, HEADER.match(line)) for i, line in enumerate(lines) if HEADER.match(line)]
    targets, failures, skips, excluded, unresolved = [], [], [], [], []
    for position, (begin, header) in enumerate(headers):
        end = headers[position + 1][0] if position + 1 < len(headers) else len(lines)
        summaries = [(i, SUMMARY.match(lines[i])) for i in range(begin, end) if SUMMARY.match(lines[i])]
        description = header["description"] or header["doc"]
        if not summaries:
            unresolved.append({"kind": "target_without_final_summary", "log_line": begin + 1,
                               "target": description})
            continue
        summary_line, summary = summaries[-1]
        for nested_line, _ in summaries[:-1]:
            excluded.append({"parent_target": description, "parent_running_log_line": begin + 1,
                             "summary_log_line": nested_line + 1, "summary": lines[nested_line]})
        category = "doctest" if header["doc"] else "test"
        values = list(map(int, summary.group(2, 3, 4, 5, 6)))
        target = dict(zip(("reported_passed", "failed", "ignored", "measured", "filtered_out"), values))
        target.update(category=category, description=description, executable=header["executable"],
                      running_log_line=begin + 1, summary_log_line=summary_line + 1,
                      result=summary[1], reported_seconds=float(summary[7]), explicit_self_skips=[])
        targets.append(target)
        cases = [(i, CASE.match(lines[i])) for i in range(begin, summary_line) if CASE.match(lines[i])]
        case_diagnostics = {}
        for index, (case_line, case) in enumerate(cases):
            stop = cases[index + 1][0] if index + 1 < len(cases) else summary_line
            # Stop before failure-name lists and nested summaries; retain real test output.
            for j in range(case_line + 1, stop):
                if lines[j] == "failures:" or SUMMARY.match(lines[j]):
                    stop = j
                    break
            diagnostic = [(case_line, case[2]), *[(j, lines[j]) for j in range(case_line + 1, stop)]]
            completion = [(j, text) for j, text in diagnostic if text.strip() in ("ok", "FAILED", "ignored")]
            status = completion[-1][1].strip() if completion else None
            case_diagnostics[case[1]] = (case_line, stop, diagnostic, status)
            skip_lines = [(j, text) for j, text in diagnostic if re.search(r"\b(?:skipping|skipped)\b", text, re.I)]
            if status == "ok" and skip_lines:
                j, text = skip_lines[0]
                entry = {"target_description": description, "target_running_log_line": begin + 1,
                         "name": case[1], "diagnostic_log_line": j + 1,
                         "completion_log_line": completion[-1][0] + 1, "message": redact(text),
                         "referenced_arming_inputs": sorted(set(ARM.findall(text))),
                         "reported_status": "ok", "live_or_artifact_proof_executed": False}
                # A parent test may spawn a libtest child with the same name.
                if not any(s["name"] == case[1] for s in target["explicit_self_skips"]):
                    target["explicit_self_skips"].append(entry)
                    skips.append(entry)
        target["explicit_self_skip_count"] = len(target["explicit_self_skips"])
        target["reported_passes_after_excluding_explicit_self_skips"] = target["reported_passed"] - target["explicit_self_skip_count"]
        if target["explicit_self_skip_count"] > target["reported_passed"]:
            unresolved.append({"kind": "skip_count_exceeds_passes", "log_line": summary_line + 1})
        if not target["failed"]:
            continue
        reruns = [RERUN.match(lines[i]) for i in range(summary_line + 1, end) if RERUN.match(lines[i])]
        package, cargo_target = (reruns[0][1], reruns[0][2]) if len(reruns) == 1 else (None, None)
        markers = [i for i in range(begin, summary_line) if lines[i] == "failures:"]
        failure_names = [(i, lines[i].strip()) for i in range(markers[-1] + 1, summary_line)
                         if lines[i].strip()] if markers else []
        if len(failure_names) != target["failed"] or package is None:
            unresolved.append({"kind": "failure_identity_or_count_incomplete", "log_line": summary_line + 1})
        for failure_line, name in failure_names:
            key = (package, cargo_target, name)
            if name in case_diagnostics:
                first, stop, diagnostic, status = case_diagnostics[name]
            else:
                first, stop, diagnostic, status = begin, summary_line, [], None
                unresolved.append({"kind": "missing_failure_diagnostic", "identity": key})
            text = "\n".join(t for _, t in diagnostic)
            prior = old.get(key)
            matches = []
            classification = "new_potential_cutover_failure"
            if prior:
                # Retain each cause only when current output supports it; names alone do not suffice.
                matches = [r["text"] for r in prior["reason_lines"] if r["text"] in text]
                if prior["classification"] == "known_baseline_infrastructure_failure":
                    same_cause = all(s in text for s in ("Kubernetes decode failed:",
                                    "localhost:8080", "connection refused"))
                else:
                    same_cause = bool(matches) and len(matches) == len(prior["reason_lines"])
                old_signatures = failure_signatures(r["text"] for r in prior["diagnostic_excerpt"])
                current_signatures = failure_signatures(t for _, t in diagnostic)
                same_cause = same_cause and old_signatures == current_signatures
                classification = prior["classification"] if same_cause else "baseline_identity_cause_changed"
            elif key == STARTUP and "WAMN_STARTUP_BURST_INPUT must name the runner-owned fixture" in text:
                classification = "new_unarmed_startup_fixture"
            elif key == HOST and "set WAMN_HOST_LIVE_NATS_SERVER_BIN to the real NATS server binary" in text:
                armed = set((environment or {}).get("explicitly_armed_names", []))
                classification = ("new_unarmed_host_fixture" if "WAMN_HOST_LIVE_NATS_SERVER_BIN" not in armed
                                  else "new_potential_cutover_failure")
            failures.append({"package": package, "cargo_target": cargo_target, "name": name,
                             "failure_list_log_line": failure_line + 1,
                             "diagnostic_start_log_line": first + 1, "diagnostic_end_log_line": stop,
                             "diagnostic_excerpt": [{"log_line": j + 1, "text": redact(t)} for j, t in diagnostic],
                             "referenced_arming_inputs": sorted(set(ARM.findall(text))),
                             "classification": classification, "baseline_identity_match": bool(prior),
                             "matched_baseline_reason_lines": [redact(t) for t in matches]})
    counts = {}
    for category in ("test", "doctest"):
        selected = [t for t in targets if t["category"] == category]
        counts[category + "_targets"] = len(selected)
        counts[category + "_nonempty_targets"] = sum(bool(t["reported_passed"] + t["failed"] + t["ignored"]) for t in selected)
        counts[category + "_empty_targets"] = len(selected) - counts[category + "_nonempty_targets"]
        counts[category + "_failed_targets"] = sum(t["failed"] > 0 for t in selected)
        for key in ("reported_passed", "failed", "ignored", "measured", "filtered_out",
                    "explicit_self_skip_count", "reported_passes_after_excluding_explicit_self_skips"):
            counts[category + "_" + key] = sum(t[key] for t in selected)
    failed_targets = sum(t["failed"] > 0 for t in targets)
    footer = [int(m[1]) for line in lines if (m := re.match(r"^error: (\d+) targets? failed:$", line))]
    if footer and footer[-1] != failed_targets:
        unresolved.append({"kind": "cargo_failed_target_footer_mismatch", "footer": footer[-1],
                           "parsed": failed_targets})
    if not targets or (exit_code and not failed_targets):
        unresolved.append({"kind": "non_libtest_or_incomplete_run", "exit_code": exit_code})
    if failed_targets and not footer:
        unresolved.append({"kind": "missing_cargo_no_fail_fast_footer"})
    if exit_code not in (0, 101):
        unresolved.append({"kind": "unexpected_process_exit", "exit_code": exit_code})
    for i, line in enumerate(lines):
        if line.startswith("error: could not compile "):
            unresolved.append({"kind": "cargo_compilation_error_requires_inspection",
                               "log_line": i + 1, "text": redact(line)})
    if exit_code == 0 and failed_targets:
        unresolved.append({"kind": "exit_zero_with_test_failures"})
    new_keys = {identity(f) for f in failures}
    if len(new_keys) != len(failures):
        unresolved.append({"kind": "duplicate_failure_identity"})
    return {
        "schema": "wamn-cutover-workspace-classification-v1",
        "analysis_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "log": file_receipt(log_path), "baseline": file_receipt(baseline_path),
        "exit_code": exit_code, "counts": counts,
        "combined_reported_counts": {
            "passed_including_doctests": counts["test_reported_passed"] + counts["doctest_reported_passed"],
            "failed_including_doctests": counts["test_failed"] + counts["doctest_failed"],
            "passed_after_excluding_explicit_self_skips": counts["test_reported_passed"] + counts["doctest_reported_passed"] - len(skips),
            "executed_pass_count_is_exact": False,
        },
        "failure_classification_counts": dict(Counter(f["classification"] for f in failures)),
        "failures": failures, "test_targets": targets, "excluded_nested_summaries": excluded,
        "explicit_self_skips": {"count": len(skips), "count_is_lower_bound": True,
                                "included_in_reported_test_passes": True, "entries": skips},
        "baseline_comparison": {
            "baseline_failure_count": len(old), "exact_identity_matches": len(set(old) & new_keys),
            "new_failure_identities": [list(k) for k in sorted(new_keys - set(old), key=str)],
            "baseline_identities_not_observed_failing": [list(k) for k in sorted(set(old) - new_keys)],
            "absence_is_not_a_fix_claim": True,
        },
        "environment_names": environment,
        "arming_status": "Explicit names recorded" if environment else "Unknown: no environment receipt supplied",
        "unresolved": unresolved,
        "limitations": [
            "Explicit self-skips are a lower bound; silent early returns and all live arming paths were not surveyed.",
            "Reported passes after skip subtraction are not an exact count of executed proofs.",
            "An absent prior failure may be a pass, rename, removed target, filtered test, self-skip, or incomplete run.",
            "Compiler error text in successful compile-fail doctests is not itself a new failure.",
            "This classifier does not supply a cutover acceptance verdict; read unresolved cases and raw diagnostics.",
        ],
        "redaction": "Copied diagnostic URL userinfo and Bearer tokens redacted; raw log is unchanged.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--exit-code-file", type=Path, required=True)
    parser.add_argument("--environment-names", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    environment = json.loads(args.environment_names.read_text()) if args.environment_names else None
    result = classify(args.log, args.baseline, int(args.exit_code_file.read_text().strip()), environment)
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    print(json.dumps({"counts": result["counts"], "failure_classes": result["failure_classification_counts"],
                      "unresolved": result["unresolved"]}, indent=2))
    return 2 if result["unresolved"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
