#!/usr/bin/env python3
"""Capture one command and its source identity in a new evidence directory."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--tree", type=Path, required=True)
parser.add_argument("--evidence-dir", type=Path, required=True)
parser.add_argument("command", nargs=argparse.REMAINDER)
args = parser.parse_args()
command = args.command[1:] if args.command[:1] == ["--"] else args.command
if not command:
    parser.error("a command is required after --")
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)

def git(*arguments):
    return subprocess.check_output(["git", *arguments], cwd=tree)

def write_json(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")

paths = set(git("diff", "--name-only", "HEAD", "-z").decode().split("\0"))
paths.update(git("ls-files", "--others", "--exclude-standard", "-z").decode().split("\0"))
files = {}
for name in sorted(paths - {""}):
    path = tree / name
    if name.startswith((".beads/", "docs/perf/", "docs/poc/")):
        continue
    files[name] = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
write_json("source.json", {"head": git("rev-parse", "HEAD").decode().strip(),
                           "changed_source_sha256": files})
write_json("command.json", {"cwd": str(tree), "argv": command})
env = {key: value for key, value in os.environ.items()
       if not key.startswith(("WAMN_", "OTEL_", "GIT_", "PG"))
       and key not in {"DATABASE_URL", "CARGO_TARGET_DIR", "CARGO"}}
env.update(RUSTUP_TOOLCHAIN="1.98.0", RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2",
           KUBECONFIG="/dev/null", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
started = time.monotonic()
with (evidence / "command.log").open("w") as output:
    result = subprocess.run(command, cwd=tree, env=env, stdout=output, stderr=subprocess.STDOUT)
write_json("result.json", {"exit_code": result.returncode,
                           "elapsed_seconds": round(time.monotonic() - started, 3)})
print(f"exit_code={result.returncode} evidence={evidence}", flush=True)
raise SystemExit(result.returncode)
