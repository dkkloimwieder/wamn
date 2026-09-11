import datetime
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time

root = Path("/home/kaalin/dev/wamn")
out = Path(__file__).resolve().parent
expected_source = "47391bbc237f2e760f5cddb8b2e8f54a417ccc1f"
expected_binary = "38517fd9f33c6d72cc4ec4d1ef550b063dbd815a301cdd1777d093bc81f22153"
binary = root / "target/debug/wamn-receiving"
argv = ["python3", "-B", "apps/wamn_receiving/tests/operator_pty.py", "--binary", str(binary)]

def git(*args):
    return subprocess.check_output(["git", *args], cwd=root)

def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def write(name, value):
    (out / name).write_text(json.dumps(value, indent=2) + "\n")

def snapshot():
    files = []
    for raw in git("ls-files", "-z").split(b"\0"):
        if not raw:
            continue
        name = os.fsdecode(raw)
        if name.startswith((".beads/", "docs/perf/")):
            continue
        path = root / name
        info = path.lstat()
        data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        files.append({"path": name, "mode": f"{stat.S_IMODE(info.st_mode):04o}", "symlink": path.is_symlink(), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    digest = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return {"tracked_files": len(files), "excludes": [".beads/", "docs/perf/"], "sha256": digest, "files": files}

head_before = git("rev-parse", "HEAD").decode().strip()
assert head_before == expected_source
binary_before = {"path": str(binary), "bytes": binary.stat().st_size, "mode": f"{stat.S_IMODE(binary.stat().st_mode):04o}", "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
assert binary_before["sha256"] == expected_binary
before = snapshot()
write("source-before.json", before)
(out / "git-status-before.txt").write_bytes(git("status", "--porcelain=v1", "--untracked-files=all"))
command = {"source_commit": head_before, "command": argv, "cwd": str(root), "binary": binary_before, "environment": {"PYTHONDONTWRITEBYTECODE": "1"}, "started_utc": now()}
write("command.json", command)
environment = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
started = time.monotonic()
with (out / "run.log").open("wb") as log:
    completed = subprocess.run(argv, cwd=root, env=environment, stdout=log, stderr=subprocess.STDOUT)
elapsed = time.monotonic() - started
finished = now()
after = snapshot()
head_after = git("rev-parse", "HEAD").decode().strip()
binary_after = {"path": str(binary), "bytes": binary.stat().st_size, "mode": f"{stat.S_IMODE(binary.stat().st_mode):04o}", "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
(out / "git-status-after.txt").write_bytes(git("status", "--porcelain=v1", "--untracked-files=all"))
old = {entry["path"]: entry for entry in before["files"]}
new = {entry["path"]: entry for entry in after["files"]}
changed = [name for name in sorted(old.keys() | new.keys()) if old.get(name) != new.get(name)]
write("source-after.json", {"tracked_files": after["tracked_files"], "excludes": after["excludes"], "sha256": after["sha256"], "changed_paths": changed, "same_as_before": not changed})
result = {**command, "finished_utc": finished, "elapsed_seconds": elapsed, "exit_code": completed.returncode, "source_head_after": head_after, "source_unchanged": before == after and head_before == head_after, "binary_after": binary_after, "binary_unchanged": binary_before == binary_after, "log_sha256": hashlib.sha256((out / "run.log").read_bytes()).hexdigest(), "execution_scope": "One existing local Receiving PTY driver invocation. No Cargo, container, cluster or external agent invocation.", "passed": completed.returncode == 0 and before == after and head_before == head_after and binary_before == binary_after}
write("result.json", result)
print(json.dumps(result, indent=2), flush=True)
raise SystemExit(completed.returncode)
