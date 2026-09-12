import hashlib, json, os, subprocess, time
from pathlib import Path

tree = Path("/home/kaalin/.cache/wamn-lanes/consolidation-generator-20260912")
output = Path(__file__).resolve().parent
commands = [
    [
        "cargo",
        "+1.98.0",
        "test",
        "--locked",
        "--offline",
        "-p",
        "wamn-schema-generator",
        "--test",
        "generation",
        "generated_metadata_has_one_strict_canonical_reader",
        "--",
        "--exact",
        "--nocapture"
    ]
]
def git(*args):
    return subprocess.check_output(["git", "-c", "core.fsmonitor=false", *args], cwd=tree)
def save(name, value):
    (output / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
def source():
    paths = git("ls-files", "-z", "--cached", "--others", "--exclude-standard").decode().split("\0")
    return {name: hashlib.sha256((tree / name).read_bytes()).hexdigest()
            for name in sorted(set(paths)) if name and not name.startswith(".beads/") and (tree / name).is_file()}
head = git("rev-parse", "HEAD").decode().strip()
before = source()
save("source-before.json", before)
(output / "source.diff").write_bytes(git("diff", "--binary"))
(output / "status-before.txt").write_bytes(git("status", "--short", "--untracked-files=all"))
environment = dict(os.environ, RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2")
environment.pop("CARGO_TARGET_DIR", None)
results = []
for number, argv in enumerate(commands, 1):
    started = time.monotonic()
    with (output / f"command-{number}.stdout").open("wb") as out, (output / f"command-{number}.stderr").open("wb") as err:
        run = subprocess.run(argv, cwd=tree, env=environment, stdout=out, stderr=err)
    result = {"argv": argv, "cwd": str(tree), "source_head": head,
              "environment": {"RUSTC_WRAPPER": "", "CARGO_BUILD_JOBS": "2", "CARGO_TARGET_DIR": None},
              "exit_code": run.returncode, "seconds": time.monotonic() - started}
    results.append(result)
    save("commands.json", results)
    print(json.dumps(result), flush=True)
    if run.returncode:
        break
after = source()
save("source-after.json", after)
(output / "status-after.txt").write_bytes(git("status", "--short", "--untracked-files=all"))
passed = len(results) == len(commands) and all(row["exit_code"] == 0 for row in results)
result = {"passed": passed, "source_unchanged": before == after and git("rev-parse", "HEAD").decode().strip() == head,
          "commands": len(results), "binary_sha256": hashlib.sha256((tree / "target/debug/examples/materialize_package").read_bytes()).hexdigest()}
save("result.json", result)
print(json.dumps(result), flush=True)
raise SystemExit(0 if result["passed"] and result["source_unchanged"] else 1)
