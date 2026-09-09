#!/usr/bin/env python3
"""Run the generated-operator live proof inside one reserved machine gap.

The guest build and the helper's native builds share that reservation.
Fresh disposable services and credentials live in private temporary scratch.
Only redacted evidence is exported beside this runner after owned cleanup.
"""
from __future__ import annotations

import argparse
import contextlib
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
import re
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
    parser.add_argument("--tree", type=Path, default=Path(__file__).resolve().parents[4])
    parser.add_argument("--evidence-dir", type=Path, help="new directory for redacted evidence")
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
        self.stages = []

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
        self.stages.append({"name": name, "command": list(map(str, command)),
                            "pid": child.pid, "exit_code": None})
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
        self.stages[-1]["exit_code"] = status
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
        for child, stage in zip(self.children, self.stages):
            stage["exit_code"] = child.poll()
        return not failed and all(child.poll() is not None for child in self.children)



def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_identity(tree):
    def git(*args, required=True):
        command = subprocess.run(["git", "-C", str(tree), *args], capture_output=True, timeout=30)
        if command.returncode:
            if required:
                raise ProofFailure("could not read source identity from Git")
            return None
        return command.stdout

    if git("diff", "--name-only", "--diff-filter=U").strip():
        raise ProofFailure("resolve Git conflicts before running the integration proof")
    names = set(git("ls-files", "--cached", "--others", "--exclude-standard", "-z").decode().split("\0"))
    prefixes = ("crates/", "services/", "components/", "packages/", "test-support/", ".cargo/")
    files = {}
    for name in sorted(names):
        if name in {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml"} or name.startswith(prefixes):
            path = tree / name
            if path.is_file():
                files[name] = sha256_file(path)
    merge_head = git("rev-parse", "--verify", "MERGE_HEAD", required=False)
    return {"head": git("rev-parse", "HEAD").decode().strip(),
            "merge_head": merge_head.decode().strip() if merge_head else None,
            "files": files}


def proof_redactor(proof):
    # Reuse the proof's credential rules without starting its main function.
    # The helper also imports the shared PTY parser without creating pycache.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("generated_operator_evidence", proof)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Redactor({})


def docker_empty(arguments, environment, scratch):
    try:
        result = subprocess.run(["docker", *arguments], env=environment,
                                capture_output=True, timeout=30)
        with (scratch / "cleanup-checks.log").open("ab") as log:
            log.write(result.stdout + result.stderr)
        return result.returncode == 0 and not result.stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return False


def export_evidence(tree, scratch, evidence, redactor, report):
    def private_values(value, secret=False):
        if isinstance(value, dict):
            for key, child in value.items():
                private_values(child, secret or key in {"data", "stringData"}
                               or bool(re.search(r"private|signing[_-]?key|seed|credential", key, re.I)))
        elif isinstance(value, list):
            for child in value:
                private_values(child, secret)
        elif secret and isinstance(value, str):
            redactor.secrets.add(value)

    complete = True
    for path in [*scratch.rglob("*.json"), scratch / ".dockerconfigjson"]:
        if not path.exists():
            continue
        try:
            if path.is_symlink():
                raise ValueError("private credential metadata cannot be a symlink")
            document = json.loads(path.read_text())
            redactor.collect(document)
            private_values(document)
        except (OSError, ValueError):
            complete = False
    if (scratch / "htpasswd").is_file():
        redactor.secrets.add((scratch / "htpasswd").read_text().strip())
    # Logs can contain JSON-escaped credentials as well as their original bytes.
    redactor.secrets.update(json.dumps(secret)[1:-1] for secret in list(redactor.secrets) if secret)
    redactor.secrets.discard("")
    ansi = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")

    def clean(text):
        text = ansi.sub("", text)
        text = re.sub(r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                      "[redacted private key]", text, flags=re.S)
        text = redactor.clean(text)
        text = text.replace(str(scratch), "<private-scratch>").replace(str(tree), "<tree>")
        if redactor.exposed(text):
            raise ProofFailure("credential remained after evidence redaction")
        return text

    report["redaction_complete"] = complete
    report["runner_sha256"] = sha256_file(Path(__file__).resolve())
    report["helper_sha256"] = sha256_file(tree / "services/ctl/tests/generated_operator_live.py")
    report["private_host_diagnostics_exported"] = False
    report["withheld_files"] = []
    for path in sorted(scratch.glob("*.log")):
        if path.is_file() and not path.is_symlink():
            if complete:
                (evidence / path.name).write_text(clean(path.read_text(errors="replace")))
            else:
                report["withheld_files"].append(path.name)
    helper_result = scratch / "proof/result.json"
    report["helper_passed"] = False
    if helper_result.is_file() and complete:
        document = json.loads(helper_result.read_text())
        report["helper_passed"] = document.get("passed") is True
        (evidence / "operator-result.json").write_text(clean(json.dumps(document, indent=2)) + "\n")
        for name in ("terminal.log", "diagnostic.txt"):
            path = scratch / "proof" / name
            if path.is_file() and not path.is_symlink():
                (evidence / name).write_text(clean(path.read_text(errors="replace")))
    report["passed"] = report["passed"] and complete and report["helper_passed"]
    (evidence / "result.json").write_text(clean(json.dumps(report, indent=2)) + "\n")
    manifest = "".join(f"{sha256_file(path)}  {path.name}\n" for path in sorted(evidence.iterdir())
                       if path.is_file())
    (evidence / "evidence.sha256").write_text(manifest)
    return report["passed"]


def main():
    args = arguments()
    os.umask(0o077)
    tree = args.tree.resolve(strict=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    evidence = (args.evidence_dir or Path(__file__).resolve().parent / "runs"
                / (stamp + "-" + secrets.token_hex(3)))
    if evidence.is_symlink() or evidence.exists():
        raise ProofFailure("evidence directory must be new and must not be a symlink")
    evidence = evidence.resolve()
    evidence.mkdir(mode=0o700, parents=True)
    target = tree / "target"
    proof = tree / "services/ctl/tests/generated_operator_live.py"
    compose_file = tree / "test-support/infrastructure/std-virtualization.compose.yaml"
    for required in (proof, compose_file, tree / "packages/receiving/wamn.json", tree / "packages/client_acme_receiving/wamn.json"):
        if not required.is_file():
            raise ProofFailure(f"required source is absent: {required}")
    for name in ("wamn", "wamn-host", "wamn-scenario-worker", "wamn-receiving-tui"):
        if not os.access(target / "debug" / name, os.X_OK):
            raise ProofFailure(f"required native artifact is absent: {name}")
    for command in ("docker", "cargo", "wash"):
        if shutil.which(command) is None:
            raise ProofFailure(f"required executable is absent: {command}")

    source_before = source_identity(tree)
    redactor = proof_redactor(proof)
    scratch = Path(tempfile.mkdtemp(prefix="wamn-generated-tui-live-"))
    if scratch.is_relative_to(tree) or scratch.is_relative_to(evidence):
        scratch.rmdir()
        raise ProofFailure("private scratch must be outside the source tree and evidence directory")
    scratch.chmod(0o700)
    print(f"Private live-proof scratch: {scratch}", flush=True)
    project = "wamn-generated-tui-" + secrets.token_hex(8)
    username = project
    temporary_container = project + "-htpasswd"
    environment = os.environ.copy()
    environment.update(RUSTUP_TOOLCHAIN="1.98.0", RUSTC_WRAPPER="", CARGO_BUILD_JOBS="2")
    # The dev loop uses the lane's native target and normal components/target.
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
    (scratch / "ownership.json").write_text(json.dumps({"project": project, "ports": ports, "tree": str(tree)}, indent=2) + "\n")
    owned = OwnedRun(tree, scratch, environment)
    compose = ["docker", "compose", "--profile", "receiving-route", "-p", project, "-f", compose_file]
    substrate_owned = False
    htpasswd_owned = False
    success = False
    failure = None
    cleanup_results = {}

    def interrupted(signum, _frame):
        raise ProofFailure(f"orchestrator interrupted by signal {signum}")

    previous_signals = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        guest_environment = environment | {"CARGO_TARGET_DIR": str(tree / "components/target")}
        owned.run("build-http-route", ["cargo", "build", "--manifest-path", tree / "components/Cargo.toml", "-p", "http-route", "--target", "wasm32-wasip2", "--locked", "--offline"], timeout=1800, environment=guest_environment)
        guest = tree / "components/target/wasm32-wasip2/debug/http_route.wasm"
        if not guest.is_file() or guest.stat().st_size == 0:
            raise ProofFailure("http-route guest artifact is absent")
        password = secrets.token_hex(32)
        redactor.secrets.add(password)
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
        if http_status(f"http://{authority}/v2/") != 401:
            raise ProofFailure("registry does not enforce authentication")
        owned.run("push-http-route", ["wash", "push", image, guest, "--insecure"], timeout=180, environment=environment | {"WASH_REG_USER": username, "WASH_REG_PASSWORD": password})
        del password
        held.pop("gate").close()
        env_root = scratch / "environment"
        up = owned.start("dev-up", [
            target / "debug/wamn", "dev", "up",
            "--system-database-url", f"postgresql://postgres:probe@127.0.0.1:{ports['postgres']}/postgres",
            "--root", env_root, "--scenario-worker-binary", target / "debug/wamn-scenario-worker",
            "--gate-bind", f"127.0.0.1:{ports['gate']}",
            "--nats-url", f"nats://127.0.0.1:{ports['nats']}",
            "--tempo-query-url", f"http://127.0.0.1:{ports['tempo']}",
            "--otel-exporter-otlp-endpoint", f"http://127.0.0.1:{ports['otlp']}",
            "--component-artifact-base", authority + "/wamn/components",
            "--release-artifact-base", authority + "/wamn/releases",
            "--registry-auth-file", auth, "--route-host", "receiving.localhost",
            "--flow-http-workload-image", image, "--host-binary", target / "debug/wamn-host",
            "--package", tree / "packages/receiving", "--overlay-root", tree / "packages/client_acme_receiving",
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
        # The live proof runs directly in this batch, never through another gap helper.
        proof_command = [sys.executable, proof, "--wamn", target / "debug/wamn",
                         "--config", config, "--overlay-root", tree / "packages/client_acme_receiving",
                         "--evidence-dir", scratch / "proof"]
        owned.run("generated-operator-live", proof_command, timeout=args.proof_timeout)
        success = True
    except BaseException as error:
        failure = type(error).__name__
        with (scratch / "orchestrator-error.log").open("w") as output:
            traceback.print_exc(file=output)
    finally:
        for sig in previous_signals:
            signal.signal(sig, signal.SIG_IGN)
        for reservation in held.values():
            reservation.close()
        cleanup_results["owned_processes_reaped"] = owned.stop_all()
        if not cleanup_results["owned_processes_reaped"]:
            success = False
            print("Owned process cleanup failed; see private logs", flush=True)
        cleanup_results["compose_down_succeeded"] = not substrate_owned
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
                cleanup_results["compose_down_succeeded"] = cleaned
                if not cleaned:
                    success = False
                    print("Owned Docker project cleanup failed; see private cleanup.log", flush=True)
        cleanup_results["htpasswd_container_absent"] = docker_empty(
            ["container", "ls", "--all", "--quiet", "--filter", f"name=^/{temporary_container}$"],
            environment, scratch,
        )
        cleanup_results["compose_project_containers_absent"] = docker_empty(
            ["container", "ls", "--all", "--quiet", "--filter", f"label=com.docker.compose.project={project}"],
            environment, scratch,
        )
        cleanup_results["compose_project_volumes_absent"] = docker_empty(
            ["volume", "ls", "--quiet", "--filter", f"label=com.docker.compose.project={project}"],
            environment, scratch,
        )
        cleanup_results["reserved_listeners_closed"] = all(
            not tcp_ready(port) for name, port in ports.items() if name != "standard_registry"
        )
        try:
            source_after = source_identity(tree)
            cleanup_results["source_inputs_unchanged"] = source_before == source_after
        except (OSError, ProofFailure, subprocess.SubprocessError):
            source_after = None
            cleanup_results["source_inputs_unchanged"] = False
        success = success and all(cleanup_results.values())
        try:
            success = export_evidence(tree, scratch, evidence, redactor, {
                "passed": success, "failure_type": failure, "compose_project": project,
                "cleanup": cleanup_results, "source_before": source_before,
                "source_after": source_after, "stages": owned.stages,
                "toolchain": "1.98.0", "native_target": "target",
                "guest_target": "components/target", "finished_utc": datetime.now(timezone.utc).isoformat(),
            })
        except (OSError, ValueError, ProofFailure) as error:
            success = False
            print(f"Evidence export failed ({type(error).__name__}); private logs retained", flush=True)
        for sig, handler in previous_signals.items():
            signal.signal(sig, handler)
        print(f"Private live-proof scratch: {scratch}", flush=True)
        print(f"Redacted integration evidence: {evidence}", flush=True)
    if not success:
        raise ProofFailure("live proof or owned cleanup failed")
    print("Disposable generated-operator live proof passed.", flush=True)


if __name__ == "__main__":
    try:
        main()
    except ProofFailure as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from None
    except Exception as error:
        print(f"orchestrator failed ({type(error).__name__}); inspect the private evidence directory", file=sys.stderr)
        raise SystemExit(1) from None
