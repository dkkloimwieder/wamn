#!/usr/bin/env python3
"""Capture the authorized serialized workspace sweep; never infer live arming."""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

COMMAND = [
    "cargo", "test", "--workspace", "--locked", "--offline", "--no-fail-fast", "--",
    "--include-ignored", "--nocapture", "--test-threads=1",
    "--skip", "regenerate_checked_in_journey_schema",
    "--skip", "regenerate_checked_in_dev_config_schema",
]
CLEAR_PREFIXES = ("WAMN_", "WASH_", "OTEL_", "GIT_", "PG")
CLEAR_NAMES = {"DB_URL", "DATABASE_URL", "CARGO_TARGET_DIR", "KUBECONFIG"}


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def clean_environment(ambient):
    return {k: v for k, v in ambient.items()
            if not k.startswith(CLEAR_PREFIXES) and k not in CLEAR_NAMES}


def source_receipt(tree):
    def git(*args):
        env = clean_environment(os.environ)
        env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        return subprocess.check_output(["git", *args], cwd=tree, env=env, text=True).strip()
    return {"head": git("rev-parse", "HEAD"),
            "status": git("status", "--porcelain=v1", "--untracked-files=all")}


def load_receipt():
    return {"utc": utc(), "load_1_5_15_minutes": os.getloadavg(),
            "logical_cpu_count": os.cpu_count()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-tree", type=Path, required=True)
    parser.add_argument("--expected-source", required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--host-nats-bin", type=Path)
    args = parser.parse_args()
    tree = args.source_tree.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    if evidence == tree or tree in evidence.parents:
        parser.error("evidence must be outside the clean source worktree")
    if evidence.exists():
        parser.error("evidence must be a fresh directory")
    before = source_receipt(tree)
    if before["head"] != args.expected_source or before["status"]:
        parser.error("source must be clean and match the full expected commit")
    if args.host_nats_bin and (not args.host_nats_bin.is_absolute()
                              or not args.host_nats_bin.is_file()
                              or not os.access(args.host_nats_bin, os.X_OK)):
        parser.error("--host-nats-bin must name an absolute executable file")
    evidence.mkdir(parents=True, exist_ok=False)
    write_json(evidence / "source-before.json", before)
    write_json(evidence / "command.json", COMMAND)
    (evidence / "source.txt").write_text(before["head"] + "\n")
    ambient = dict(os.environ)
    env = clean_environment(ambient)
    controlled = {"KUBECONFIG": "/dev/null", "GIT_CONFIG_GLOBAL": "/dev/null",
                  "GIT_CONFIG_NOSYSTEM": "1", "RUSTUP_TOOLCHAIN": "1.98.0",
                  "RUSTC_WRAPPER": "", "CARGO_BUILD_JOBS": "4"}
    env.update(controlled)
    armed = []
    if args.host_nats_bin:
        env["WAMN_HOST_LIVE_NATS_SERVER_BIN"] = str(args.host_nats_bin)
        env["WAMN_HOST_LIVE_EVIDENCE_DIR"] = str(evidence / "host-lifecycle")
        armed = ["WAMN_HOST_LIVE_NATS_SERVER_BIN", "WAMN_HOST_LIVE_EVIDENCE_DIR"]
    started = time.monotonic()
    receipt = {"started_utc": utc(), "exit_code": None}
    write_json(evidence / "load-before.json", load_receipt())
    result_code = 125
    try:
        with tempfile.TemporaryDirectory(prefix="wamn-final-sweep-") as scratch:
            env["TMPDIR"] = scratch
            for name in ("HELM_CACHE_HOME", "HELM_CONFIG_HOME", "HELM_DATA_HOME"):
                env[name] = str(Path(scratch) / name.lower())
            write_json(evidence / "environment-names.json", {
                "ambient_names": sorted(ambient),
                "removed_ambient_names": sorted(set(ambient) - set(clean_environment(ambient))),
                "controlled_names": sorted([*controlled, "TMPDIR", "HELM_CACHE_HOME",
                                            "HELM_CONFIG_HOME", "HELM_DATA_HOME"]),
                "explicitly_armed_names": armed,
                "child_environment_names": sorted(env),
                "values_recorded": False,
                "startup_burst_fixture_armed": False,
                "remaining_arming_status": "Unknown; names do not prove test execution.",
            })
            write_json(evidence / "run.json", receipt)
            with (evidence / "workspace.log").open("wb") as log:
                result_code = subprocess.run(COMMAND, cwd=tree, env=env, stdout=log,
                                             stderr=subprocess.STDOUT).returncode
    except KeyboardInterrupt:
        result_code = 130
        receipt["wrapper_error_kind"] = "KeyboardInterrupt"
    except OSError as error:
        receipt["wrapper_error_kind"] = type(error).__name__
    finally:
        receipt.update(finished_utc=utc(), wall_seconds=time.monotonic() - started,
                       exit_code=result_code)
        write_json(evidence / "run.json", receipt)
        (evidence / "exit-code.txt").write_text(str(result_code) + "\n")
        write_json(evidence / "load-after.json", load_receipt())
        after = source_receipt(tree)
        write_json(evidence / "source-after.json", after)
        write_json(evidence / "source-stability.json", {
            "same_commit": before["head"] == after["head"],
            "clean_before_and_after": not before["status"] and not after["status"],
        })
    return result_code if result_code >= 0 else 128 - result_code


if __name__ == "__main__":
    raise SystemExit(main())
