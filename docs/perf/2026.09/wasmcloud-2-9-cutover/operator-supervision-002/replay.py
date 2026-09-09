#!/usr/bin/env python3
"""Replay the captured operator transition and refuse broken evidence."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import runpy
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", required=True, type=Path)
parser.add_argument("--evidence-root", required=True, type=Path)
parser.add_argument("--baseline", default="e7033f72ca32e92b02c3bbe1dc4af420cb23b145")
args = parser.parse_args()
actual = args.evidence_root / "live-receiving-006/journey/operator-recovery"
state = json.loads((actual / "operator-transition-3-state.json").read_text())
events = json.loads((actual / "0329-operator-transition-3-events.stdout").read_text())
previous_log = (actual / "0328-operator-transition-3-previous-log.stdout").read_text()
since = json.loads((actual / "phases.json").read_text())["scheduler-stopped"]["started"]
source = args.source / "tools/receiving-operator-recovery-run"
current = runpy.run_path(str(source))["supervised_restart"]
old_source = subprocess.check_output(["git", "-C", str(args.source), "show", args.baseline + ":tools/receiving-operator-recovery-run"], text=True)
old_globals = {"__name__": "captured_baseline"}
exec(compile(old_source, "captured-baseline", "exec"), old_globals)
inputs = [state["previous"], state["current"], previous_log, events, since]
try:
    old_globals["supervised_restart"](*inputs)
except RuntimeError as error:
    baseline_refusal = str(error)
else:
    raise AssertionError("original assertion unexpectedly accepts the captured timeout")
accepted = current(*inputs)
assert accepted["cause"] == "kubelet-liveness-http-timeout"
# Retain the previously supported terminal-close branch as a synthetic control.
terminal = copy.deepcopy(inputs)
terminal[2] = "nats connection closed"
terminal[3]["items"] = [x for x in terminal[3]["items"] if x.get("reason") != "Unhealthy"]
assert current(*terminal)["cause"] == "terminal-nats-closure-and-kubelet-liveness-restart"
mutations = []
def refused(name, change):
    value = copy.deepcopy(inputs)
    change(value)
    try:
        current(*value)
    except RuntimeError as error:
        mutations.append({"name": name, "result": "refused", "error": str(error)})
    else:
        raise AssertionError("accepted mutant: " + name)

def mutate_events(value, key, replacement):
    for event in value[3]["items"]:
        if event.get("reason") in {"Unhealthy", "Killing"}:
            if key.startswith("object."):
                event["involvedObject"][key[7:]] = replacement
            else:
                event[key] = replacement

refused("different operator Pod", lambda v: v[1].update(pod_uid="another-pod"))
refused("different operator image", lambda v: v[1].update(image_id="another-image"))
refused("missing restart history", lambda v: v[1].update(restart_count=v[0]["restart_count"] + 2))
refused("unchanged container", lambda v: v[1].update(container_id=v[0]["container_id"]))
refused("wrong previous process", lambda v: v[1]["termination"].update(containerID="another-container"))
refused("failed previous process", lambda v: v[1]["termination"].update(exitCode=1))
refused("non-graceful termination", lambda v: v[1]["termination"].update(reason="OOMKilled"))
refused("missing Killing event", lambda v: v[3].update(items=[x for x in v[3]["items"] if x.get("reason") != "Killing"]))
refused("missing timeout event", lambda v: v[3].update(items=[x for x in v[3]["items"] if x.get("reason") != "Unhealthy"]))
refused("wrong event Pod", lambda v: mutate_events(v, "object.uid", "another-pod"))
refused("wrong timeout container", lambda v: mutate_events(v, "object.fieldPath", "spec.containers{another-container}"))
refused("stale fault evidence", lambda v: mutate_events(v, "lastTimestamp", "2026-01-01T00:00:00Z"))
refused("wrong event reporter", lambda v: mutate_events(v, "reportingComponent", "another-controller"))
refused("readiness timeout only", lambda v: [x.update(message=x.get("message", "").replace("Liveness probe failed:", "Readiness probe failed:")) for x in v[3]["items"]])
refused("wrong health endpoint", lambda v: [x.update(message=x.get("message", "").replace("/healthz", "/readyz")) for x in v[3]["items"]])
result = {"result": "pass", "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
          "baseline_commit": args.baseline, "baseline_refusal": baseline_refusal,
          "captured_timeout_result": accepted, "synthetic_terminal_closure_control": "pass",
          "mutations": mutations, "scope": "recorded restart evidence only; live recovery is still required"}
output = Path(__file__).parent / "receipt.json"
output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
print(json.dumps({"result": "pass", "refused_mutations": len(mutations), "receipt": str(output)}))
