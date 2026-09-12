#!/usr/bin/env python3
"""Compile the split transaction shape against shipped generated accessors."""

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
source = Path(__file__).with_suffix(".rs").resolve()
accessors = repository / "packages/receiving/generated/wamn/receiving_record_receipt.rs"
binding = repository / "components/data/postgres-statements"
inputs = [source, accessors, binding / "src/lib.rs", binding / "Cargo.toml",
          repository / "components/Cargo.toml"]
hashes = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in inputs}
with tempfile.TemporaryDirectory(prefix="wamn-claim-split-probe-") as scratch:
    manifest = Path(scratch) / "Cargo.toml"
    manifest.write_text(
        '[package]\nname = "wamn-claim-split-probe"\nversion = "0.0.0"\nedition = "2024"\n'
        '\n[workspace]\n\n[lib]\npath = ' + json.dumps(str(source)) + '\n'
        '\n[dependencies]\nwamn-postgres-statements = { path = '
        + json.dumps(str(binding)) + ' }\n'
    )
    command = ["cargo", "check", "--offline", "--manifest-path", str(manifest)]
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(repository / "target/claim-split-probe")
    environment["WAMN_SPLIT_PROBE_ACCESSORS"] = str(accessors)
    started = time.monotonic()
    with (output / "cargo.log").open("w") as log:
        result = subprocess.run(command, cwd=repository, env=environment,
                                stdout=log, stderr=subprocess.STDOUT, check=False)
    elapsed = time.monotonic() - started
    shutil.copy2(manifest, output / "Cargo.toml")
    lock = manifest.with_name("Cargo.lock")
    if lock.exists():
        shutil.copy2(lock, output / "Cargo.lock")
    record = {
        "command": command,
        "cwd": str(repository),
        "environment": {name: environment[name] for name in
                        ["CARGO_TARGET_DIR", "WAMN_SPLIT_PROBE_ACCESSORS"]},
        "exit_code": result.returncode,
        "elapsed_seconds": elapsed,
        "input_sha256": hashes,
        "observation": "split shape compiles" if result.returncode == 0 else "compile refused",
        "scope": "compile only; no runtime execution or database mutation",
    }
    (output / "result.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, indent=2))
    raise SystemExit(result.returncode)
