#!/usr/bin/env python3
"""Capture the root workspace sweep without ambient live-service inputs."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--evidence-dir", type=Path, required=True)
args = parser.parse_args()
tree = Path(__file__).resolve().parents[5]
evidence = args.evidence_dir.resolve()
evidence.relative_to(tree)
evidence.mkdir(parents=True, exist_ok=False)
command = [
    "cargo", "test", "--workspace", "--locked", "--offline", "--no-fail-fast", "--",
    "--include-ignored", "--nocapture", "--test-threads=1",
    "--skip", "regenerate_checked_in_journey_schema",
    "--skip", "regenerate_checked_in_dev_config_schema",
]
env = {
    key: value for key, value in os.environ.items()
    if not key.startswith(("WAMN_", "OTEL_", "GIT_", "PG"))
    and key not in {"DATABASE_URL", "CARGO_TARGET_DIR"}
}
env.update(KUBECONFIG="/dev/null", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
           RUSTUP_TOOLCHAIN="1.98.0", RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2")
head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=tree, text=True).strip()
(evidence / "command.json").write_text(json.dumps(command, indent=2) + "\n")
(evidence / "source.txt").write_text(head + "\n")
with tempfile.TemporaryDirectory(prefix="wamn-workspace-sweep-") as scratch:
    env["TMPDIR"] = scratch
    for name in ("HELM_CACHE_HOME", "HELM_CONFIG_HOME", "HELM_DATA_HOME"):
        env[name] = str(Path(scratch) / name.lower())
    with (evidence / "workspace.log").open("w") as log:
        result = subprocess.run(command, cwd=tree, env=env, stdout=log, stderr=subprocess.STDOUT)
(evidence / "exit-code.txt").write_text(str(result.returncode) + "\n")
raise SystemExit(result.returncode)
