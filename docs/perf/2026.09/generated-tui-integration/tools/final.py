#!/usr/bin/env python3
"""Capture the focused integration repair, contract runner, and native rebuild."""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--evidence-dir", type=Path, required=True)
args = parser.parse_args()
tree = Path(__file__).resolve().parents[5]
evidence = args.evidence_dir.resolve()
evidence.relative_to(tree)
evidence.mkdir(parents=True, exist_ok=False)
commands = {
    "materialize-fixture": ["cargo", "test", "-p", "wamn-schema-generator", "--lib", "--locked", "--offline",
        "materialize::tests::wrapper_and_split_paths_are_identical_without_reintrospection", "--",
        "--exact", "--include-ignored", "--nocapture"],
    "contract-diff": ["tools/contract-diff", "run"],
    "build-wamn": ["cargo", "build", "-p", "wamn-ctl", "--bin", "wamn", "--locked", "--offline"],
}
env = {key: value for key, value in os.environ.items()
       if not key.startswith(("WAMN_", "OTEL_", "GIT_", "PG"))
       and key not in {"DATABASE_URL", "CARGO_TARGET_DIR", "CARGO"}}
env.update(RUSTUP_TOOLCHAIN="1.98.0", RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2",
           KUBECONFIG="/dev/null", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
(evidence / "source.txt").write_text(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=tree, text=True))
(evidence / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
results = {}
for name, command in commands.items():
    print("Stage: " + name, flush=True)
    with (evidence / (name + ".log")).open("w") as output:
        result = subprocess.run(command, cwd=tree, env=env, stdout=output, stderr=subprocess.STDOUT)
    results[name] = result.returncode
    (evidence / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    if result.returncode:
        raise SystemExit(result.returncode)
