#!/usr/bin/env python3
"""Parse rendered arguments, then refuse at the log level before service startup."""
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[5]
evidence = Path(__file__).resolve().parent
renders = evidence.parent / "deployment-001"
records = []
for profile in ["default", "receiving", "wms"]:
    resources = json.loads((renders / f"host-{profile}.json").read_text())
    deployments = [item for item in resources if item["kind"] == "Deployment"]
    assert len(deployments) == 1
    deployment = deployments[0]
    containers = deployment["spec"]["template"]["spec"]["containers"]
    hosts = [item for item in containers if item["name"] == "host"]
    assert len(hosts) == 1
    host = hosts[0]
    environment = os.environ.copy()
    literal_environment = {
        item["name"]: item["value"] for item in host["env"]
        if "value" in item and item["name"].startswith(("WASH_", "WASMCLOUD_", "WAMN_", "OTEL_"))
    }
    environment.update(literal_environment)
    substitutions = {
        "WASMCLOUD_HOST_IP": "127.0.0.1",
        "WASMCLOUD_HOST_ENVIRONMENT": deployment["metadata"]["namespace"],
    }
    arguments = host["args"].copy()
    for index, argument in enumerate(arguments):
        for key, value in substitutions.items():
            argument = argument.replace("$(" + key + ")", value)
        arguments[index] = argument
    command = [str(root / "target/debug/wamn-host"), *arguments, "--log-level=cutover-parse-sentinel"]
    result = subprocess.run(command, env=environment, text=True, capture_output=True, timeout=10)
    (evidence / f"host-{profile}-arguments.stdout").write_text(result.stdout)
    (evidence / f"host-{profile}-arguments.stderr").write_text(result.stderr)
    passed = result.returncode == 1 and "invalid log level: cutover-parse-sentinel" in result.stderr
    records.append({
        "profile": profile, "command": command, "exit_code": result.returncode,
        "literal_environment_names": sorted(literal_environment),
        "field_substitutions": substitutions,
        "secret_environment": "not loaded",
        "argument_parse_passed": passed,
        "service_startup": "intentionally refused before observability or service code",
    })
(evidence / "host-arguments.json").write_text(json.dumps(records, indent=2) + "\n")
assert all(record["argument_parse_passed"] for record in records), records
