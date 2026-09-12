#!/usr/bin/env python3
import hashlib
import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[5]
evidence = Path(__file__).resolve().parent
paths = set(subprocess.check_output(["git", "diff", "--name-only", "--diff-filter=ACMRT"], cwd=root, text=True).splitlines())
paths.update(subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard"], cwd=root, text=True).splitlines())
files = []
for name in sorted(paths):
    path = root / name
    if name.startswith("docs/perf/2026.09/wasmcloud-2-9-cutover/") or not path.is_file():
        continue
    files.append({"path": name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
record = {
    "base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
    "branch": subprocess.check_output(["git", "branch", "--show-current"], cwd=root, text=True).strip(),
    "uncommitted": True,
    "changed_or_new_files": files,
    "deleted_files": subprocess.check_output(["git", "diff", "--name-only", "--diff-filter=D"], cwd=root, text=True).splitlines(),
}
(evidence / "source-inputs.json").write_text(json.dumps(record, indent=2) + "\n")
