#!/usr/bin/env python3
"""Run the Receiving composition against an owned disposable development environment."""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import re
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback
import urllib.error
import urllib.request

SERVICES = (
    "receiving-route-postgres", "authenticated-registry",
    "receiving-dev-nats", "receiving-dev-tempo",
)


class ProofFailure(RuntimeError):
    pass


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--proof-timeout", type=int, default=3600)
    return parser.parse_args()


def http_status(url):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code
    except (OSError, urllib.error.URLError):
        return None


def wait_until(label, check, timeout, alive=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if alive is not None and alive.poll() is not None:
            raise ProofFailure(f"{label}: owned process exited before readiness")
        if check():
            return
        time.sleep(0.5)
    raise ProofFailure(f"{label}: timed out")


def reserve_port():
    held = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    held.bind(("127.0.0.1", 0))
    return held, held.getsockname()[1]


def tcp_ready(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True
    except OSError:
        return False


class OwnedRun:
    def __init__(self, tree, scratch, environment):
        self.tree = tree
        self.scratch = scratch
        self.environment = environment
        self.children = []

    def start(self, name, command, *, environment=None, output=None, input_data=None):
        # Every process group is created here and belongs only to this run.
        log = open(self.scratch / f"{name}.log", "ab", buffering=0)
        try:
            child = subprocess.Popen(
                [str(part) for part in command], cwd=self.tree,
                env=environment or self.environment, stdin=subprocess.PIPE if input_data is not None else subprocess.DEVNULL,
                stdout=output if output is not None else log, stderr=log,
                start_new_session=True,
            )
        finally:
            log.close()
        self.children.append(child)
        if input_data is not None:
            child.stdin.write(input_data)
            child.stdin.close()
        return child

    def run(self, name, command, *, timeout=120, environment=None, output=None, input_data=None):
        print(f"Stage: {name}", flush=True)
        child = self.start(name, command, environment=environment, output=output, input_data=input_data)
        try:
            status = child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.stop(child)
            raise ProofFailure(f"{name}: timed out") from None
        if status != 0:
            raise ProofFailure(f"{name}: exit {status}; see private stage log")
        return child

    @staticmethod
    def stop(child):
        # killpg targets only the new session/group created by start().
        if child.poll() is not None:
            return
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGINT)
        try:
            child.wait(timeout=60)
            return
        except subprocess.TimeoutExpired:
            pass
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(timeout=5)
            return
        except subprocess.TimeoutExpired:
            pass
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)

    def stop_all(self):
        failed = False
        for child in reversed(self.children):
            try:
                self.stop(child)
            except (OSError, subprocess.TimeoutExpired):
                failed = True
        return not failed


