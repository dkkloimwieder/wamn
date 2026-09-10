#!/usr/bin/env python3
"""Prove two response-evidence guards with isolated source mutations."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys


MUTANTS = (
    {
        "name": "unselected-success-overwrites-committed-result",
        "path": "crates/execution/host/src/router_response.rs",
        "test": "router_response::tests::selected_committed_result_survives_arbitrary_success_and_label_enrichment",
        "before": b"""            NodeOutcome::Success { payload, .. } => {
                if contract.declaration.committed_result.as_deref() == Some(node) {""",
        "after": b"""            NodeOutcome::Success { payload, .. } => {
                if self.committed_result.is_some() {
                    self.committed_result = Some(payload.clone());
                }
                if contract.declaration.committed_result.as_deref() == Some(node) {""",
    },
    {
        "name": "multiple-failures-keep-latest-outcome",
        "path": "crates/platform/runtime/src/plugins/effect_span.rs",
        "test": "plugins::effect_span::tests::one_failure_survives_successes_but_never_another_failure",
        "before": b"                FailureEvidence::One(_) | FailureEvidence::Ambiguous => FailureEvidence::Ambiguous,",
        "after": b"                FailureEvidence::One(_) | FailureEvidence::Ambiguous => FailureEvidence::One(outcome),",
    },
)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def stop(_number, _frame):
    raise KeyboardInterrupt("mutation harness interrupted")


def run_arm(tree, capture, evidence, test):
    command = [
        "cargo", "test", "--locked", "--offline",
        "-p", "wamn-runtime", "-p", "wamn-execution-host", "--lib",
        test, "--", "--include-ignored", "--exact", "--test-threads=1",
    ]
    argv = [sys.executable, str(capture), "--tree", str(tree),
            "--evidence-dir", str(evidence), "--", *command]
    with evidence.with_suffix(".capture.log").open("x") as output:
        process = subprocess.Popen(argv, cwd=tree, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            status = process.wait()
        except BaseException:
            # Stop this capture and its Cargo children before restoring source.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            raise
    result = json.loads((evidence / "result.json").read_text())
    if status != result["exit_code"]:
        raise RuntimeError(f"capture status differs from its result: {evidence}")
    return result, (evidence / "command.log").read_text(errors="replace")


def require_pass(result, log, test):
    named = re.findall(rf"^test {re.escape(test)} \.\.\. ok$", log, re.MULTILINE)
    if result["exit_code"] != 0 or len(named) != 1:
        raise RuntimeError(f"unmutated named test did not pass exactly once: {test}")


def require_assertion_failure(result, log, test):
    named = re.findall(rf"^test {re.escape(test)} \.\.\. FAILED$", log, re.MULTILINE)
    section = re.search(
        rf"^---- {re.escape(test)} stdout ----\n(?P<body>.*?)(?=\n---- |\nfailures:|\Z)",
        log, re.MULTILINE | re.DOTALL,
    )
    body = section.group("body") if section else ""
    asserted = re.search(r"assertion(?: `[^\n]+`)? failed", body)
    panicked = re.search(
        rf"thread '{re.escape(test)}'(?: \(\d+\))? panicked at", body,
    )
    # Cargo prints a final `error: test failed` on a valid assertion failure.
    unexpected_error = any(
        not line.startswith("error: test failed,")
        for line in log.splitlines()
        if re.match(r"^error(?:\[E\d+\])?:", line)
    )
    if (result["exit_code"] != 101 or len(named) != 1 or not asserted
            or not panicked or unexpected_error
            or "test result: FAILED. 0 passed; 1 failed;" not in log):
        raise RuntimeError(f"mutant was not killed by its named assertion: {test}")


def run_mutant(tree, capture, evidence, mutant):
    path = tree / mutant["path"]
    source_stat = path.stat(follow_symlinks=False)
    if not stat.S_ISREG(source_stat.st_mode):
        raise RuntimeError(f"mutation target is not a regular file: {path}")
    original = path.read_bytes()
    if original.count(mutant["before"]) != 1:
        raise RuntimeError(f"mutation anchor must occur exactly once: {path}")
    changed = original.replace(mutant["before"], mutant["after"], 1)
    directory = evidence / mutant["name"]
    directory.mkdir()
    (directory / "original.rs").write_bytes(original)
    (directory / "mutant.rs").write_bytes(changed)
    report = {
        "source": mutant["path"], "test": mutant["test"], "replacement_count": 1,
        "original_sha256": sha256(original), "mutant_sha256": sha256(changed),
        "original_mode": stat.S_IMODE(source_stat.st_mode),
        "original_atime_ns": source_stat.st_atime_ns,
        "original_mtime_ns": source_stat.st_mtime_ns,
        "baseline_passed": False, "mutant_assertion_failed": False,
        "source_restored": False, "restored_passed": False,
    }
    applied = False
    interrupted = False
    try:
        result, log = run_arm(tree, capture, directory / "baseline", mutant["test"])
        require_pass(result, log, mutant["test"])
        report["baseline_passed"] = True
        if path.read_bytes() != original:
            raise RuntimeError(f"source changed during baseline: {path}")
        path.write_bytes(changed)
        applied = True
        result, log = run_arm(tree, capture, directory / "mutated", mutant["test"])
        require_assertion_failure(result, log, mutant["test"])
        report["mutant_assertion_failed"] = True
    except KeyboardInterrupt:
        interrupted = True
        report["error"] = "interrupted; restored arm remains unproved"
        raise
    except BaseException as error:
        report["error"] = str(error)
        raise
    finally:
        try:
            present = path.read_bytes()
            if present not in (original, changed):
                raise RuntimeError(f"concurrent source edit; refusing to overwrite it: {path}")
            path.write_bytes(original)
            path.chmod(stat.S_IMODE(source_stat.st_mode))
            if path.read_bytes() != original:
                raise RuntimeError(f"source restoration differs: {path}")
            # Mutation discipline requires a fresh mtime. Backdating to the
            # original mtime can make Cargo reuse the mutated binary.
            os.utime(path, None)
            restored_stat = path.stat()
            report["restored_mtime_ns"] = restored_stat.st_mtime_ns
            report["source_restored"] = (
                stat.S_IMODE(restored_stat.st_mode) == stat.S_IMODE(source_stat.st_mode)
            )
            if not report["source_restored"]:
                raise RuntimeError(f"source metadata restoration differs: {path}")
            if applied and not interrupted:
                result, log = run_arm(tree, capture, directory / "restored", mutant["test"])
                require_pass(result, log, mutant["test"])
                report["restored_passed"] = True
        finally:
            write_json(directory / "proof.json", report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--repository-root", type=Path, required=True,
                        help="main repository root that owns retained evidence")
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    tree = args.tree.resolve(strict=True)
    repository = args.repository_root.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    if not (tree / "Cargo.toml").is_file() or not (repository / ".git").exists():
        parser.error("tree and repository-root must identify repository roots")
    if (not evidence.is_relative_to(repository) or evidence == repository
            or ".cache" in evidence.parts or ".git" in evidence.relative_to(repository).parts):
        parser.error("evidence-dir must be new, under repository-root, and outside cache/.git")
    capture = tree / "docs/perf/2026.09/effects-response/tools/capture.py"
    if not capture.is_file():
        parser.error(f"capture helper is absent: {capture}")
    os.umask(0o077)
    evidence.mkdir(parents=True, exist_ok=False)
    signal.signal(signal.SIGTERM, stop)
    report = {
        "tree": str(tree), "repository_root": str(repository),
        "capture_sha256": sha256(capture.read_bytes()),
        "harness_sha256": sha256(Path(__file__).read_bytes()), "mutations": [],
        "passed": False,
        "limits": "Two named in-process regressions only; no live HTTP, guest, database, or full-suite proof. Interruptions and concurrent edits cannot pass.",
    }
    try:
        for mutant in MUTANTS:
            report["mutations"].append(run_mutant(tree, capture, evidence, mutant))
        report["passed"] = True
    except BaseException as error:
        report["error"] = str(error)
        raise
    finally:
        write_json(evidence / "result.json", report)
        print(f"passed={report['passed']} evidence={evidence}", flush=True)


if __name__ == "__main__":
    main()
