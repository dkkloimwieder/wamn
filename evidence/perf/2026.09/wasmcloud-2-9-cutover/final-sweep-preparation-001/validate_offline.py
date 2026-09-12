#!/usr/bin/env python3
"""Validate preparation using retained and explicitly synthetic logs only."""
import argparse
import ast
import importlib.util
import json
from pathlib import Path
import tempfile


def module(path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    for path in (here / "run.py", here / "classify.py", Path(__file__)):
        ast.parse(path.read_text(), filename=str(path))
    run = module(here / "run.py")
    reducer = module(here / "classify.py")
    baseline_path = args.baseline_dir / "workspace-results.json"
    log = args.baseline_dir / "workspace-sweep.log"
    baseline = json.loads(baseline_path.read_text())
    actual = reducer.classify(log, baseline_path, 101)
    assert actual["counts"] == baseline["counts"]
    assert actual["combined_reported_counts"] == baseline["combined_reported_counts"]
    assert actual["failure_classification_counts"] == {k: v for k, v in baseline["failure_classification_counts"].items() if v}
    assert {reducer.identity(f) for f in actual["failures"]} == {reducer.identity(f) for f in baseline["failures"]}
    assert actual["explicit_self_skips"]["entries"] == baseline["explicit_self_skips"]["entries"]
    assert len(actual["excluded_nested_summaries"]) == 1
    assert actual["excluded_nested_summaries"][0]["summary_log_line"] == 3641
    assert not actual["unresolved"]
    assert run.COMMAND == baseline["run"]["argv"]
    environment = {name: "synthetic-private-value" for name in [
        "WAMN_JOURNEY_DOCUMENT", "WAMN_HOST_LIVE_NATS_SERVER_BIN", "WASH_NATS_URL",
        "DB_URL", "DATABASE_URL", "PGHOST", "PGPASSWORD", "PGSERVICEFILE",
        "OTEL_EXPORTER_OTLP_ENDPOINT", "GIT_CONFIG_GLOBAL", "KUBECONFIG", "CARGO_TARGET_DIR"]}
    environment["PATH"] = "synthetic-nonsecret-path"
    assert run.clean_environment(environment) == {"PATH": "synthetic-nonsecret-path"}
    raw = log.read_text()
    synthetic_startup = """     Running tests/startup_burst_live.rs (target/debug/deps/startup_burst_live-SYNTHETIC)

running 1 test
test production_http_start_burst_keeps_native_host_progress ... Error: WAMN_STARTUP_BURST_INPUT must name the runner-owned fixture
FAILED

failures:

failures:
    production_http_start_burst_keeps_native_host_progress

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p wamn-proof-integration --test startup_burst_live`

"""
    controls = []
    with tempfile.TemporaryDirectory(prefix="wamn-offline-log-controls-", dir=here) as scratch:
        scratch = Path(scratch)
        fixture = scratch / "synthetic-startup.log"
        fixture.write_text(raw.replace("error: 32 targets failed:\n", synthetic_startup + "error: 33 targets failed:\n"))
        startup = reducer.classify(fixture, baseline_path, 101, {"explicitly_armed_names": []})
        assert startup["baseline_comparison"]["exact_identity_matches"] == 67
        assert startup["failure_classification_counts"]["new_unarmed_startup_fixture"] == 1
        assert startup["combined_reported_counts"]["failed_including_doctests"] == 68
        assert startup["counts"]["test_explicit_self_skip_count"] == 85
        assert not startup["unresolved"]
        controls.append("Synthetic new startup fixture failure stays separate: 67 baseline identities plus one new unarmed fixture.")
        first_reason = baseline["failures"][0]["reason_lines"][0]["text"]
        fixture.write_text(raw.replace(first_reason, "Error: native driver unexpectedly stopped", 1))
        changed = reducer.classify(fixture, baseline_path, 101)
        assert changed["failure_classification_counts"]["baseline_identity_cause_changed"] == 1
        controls.append("An existing failure name with a changed diagnostic is not classified as the old cause.")
        fixture.write_text(raw.replace(first_reason, first_reason + "\nthread 'synthetic' (1) panicked at synthetic.rs:1:1:\nnew failure", 1))
        extra = reducer.classify(fixture, baseline_path, 101)
        assert extra["failure_classification_counts"]["baseline_identity_cause_changed"] == 1
        controls.append("An additional panic after a retained baseline reason is classified as a changed cause.")
        fixture.write_text(raw.replace("error: 32 targets failed:", "error: 31 targets failed:"))
        assert reducer.classify(fixture, baseline_path, 101)["unresolved"]
        controls.append("A truncated or inconsistent Cargo footer remains unresolved.")
        fixture.write_text(raw + "error: could not compile `synthetic-crate` (lib) due to 1 previous error\n")
        assert reducer.classify(fixture, baseline_path, 101)["unresolved"]
        assert reducer.classify(log, baseline_path, 130)["unresolved"]
        controls.append("Cargo compilation errors and abnormal process exits remain unresolved.")
    result = {
        "scope": "Offline parser and wrapper preparation only; no Cargo, tests, builds, services, or live calls were launched.",
        "baseline_log": reducer.file_receipt(log), "baseline_results": reducer.file_receipt(baseline_path),
        "retained_counts_match_exactly": True, "retained_failure_identities_match": 67,
        "retained_self_skip_entries_match": 85, "excluded_nested_summaries": 1,
        "full_sweep_argv_matches": True, "environment_filter_control_passed": True,
        "python_ast_syntax_passed": True, "synthetic_controls": controls,
        "preparation_files": [reducer.file_receipt(here / name) for name in
                              ("run.py", "classify.py", "validate_offline.py")],
    }
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("preparation_files", "baseline_log", "baseline_results")}, indent=2))


if __name__ == "__main__":
    main()
