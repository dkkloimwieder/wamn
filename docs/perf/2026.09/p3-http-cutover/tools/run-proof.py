#!/usr/bin/env python3
"""Capture one P3 cutover command and its source and artifact identities."""
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path

root = Path(__file__).resolve().parents[5]
name, *command = sys.argv[1:]
output = root / "docs/perf/2026.09/p3-http-cutover" / name
output.mkdir()
def git(*args):
    return subprocess.check_output(["git", *args], cwd=root)
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
patch = git("diff", "--binary", "HEAD", "--", ".", ":(exclude)docs/perf")
changed = git("diff", "--name-only", "HEAD", "--", ".", ":(exclude)docs/perf").decode().splitlines()
new = git("ls-files", "--others", "--exclude-standard", "--", ".", ":(exclude)docs/perf").decode().splitlines()
for name in new:
    result = subprocess.run(["git", "diff", "--no-index", "--binary", "--", "/dev/null", name], cwd=root, stdout=subprocess.PIPE, check=False)
    assert result.returncode == 1
    patch += result.stdout
changed += new
(output / "source.patch").write_bytes(patch)
source = {name: digest(root / name) if (root / name).is_file() else None for name in changed}
receipt = {
    "command": command,
    "capture_tool_sha256": digest(Path(__file__)),
    "cwd": str(root),
    "baseline": git("rev-parse", "HEAD").decode().strip(),
    "source_patch_sha256": hashlib.sha256(patch).hexdigest(),
    "source_sha256": source,
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(),
}
(output / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
start = time.monotonic()
with (output / "command.log").open("w") as log:
    result = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT)
receipt["exit_code"] = result.returncode
receipt["elapsed_seconds"] = time.monotonic() - start
receipt["log_sha256"] = digest(output / "command.log")
receipt["artifacts"] = {
    str(path.relative_to(root)): digest(path)
    for path in [root / "components/target/wasm32-wasip2/debug/http_route.wasm"]
    if path.is_file()
}
for name in re.findall(r"(?:Running|Executable)[^\n]*\(([^()]+)\)", (output / "command.log").read_text()):
    path = root / name
    if path.is_file():
        receipt["artifacts"][name] = digest(path)
receipt["source_changed_during_command"] = [
    name for name, identity in source.items()
    if (digest(root / name) if (root / name).is_file() else None) != identity
]
(output / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps({key: receipt[key] for key in ["command", "exit_code", "elapsed_seconds", "artifacts", "source_changed_during_command"]}, indent=2))
print("\n".join((output / "command.log").read_text().splitlines()[-35:]))
sys.exit(result.returncode)
