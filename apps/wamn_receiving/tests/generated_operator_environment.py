#!/usr/bin/env python3
"""Run the live operator test inside the existing owned service and PostgreSQL fixtures."""

import os
from pathlib import Path
import subprocess
import sys


def main():
    os.umask(0o077)
    source = Path(__file__).resolve().parents[3]
    if not (source / ".git").is_file():
        raise RuntimeError("run this test from an owned linked worktree")
    root = Path(os.environ["WAMN_TIMINGS_ROOT"]) / "operator"
    root.mkdir(mode=0o700)
    environment = os.environ.copy()
    for target, original in {
        "WAMN_DEV_ENV_NATS_URL": "WAMN_RECEIVING_DEV_NATS_URL",
        "WAMN_DEV_ENV_TEMPO_QUERY_URL": "WAMN_RECEIVING_DEV_TEMPO_QUERY_URL",
        "WAMN_DEV_ENV_OTEL_EXPORTER_OTLP_ENDPOINT": "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
        "WAMN_DEV_ENV_HOST_BIN": "WAMN_RECEIVING_DEV_HOST_BIN",
        "WAMN_DEV_ENV_ROUTE_HOST": "WAMN_ROUTE_HOST",
    }.items():
        environment[target] = environment[original]
    environment["WAMN_DEV_ENV_ROOT"] = str(root / "environment")
    environment["WAMN_DEV_ENV_PLATFORM_DOMAIN"] = "example.invalid"
    wamn = environment["WAMN_RECEIVING_DEV_BIN"]
    subprocess.run([
        wamn, "dev", "up", "--package", str(source / "apps/wamn_receiving"),
        "--package", str(source / "apps/client_acme_receiving"),
    ], env=environment, check=True)
    return subprocess.call([
        sys.executable, str(source / "apps/wamn_receiving/tests/generated_operator_live.py"),
        "--wamn", wamn, "--config", str(root / "environment/dev.json"),
        "--overlay-root", str(source / "apps/client_acme_receiving"),
        "--evidence-dir", str(root / "result"),
    ], env=environment)


if __name__ == "__main__":
    sys.exit(main())