def main():
    args = arguments()
    os.umask(0o077)
    tree = args.tree.resolve()
    target = tree / "target"
    proof = tree / "docs/perf/2026.09/generated-tui-parity/tools/receiving_pty.py"
    evidence = args.evidence_dir.resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    compose_file = tree / "test-support/infrastructure/std-virtualization.compose.yaml"
    for required in (proof, compose_file, tree / "apps/wamn_receiving/wamn.json", tree / "apps/client_acme_receiving/wamn.json"):
        if not required.is_file():
            raise ProofFailure(f"required source is absent: {required}")
    for name in ("wamn", "wamn-identity", "wamn-host", "wamn-scenario-worker", "wamn-receiving"):
        if not os.access(target / "debug" / name, os.X_OK):
            raise ProofFailure(f"required native artifact is absent: {name}")
    for command in ("docker", "cargo", "psql"):
        if shutil.which(command) is None:
            raise ProofFailure(f"required executable is absent: {command}")

    scratch = Path(tempfile.mkdtemp(prefix="wamn-receiving-parity-live-", dir="/tmp"))
    scratch.chmod(0o700)
    print(f"Private live-proof scratch: {scratch}", flush=True)
    project = "wamn-receiving-parity-" + secrets.token_hex(8)
    username = project
    temporary_container = project + "-htpasswd"
    environment = os.environ.copy()
    environment.update(RUSTUP_TOOLCHAIN="1.98.0", RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2",
                       WAMN_IDENTITY_BINARY=str(target / "debug/wamn-identity"))
    # The dev loop uses the lane's native target and normal apps/target.
    environment.pop("CARGO_TARGET_DIR", None)
    environment.pop("CARGO_BUILD_TARGET_DIR", None)
    held = {}
    ports = {}
    for name in ("postgres", "standard_registry", "registry", "nats", "tempo", "otlp", "gate"):
        held[name], ports[name] = reserve_port()
    htpasswd = scratch / "htpasswd"
    auth = scratch / ".dockerconfigjson"
    authority = f"127.0.0.1:{ports['registry']}"
    image = authority + "/wamn/flow-http:dev"
    environment.update({
        "WAMN_STD_VIRT_PG_PORT": str(ports["postgres"]),
        "WAMN_STD_VIRT_REGISTRY_PORT": str(ports["standard_registry"]),
        "WAMN_ROUTE_REGISTRY_PORT": str(ports["registry"]),
        "WAMN_ROUTE_REGISTRY_HTPASSWD": str(htpasswd),
        "WAMN_RECEIVING_DEV_NATS_PORT": str(ports["nats"]),
        "WAMN_RECEIVING_DEV_TEMPO_PORT": str(ports["tempo"]),
        "WAMN_RECEIVING_DEV_OTLP_PORT": str(ports["otlp"]),
    })
    (evidence / "ownership.json").write_text(json.dumps({"project": project, "ports": ports, "tree": str(tree)}, indent=2) + "\n")
    owned = OwnedRun(tree, scratch, environment)
    compose = ["docker", "compose", "--profile", "receiving-route", "-p", project, "-f", compose_file]
    substrate_owned = False
    htpasswd_owned = False
    success = False
    failure = None
    config = scratch / "environment/dev.json"
    started = time.monotonic()
    redactions = set()

    def interrupted(signum, _frame):
        raise ProofFailure(f"orchestrator interrupted by signal {signum}")

    previous_signals = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        guest_environment = environment | {"CARGO_TARGET_DIR": str(tree / "apps/target")}
        owned.run("build-http-route", ["cargo", "build", "--manifest-path", tree / "apps/Cargo.toml", "-p", "http-route", "--target", "wasm32-wasip2", "--locked", "--offline"], timeout=1800, environment=guest_environment)
        guest = tree / "apps/target/wasm32-wasip2/debug/http_route.wasm"
        if not guest.is_file() or guest.stat().st_size == 0:
            raise ProofFailure("http-route guest artifact is absent")
        artifacts = [guest, *(target / "debug" / name for name in
                              ("wamn", "wamn-identity", "wamn-host", "wamn-scenario-worker", "wamn-receiving"))]
        (evidence / "artifacts.json").write_text(json.dumps({
            str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in artifacts}, indent=2) + "\n")
        password = secrets.token_hex(32)
        redactions.add(password)
        htpasswd_owned = True
        with htpasswd.open("wb") as output:
            owned.run("create-registry-auth", ["docker", "run", "--rm", "-i", "--name", temporary_container, "--entrypoint", "htpasswd", "httpd:2-alpine", "-Bni", username], output=output, input_data=(password + "\n").encode())
        auth.write_text(json.dumps({"auths": {authority: {"username": username, "password": password}}}) + "\n")
        # Gate stays reserved until immediately before dev up; Compose ports are released together.
        for name in set(held) - {"gate"}:
            held.pop(name).close()
        substrate_owned = True
        owned.run("substrate-up", compose + ["up", "--detach", "--wait", "--wait-timeout", "90", "--no-deps", *SERVICES], timeout=150)
        wait_until("tempo ready", lambda: http_status(f"http://127.0.0.1:{ports['tempo']}/ready") == 200, 90)
        owned.run("postgres-ready", compose + ["exec", "-T", "-e", "PGPASSWORD=probe", "receiving-route-postgres", "psql", "-h", "127.0.0.1", "-U", "postgres", "-Atqc", "select 1"])
        owned.run("create-system-database", compose + ["exec", "-T", "-e", "PGPASSWORD=probe", "receiving-route-postgres", "psql", "-h", "127.0.0.1", "-U", "postgres", "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-c", "CREATE DATABASE wamn_system"])
        if http_status(f"http://{authority}/v2/") != 401:
            raise ProofFailure("registry does not enforce authentication")
        docker_auth = scratch / "docker"
        docker_auth.mkdir()
        (docker_auth / "config.json").write_bytes(auth.read_bytes())
        wash = subprocess.check_output([str(tree / "tools/install-wash")], cwd=tree, env=environment, text=True).strip()
        owned.run("push-http-route", [wash, "oci", "push", image, guest, "--insecure"], timeout=180,
                  environment=environment | {"DOCKER_CONFIG": str(docker_auth)})
        del password
        held.pop("gate").close()
        env_root = scratch / "environment"
        up = owned.start("dev-up", [
            target / "debug/wamn", "dev", "up",
            "--system-database-url", f"postgresql://postgres:probe@127.0.0.1:{ports['postgres']}/wamn_system",
            "--root", env_root, "--scenario-worker-binary", target / "debug/wamn-scenario-worker",
            "--gate-bind", f"127.0.0.1:{ports['gate']}",
            "--nats-url", f"nats://127.0.0.1:{ports['nats']}",
            "--tempo-query-url", f"http://127.0.0.1:{ports['tempo']}",
            "--otel-exporter-otlp-endpoint", f"http://127.0.0.1:{ports['otlp']}",
            "--component-artifact-base", authority + "/wamn/components",
            "--release-artifact-base", authority + "/wamn/releases",
            "--registry-auth-file", auth, "--route-host", "receiving.localhost",
            "--flow-http-workload-image", image, "--host-binary", target / "debug/wamn-host",
            "--package", tree / "apps/wamn_receiving", "--overlay-root", tree / "apps/client_acme_receiving",
        ])
        config = env_root / "dev.json"

        def ready():
            if not config.is_file() or "environment ready" not in (scratch / "dev-up.log").read_text(errors="replace"):
                return False
            try:
                document = json.loads(config.read_text())
            except (OSError, ValueError):
                return False
            return document.get("gate_url") == f"http://127.0.0.1:{ports['gate']}/authoring" and bool(document.get("operator_bearer_token")) and tcp_ready(ports["gate"])

        wait_until("dev up and Gate ready", ready, 300, alive=up)
        loop = owned.start("dev-run", [target / "debug/wamn", "dev", "--config", config,
                                             "--overlay-root", tree / "apps/client_acme_receiving", "--hold"])
        served = re.compile(r"^run served: (\S+) host=(\S+) target_instance=(\S+)$", re.MULTILINE)
        wait_until("served activation", lambda: served.search((scratch / "dev-run.log").read_text(errors="replace")),
                   args.proof_timeout, alive=loop)
        binding = served.search((scratch / "dev-run.log").read_text(errors="replace")).groups()
        document = json.loads(config.read_text())
        target_url = scratch / "target-database-url"
        target_url.write_text(document["target_database_url"])
        operator_pat = scratch / "operator-pat"
        operator_pat.write_text(document["operator_bearer_token"])
        (evidence / "binding.json").write_text(json.dumps(dict(zip(["url", "host", "target_instance"], binding)), indent=2) + "\n")
        proof_command = [sys.executable, proof, "--binary", target / "debug/wamn-receiving",
                         "--endpoint", binding[0], "--host", binding[1], "--target-instance", binding[2],
                         "--operator-pat-file", operator_pat,
                         "--target-postgres-url-file", target_url, "--evidence-dir", evidence / "client"]
        owned.run("receiving-composition-live", proof_command, timeout=180)
        success = True
    except BaseException as error:
        failure = type(error).__name__
        with (scratch / "orchestrator-error.log").open("w") as output:
            traceback.print_exc(file=output)
        raise
    finally:
        for sig in previous_signals:
            signal.signal(sig, signal.SIG_IGN)
        for reservation in held.values():
            reservation.close()
        if not owned.stop_all():
            success = False
            print("Owned process cleanup failed; see private logs", flush=True)
        with (scratch / "cleanup.log").open("ab", buffering=0) as cleanup:
            if htpasswd_owned:
                try:
                    subprocess.run(["docker", "rm", "--force", temporary_container], env=environment, stdout=cleanup, stderr=cleanup, timeout=30, check=False)
                except (OSError, subprocess.TimeoutExpired):
                    success = False
                    print("Owned htpasswd container cleanup failed", flush=True)
            if substrate_owned:
                try:
                    result = subprocess.run([str(part) for part in compose + ["down", "--volumes", "--remove-orphans"]], env=environment, cwd=tree, stdout=cleanup, stderr=cleanup, timeout=120, check=False)
                    cleaned = result.returncode == 0
                except (OSError, subprocess.TimeoutExpired):
                    cleaned = False
                if not cleaned:
                    success = False
                    print("Owned Docker project cleanup failed; see private cleanup.log", flush=True)
        for sig, handler in previous_signals.items():
            signal.signal(sig, handler)
        # Keep credentials in private scratch and publish only redacted diagnostics.
        for secret_file in [config, *scratch.glob("environment/*pat*.json"), auth]:
            if not secret_file.is_file():
                continue
            def collect(value, key=""):
                if isinstance(value, dict):
                    for child_key, child in value.items():
                        collect(child, child_key)
                elif isinstance(value, list):
                    for child in value:
                        collect(child, key)
                elif isinstance(value, str) and re.search(r"token|password|secret|auth", key, re.I):
                    redactions.add(value)
            collect(json.loads(secret_file.read_text()))
        for log in scratch.rglob("*.log"):
            text = log.read_text(errors="replace")
            for secret in sorted(redactions - {""}, key=len, reverse=True):
                text = text.replace(secret, "[redacted]")
            text = re.sub(r"(?i)(Bearer\s+)[^\s\"'<>]+", r"\1[redacted]", text)
            text = re.sub(r"(://)[^/@\s]+:[^/@\s]+@", r"\1[redacted]@", text)
            destination = evidence / "logs" / log.relative_to(scratch)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(text)
        result = {"passed": success, "failure": failure, "project": project,
                  "elapsed_seconds": round(time.monotonic() - started, 3), "private_scratch": str(scratch)}
        (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(f"Retained live-proof evidence: {evidence}", flush=True)
    if not success:
        raise ProofFailure("live proof or owned cleanup failed")
    print("Disposable Receiving composition live proof passed.", flush=True)


if __name__ == "__main__":
    try:
        main()
    except ProofFailure as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from None
    except Exception as error:
        print(f"orchestrator failed ({type(error).__name__}); inspect the private evidence directory", file=sys.stderr)
        raise SystemExit(1) from None
