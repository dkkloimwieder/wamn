#!/usr/bin/env python3
"""Replay the runner's actual assertions against retained HTTP evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source-tree", type=Path, required=True)
parser.add_argument("--evidence-dir", type=Path, required=True)
args = parser.parse_args()
runner_path = args.source_tree / "tools/receiving-cluster-journey-run"
runner = runner_path.read_text()
committed = subprocess.check_output(
    ["git", "show", "HEAD:tools/receiving-cluster-journey-run"],
    cwd=args.source_tree,
    text=True,
)
root = Path(__file__).resolve().parents[2]
prior = root / "wasmcloud-2-9-cutover/live-receiving-009/journey"
failure = root / "receiving-postcommit/live-002/pair/baseline/failure-pods.json"
args.evidence_dir.mkdir(mode=0o700)
positive = args.evidence_dir / "positive"
negative = args.evidence_dir / "failed-pod"
positive.mkdir()
negative.mkdir()


def command(text, anchor, output):
    marker = f'>/dev/null "$evidence_dir/{output}"'
    end = text.index(marker) + len(marker)
    start = text.rindex(anchor, 0, end)
    return text[start:end]


checks = {
    "pod": (
        'jq -e --arg image "$host_image" --arg image_id "$host_runtime_digest"',
        "flow-http-probe-pod.json",
    ),
    "response": (
        'jq -e --arg host "$route_host" \'\n  . ==',
        "flow-http-response.json",
    ),
    "update": (
        'jq -e \'\n  type == "array" and length == 1 and\n  .[0].request_id == "materializer-order"',
        "materializer-update.json",
    ),
    "receipt": (
        'jq -e \'\n  type == "array" and length == 1 and\n  .[0].request_id == "materializer-receipt"',
        "materializer-receipt.json",
    ),
}
for _, filename in checks.values():
    shutil.copyfile(prior / filename, positive / filename)
failed = json.loads(failure.read_text())
failed["items"] = [
    pod
    for pod in failed["items"]
    if pod["metadata"]["labels"].get("job-name") == "flow-http-reachability"
]
assert len(failed["items"]) == 1
(negative / "flow-http-probe-pod.json").write_text(json.dumps(failed, indent=2) + "\n")
results = []
for name, directory, expected in [
    ("pod", positive, 0),
    ("response", positive, 0),
    ("update", positive, 0),
    ("receipt", positive, 0),
    ("pod", negative, 1),
]:
    anchor, filename = checks[name]
    actual = command(runner, anchor, filename)
    assert actual == command(committed, anchor, filename)
    pod = json.loads((directory / "flow-http-probe-pod.json").read_text())["items"][0]
    environment = dict(
        os.environ,
        evidence_dir=str(directory),
        route_host="receiving.localhost",
        host_image=pod["spec"]["containers"][0]["image"],
        host_runtime_digest=pod["status"]["containerStatuses"][0]["imageID"].split("@")[-1],
    )
    result = subprocess.run(
        ["bash", "-euo", "pipefail", "-c", actual],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    results.append(
        dict(
            assertion=name,
            fixture=str(directory / filename),
            extracted_command_sha256=hashlib.sha256(actual.encode()).hexdigest(),
            unchanged_from_source_head=True,
            expected_exit=expected,
            actual_exit=result.returncode,
            stderr=result.stderr,
        )
    )
    assert result.returncode == expected, results[-1]
syntax = subprocess.run(["bash", "-n", str(runner_path)], capture_output=True, text=True)
assert syntax.returncode == 0, syntax.stderr
receipt = dict(
    scope="offline retained-evidence assertion replay; no HTTP requests, builds, or live jobs",
    runner=str(runner_path),
    runner_sha256=hashlib.sha256(runner_path.read_bytes()).hexdigest(),
    positive_source=str(prior),
    failed_pod_source=str(failure),
    checks=results,
    syntax_exit_code=syntax.returncode,
    verdict="pass",
)
(args.evidence_dir / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps(receipt))
