#!/usr/bin/env python3
"""Materialize all three packages against fresh PostgreSQL 18 databases.

Build target/debug/examples/materialize_package before reserving this gate's
machine gap. This script runs no Cargo or Git commands and owns one disposable
container. Evidence contains redacted commands, logs, and generated snapshots.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import time

PACKAGES = (
    ("receiving", "receiving", ("receiving",)),
    ("client_acme_receiving", "receiving", ("receiving", "client_acme_receiving")),
    ("wms", "wms", ("wms",)),
)
OWNER_LABEL = "wamn.generated-tui-materialize"


class GateError(RuntimeError):
    pass


def require(condition, detail):
    if not condition:
        raise GateError(detail)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def snapshot(tree, evidence, name):
    files = {}
    for package, _, _ in PACKAGES:
        root = tree / "packages" / package / "generated"
        require(root.is_dir() and not root.is_symlink(), f"missing generated root: {package}")
        for path in sorted(root.rglob("*")):
            require(not path.is_symlink(), "generated output contains a symlink")
            if path.is_dir():
                continue
            require(path.is_file(), "generated output is not a regular file")
            relative = path.relative_to(tree)
            data = path.read_bytes()
            target = evidence / name / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            files[str(relative)] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    write_json(evidence / f"{name}.json", files)
    return files


class Runner:
    def __init__(self, tree, evidence, password):
        self.tree, self.evidence, self.password = tree, evidence, password
        self.commands = []

    def clean(self, text):
        return text.replace(self.password, "[redacted]")

    def run(self, name, command, *, environment=None, input_data=None, timeout=120, check=True):
        command = list(map(str, command))
        filename = f"{len(self.commands):03d}-{name}.log"
        record = {"name": name, "command": command, "log": filename, "exit_code": None}
        self.commands.append(record)
        started, child, output, failure = time.monotonic(), None, b"", None
        print(f"Stage: {name}", flush=True)
        try:
            env = os.environ.copy()
            env.update(environment or {})
            child = subprocess.Popen(command, cwd=self.tree, env=env,
                                     stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                     start_new_session=True)
            output, _ = child.communicate(input=input_data, timeout=timeout)
        except BaseException as error:
            failure = error
            if child is not None:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                output, _ = child.communicate(timeout=10)
        finally:
            record["elapsed_seconds"] = round(time.monotonic() - started, 3)
            record["exit_code"] = child.returncode if child is not None else None
            record["failure"] = type(failure).__name__ if failure else None
            cleaned = self.clean(output.decode("utf-8", "replace"))
            (self.evidence / filename).write_text(cleaned)
            write_json(self.evidence / "commands.json", self.commands)
        if failure is not None:
            raise failure
        if check and record["exit_code"] != 0:
            raise GateError(f"{name}: exit {record['exit_code']}; see {filename}")
        return record["exit_code"], cleaned

    def cleanup(self, container, owner):
        status, output = self.run("inspect-owned-container", [
            "docker", "inspect", "--format",
            '{{.Id}} {{index .Config.Labels "' + OWNER_LABEL + '"}}', container,
        ], check=False, timeout=30)
        if status != 0:
            require("No such object" in output or "No such container" in output,
                    "cannot determine whether the owned container remains")
            return
        identity = output.strip().split()
        require(len(identity) == 2 and identity[1] == owner
                and re.fullmatch(r"[a-f0-9]{64}", identity[0]),
                "container ownership did not match; cleanup refused")
        self.run("remove-owned-container", ["docker", "rm", "--force", "--volumes", identity[0]], timeout=60)


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, default=Path(__file__).resolve().parents[5])
    parser.add_argument("--evidence-root", type=Path,
                        help="repository that retains evidence, defaults to --tree")
    parser.add_argument("--evidence-dir", required=True, type=Path,
                        help="new evidence directory inside the evidence repository")
    args = parser.parse_args()
    os.umask(0o077)
    tree = args.tree.resolve(strict=True)
    binary = tree / "target/debug/examples/materialize_package"
    require((tree / "Cargo.toml").is_file(), "--tree must name the repository root")
    require(binary.is_file() and os.access(binary, os.X_OK),
            "build target/debug/examples/materialize_package before running this gate")
    require(shutil.which("docker") is not None, "docker is required")
    require(not args.evidence_dir.exists() and not args.evidence_dir.is_symlink(),
            "evidence directory must be new")
    evidence = args.evidence_dir.resolve()
    evidence_root = args.evidence_root.resolve(strict=True) if args.evidence_root else tree
    require((evidence_root / ".git").exists(), "evidence root must be a repository")
    require(evidence.is_relative_to(evidence_root), "evidence directory must be inside the repository")
    for package, _, migrations in PACKAGES:
        root = tree / "packages" / package
        require(not evidence.is_relative_to(root / "generated"), "evidence cannot be generated output")
        require((root / "wamn.json").is_file(), f"missing package manifest: {package}")
        for migration_package in migrations:
            require(any((tree / "packages" / migration_package / "migrations").glob("*.sql")),
                    f"missing migrations: {migration_package}")
    evidence.mkdir(parents=True, mode=0o700)
    require(stat.S_IMODE(evidence.stat().st_mode) == 0o700, "evidence must have mode 0700")
    password, owner = secrets.token_hex(24), secrets.token_hex(12)
    container = "wamn-tui-materialize-" + owner
    runner = Runner(tree, evidence, password)
    result = {"tree": str(tree), "image": "postgres:18", "container": container,
              "packages": {}, "cleanup_complete": False, "passed": False, "failure": None}
    before, created = None, False
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    try:
        before = snapshot(tree, evidence, "before")
        created = True  # Inspect the exact ownership label even if docker run is interrupted.
        runner.run("start-postgres", [
            "docker", "run", "--detach", "--name", container,
            "--label", f"{OWNER_LABEL}={owner}", "--env", "POSTGRES_PASSWORD",
            "--publish", "127.0.0.1::5432", "postgres:18",
        ], environment={"POSTGRES_PASSWORD": password}, timeout=300)
        _, published = runner.run("postgres-port", ["docker", "port", container, "5432/tcp"])
        address = re.fullmatch(r"127\.0\.0\.1:(\d+)\s*", published)
        require(address is not None, "PostgreSQL must publish exactly one loopback port")
        port = int(address[1])
        psql = ["docker", "exec", "--interactive", "--env", "PGPASSWORD", container,
                "psql", "--host", "127.0.0.1", "--username", "postgres",
                "--no-psqlrc", "--set", "ON_ERROR_STOP=1", "--quiet"]
        pg_env = {"PGPASSWORD": password}
        deadline = time.monotonic() + 120
        while True:
            status, version = runner.run("postgres-ready", psql + [
                "--dbname", "postgres", "--tuples-only", "--no-align",
                "--command", "SHOW server_version_num",
            ], environment=pg_env, timeout=10, check=False)
            if status == 0:
                require(version.strip().isdigit() and 180000 <= int(version.strip()) < 190000,
                        "the disposable database is not PostgreSQL 18")
                result["server_version_num"] = int(version.strip())
                break
            require(time.monotonic() < deadline, "PostgreSQL did not become ready")
            time.sleep(0.5)
        for package, schema, migration_packages in PACKAGES:
            database = "wamn_materialize_" + package
            runner.run(f"{package}-database", psql + [
                "--dbname", "postgres", "--command", f'CREATE DATABASE "{database}"',
            ], environment=pg_env)
            runner.run(f"{package}-schema", psql + [
                "--dbname", database, "--command", f'CREATE SCHEMA "{schema}"',
            ], environment=pg_env)
            migrations = []
            for migration_package in migration_packages:
                for migration in sorted((tree / "packages" / migration_package / "migrations").glob("*.sql")):
                    relative = str(migration.relative_to(tree))
                    runner.run(f"{package}-{migration_package}-{migration.stem}",
                               psql + ["--dbname", database, "--file", "-"],
                               environment=pg_env, input_data=migration.read_bytes())
                    migrations.append(relative)
            url = f"postgresql://postgres:{password}@127.0.0.1:{port}/{database}"
            completed = []
            result["packages"][package] = {"database": database, "migrations": migrations,
                                           "completed_modes": completed}
            for label, mode in [("write", "write"), ("check-1", "check"), ("check-2", "check")]:
                runner.run(f"{package}-{label}", [binary, mode, tree / "packages" / package],
                           environment={"WAMN_SCHEMA_INTROSPECTION_PG_URL": url}, timeout=600)
                completed.append(mode)
    except BaseException as error:
        result["failure"] = runner.clean(str(error)) if isinstance(error, GateError) else type(error).__name__
    finally:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        try:
            if created:
                runner.cleanup(container, owner)
            result["cleanup_complete"] = True
        except Exception as error:
            result["cleanup_failure"] = runner.clean(str(error))
            result["failure"] = result["failure"] or "owned container cleanup failed"
        try:
            if before is not None:
                after = snapshot(tree, evidence, "after")
                changes = {"added": sorted(after.keys() - before.keys()),
                           "removed": sorted(before.keys() - after.keys()),
                           "modified": sorted(path for path in before.keys() & after.keys()
                                              if before[path] != after[path])}
                write_json(evidence / "changes.json", changes)
                result["changed_paths"] = sorted(path for paths in changes.values() for path in paths)
        except Exception as error:
            result["snapshot_failure"] = runner.clean(str(error))
            result["failure"] = result["failure"] or "generated output snapshot failed"
        result["passed"] = (result["failure"] is None and result["cleanup_complete"]
                            and len(result["packages"]) == len(PACKAGES)
                            and all(package["completed_modes"] == ["write", "check", "check"]
                                    for package in result["packages"].values()))
        write_json(evidence / "result.json", result)
    print(f"Materialization {'passed' if result['passed'] else 'failed'}; evidence: {evidence}")
    for path in result.get("changed_paths", []):
        print(f"Changed: {path}")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (GateError, OSError) as error:
        print(f"Materialization refused: {error}", file=sys.stderr)
        sys.exit(1)
