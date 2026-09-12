import datetime, hashlib, json, os, stat, subprocess, time
from pathlib import Path

root = Path("/home/kaalin/.cache/wamn-lanes/consolidation-generator-20260912")
output = Path(__file__).resolve().parent
argv = ["cargo", "+1.98.0", "test", "--locked", "--offline", "-p", "wamn-execution-host", "--lib", "router_driver::native_policy::tests::", "--", "--nocapture"]
def git(*args):
    return subprocess.check_output(["git", "-c", "core.fsmonitor=false", *args], cwd=root)
def save(name, value):
    (output / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
def source():
    digest = hashlib.sha256()
    count = 0
    for name in sorted(set(git("ls-files", "-z").decode().split("\0"))):
        if not name or name.startswith(".beads/"):
            continue
        path = root / name
        data = os.readlink(path).encode() if path.is_symlink() else path.read_bytes()
        for value in [name.encode(), str(stat.S_IMODE(path.lstat().st_mode)).encode(), data]:
            digest.update(len(value).to_bytes(8, "big"))
            digest.update(value)
        count += 1
    return {"head": git("rev-parse", "HEAD").decode().strip(), "tracked_files": count, "sha256": digest.hexdigest()}
before = source()
save("source-before.json", before)
(output / "status-before.txt").write_bytes(git("status", "--short", "--untracked-files=all"))
environment = dict(os.environ, RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2")
environment.pop("CARGO_TARGET_DIR", None)
started_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
started = time.monotonic()
with (output / "command.stdout").open("wb") as out, (output / "command.stderr").open("wb") as err:
    run = subprocess.run(argv, cwd=root, env=environment, stdout=out, stderr=err)
seconds = time.monotonic() - started
finished_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
after = source()
save("source-after.json", after)
(output / "status-after.txt").write_bytes(git("status", "--short", "--untracked-files=all"))
result = {"argv": argv, "cwd": str(root), "environment": {"RUSTC_WRAPPER": "", "CARGO_BUILD_JOBS": "2", "CARGO_TARGET_DIR": None}, "started_at": started_at, "finished_at": finished_at, "seconds": seconds, "exit_code": run.returncode, "source_unchanged": before == after}
save("result.json", result)
print(json.dumps(result), flush=True)
raise SystemExit(0 if run.returncode == 0 and before == after else 1)
