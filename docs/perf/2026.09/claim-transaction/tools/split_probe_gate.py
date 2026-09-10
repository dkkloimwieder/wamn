#!/usr/bin/env python3
"""Require valid owning claims to compile and split commits to fail."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--repository", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
repository = args.repository.resolve()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
tools = Path(__file__).resolve().parent
original = tools / "split_probe.rs"
assert hashlib.sha256(original.read_bytes()).hexdigest() == (
    "9eb98872bac04d29f8a584633476c5062ff0cad555c4c9db0f3ebe6bcd3aba9e"
), "The original split probe must remain byte-identical."
accessors = repository / "packages/receiving/generated/wamn/receiving_record_receipt.rs"
wms_accessors = repository / "packages/wms/generated/wamn/inventory_split.rs"
binding = repository / "components/data/postgres-statements"
environment = os.environ.copy()
environment["CARGO_TARGET_DIR"] = str(repository / "target/claim-split-probe")
environment["WAMN_SPLIT_PROBE_ACCESSORS"] = str(accessors)
environment["WAMN_SPLIT_PROBE_WMS_ACCESSORS"] = str(wms_accessors)
# Each negative case requires a named Rust error at its offending source line.
cases = [
    ("valid", "split_probe_valid.rs", None, None, None, None),
    ("old-split", "split_probe.rs", None, "E0308", "&mut claim_transaction,", "mismatched types"),
    ("early-commit", "split_probe_reject.rs", "early-commit", "E0599", "pending.commit().await", "PendingClaim"),
    ("moved-transaction", "split_probe_reject.rs", "moved-transaction", "E0382", "transaction.commit().await", "moved value"),
    ("private-transaction", "split_probe_reject.rs", "private-transaction", "E0616", "pending.transaction.commit().await", "private"),
]
records = []
with tempfile.TemporaryDirectory(prefix="wamn-claim-compile-gate-") as scratch:
    manifest = Path(scratch) / "Cargo.toml"
    for name, filename, feature, code, source_line, message_fragment in cases:
        case_output = output / name
        case_output.mkdir()
        source = tools / filename
        manifest.write_text(
            '[package]\nname = "wamn-claim-split-probe"\nversion = "0.0.0"\nedition = "2024"\n'
            '\n[workspace]\n\n[lib]\npath = ' + json.dumps(str(source)) + '\n'
            '\n[dependencies]\nwamn-postgres-statements = { path = '
            + json.dumps(str(binding)) + ' }\n'
            '\n[features]\nearly-commit = []\nmoved-transaction = []\nprivate-transaction = []\n'
        )
        command = ["cargo", "check", "--offline", "--message-format=json", "--manifest-path", str(manifest)]
        if feature:
            command += ["--features", feature]
        started = time.monotonic()
        with (case_output / "cargo.jsonl").open("w") as stdout, (case_output / "cargo.stderr").open("w") as stderr:
            result = subprocess.run(command, cwd=repository, env=environment,
                                    stdout=stdout, stderr=stderr, check=False)
        elapsed = time.monotonic() - started
        errors = []
        for line in (case_output / "cargo.jsonl").read_text().splitlines():
            item = json.loads(line)
            if item.get("reason") == "compiler-message" and item["message"]["level"] == "error":
                errors.append(item["message"])
        matched = [error for error in errors if code is not None
                   and (error.get("code") or {}).get("code") == code
                   and message_fragment in error["message"]
                   and any(span["is_primary"] and Path(span["file_name"]).resolve() == source
                           and any(source_line in text["text"] for text in span["text"])
                           for span in error["spans"])]
        allowed_codes = {"E0308", "E0609"} if name == "old-split" else {code}
        passed = (result.returncode == 0 and not errors) if code is None else (
            result.returncode != 0 and bool(matched)
            and all((error.get("code") or {}).get("code") in allowed_codes for error in errors)
        )
        inputs = [Path(__file__).resolve(), source, accessors, wms_accessors,
                  binding / "src/lib.rs", binding / "Cargo.toml", repository / "components/Cargo.toml"]
        record = {
            "case": name, "command": command, "cwd": str(repository),
            "exit_code": result.returncode, "elapsed_seconds": elapsed, "passed": passed,
            "expected_error": code, "matched_errors": len(matched),
            "actual_error_codes": [(error.get("code") or {}).get("code") for error in errors],
            "input_sha256": {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in inputs},
        }
        shutil.copy2(manifest, case_output / "Cargo.toml")
        if manifest.with_name("Cargo.lock").exists():
            shutil.copy2(manifest.with_name("Cargo.lock"), case_output / "Cargo.lock")
        (case_output / "result.json").write_text(json.dumps(record, indent=2) + "\n")
        records.append(record)
        print(json.dumps({key: record[key] for key in ["case", "exit_code", "elapsed_seconds", "passed", "actual_error_codes"]}), flush=True)
        if not passed:
            break
summary = {
    "passed": len(records) == len(cases) and all(record["passed"] for record in records),
    "scope": "compile only; no runtime execution or database mutation",
    "environment": {key: value for key, value in environment.items() if key in
                    ["CARGO_TARGET_DIR", "WAMN_SPLIT_PROBE_ACCESSORS", "WAMN_SPLIT_PROBE_WMS_ACCESSORS"]},
    "cases": records,
}
(output / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
raise SystemExit(0 if summary["passed"] else 1)
