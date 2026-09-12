#!/usr/bin/env python3
"""Prepare a narrow proof correction; do not modify the source worktree."""
import ast
import difflib
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909/tools/receiving-operator-recovery-run')
original = SOURCE.read_text()
proposed = original


def replace(old, new):
    global proposed
    assert proposed.count(old) == 1, old
    proposed = proposed.replace(old, new)


replace('\n\ndef classify_sample(line, expected_id):', '''

def operator_state(items):
    require(len(items) == 1, "expected one supervised operator Pod")
    pod = items[0]
    require(not pod["metadata"].get("deletionTimestamp"), "operator Pod unexpectedly terminating")
    statuses = [s for s in pod["status"].get("containerStatuses", []) if s["name"] == "runtime-operator"]
    require(len(statuses) == 1, "operator container status missing")
    status = statuses[0]
    return dict(pod_name=pod["metadata"]["name"], pod_uid=pod["metadata"]["uid"],
                container_id=status.get("containerID"), restart_count=status["restartCount"],
                image_id=status.get("imageID"), ready=status.get("ready", False),
                termination=status.get("lastState", {}).get("terminated"),
                state=status.get("state", {}))


def supervised_restart(previous, current, previous_log, events, since):
    # Native /healthz deliberately asks kubelet to restart a terminally closed
    # NATS connection. Ordinary reconnecting stays healthy and retries forever.
    require(current["pod_uid"] == previous["pod_uid"] and current["image_id"] == previous["image_id"],
            "operator Pod or image changed during the scheduler fault")
    require(current["restart_count"] == previous["restart_count"] + 1,
            "operator restart history is incomplete")
    require(current["container_id"] and current["container_id"] != previous["container_id"],
            "operator restart does not identify a new container")
    termination = current["termination"] or {}
    require(termination.get("containerID") == previous["container_id"],
            "previous operator termination does not identify the observed process")
    require(termination.get("exitCode") == 0 and termination.get("reason") == "Completed",
            "operator restart was not a graceful supervised exit")
    require("nats connection closed" in previous_log,
            "previous operator log does not establish terminal NATS closure")
    witnesses = []
    for event in events.get("items", []):
        obj = event.get("involvedObject", {})
        stamp = (event.get("series", {}).get("lastObservedTime") or event.get("lastTimestamp")
                 or event.get("eventTime") or event.get("metadata", {}).get("creationTimestamp"))
        if (obj.get("uid") == current["pod_uid"] and event.get("reason") == "Killing"
                and (event.get("reportingComponent") or event.get("source", {}).get("component")) == "kubelet"
                and "runtime-operator" in event.get("message", "")
                and "failed liveness probe" in event.get("message", "") and stamp
                and datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() >= since):
            witnesses.append(event)
    require(witnesses, "operator restart lacks a same-Pod kubelet liveness event from this fault")
    return dict(previous=previous, current=current, termination=termination,
                cause="terminal-nats-closure-and-kubelet-liveness-restart", events=witnesses)


def classify_sample(line, expected_id):''')

replace('''    def continuity(value, same_operator=True):
''', '''    operator_seen = operator_state(before["operator_pods"])
    operator_initial = dict(operator_seen)
    operator_transitions = []
    operator_fresh_after = 0.0
    scheduler_fault_started = None

    def operator_continuity(value):
        nonlocal operator_seen, operator_fresh_after
        current = operator_state(value["operator_pods"])
        require(current["pod_uid"] == operator_initial["pod_uid"] and
                current["image_id"] == operator_initial["image_id"],
                "operator Pod or image changed during the scheduler fault")
        if current["restart_count"] != operator_seen["restart_count"]:
            # Capture both sources before asserting a cause. A missing receipt
            # or an unexplained restart remains a failure, even if service returns.
            label = f"operator-transition-{current['restart_count']}"
            previous_log = run(label + "-previous-log", kube + ["-n", SYSTEM, "logs",
                "pod/" + current["pod_name"], "-c", "runtime-operator", "--previous", "--timestamps"], tolerate=True)
            event_log = run(label + "-events", kube + ["-n", SYSTEM, "get", "events",
                "--field-selector", "involvedObject.uid=" + current["pod_uid"], "-o", "json"], tolerate=True)
            write(label + "-state.json", dict(previous=operator_seen, current=current))
            try:
                events = json.loads(event_log)
            except ValueError:
                events = {}
            transition = supervised_restart(operator_seen, current, previous_log.decode(errors="replace"),
                                            events, scheduler_fault_started)
            operator_fresh_after = time.time()
            transition["observed_at"] = operator_fresh_after
            operator_transitions.append(transition)
            write("operator-supervision.json", dict(initial=operator_initial, transitions=operator_transitions))
            operator_seen = current
        else:
            require(current["container_id"] == operator_seen["container_id"],
                    "operator container changed without a recorded restart")
        return current["ready"]

    def continuity(value, same_operator=True):
''')
replace('''        if same_operator:
            require(pod_ids(value["operator_pods"], "runtime-operator") == original_operator,
                    "operator process changed during the scheduler outage")
''', '''        if same_operator == "supervised":
            return operator_continuity(value)
        if same_operator:
            require(pod_ids(value["operator_pods"], "runtime-operator") == original_operator,
                    "operator process changed before the scheduler fault")
            return True
        return len(value["operator_pods"]) == 1 and all(map(ready, value["operator_pods"]))
''')
replace('''            continuity(current, same_operator)
            # A fresh status proves the operator actually hears this fleet again.
            fresh = all(datetime.fromisoformat(h["status"]["lastSeen"].replace("Z", "+00:00")).timestamp() >= after
                        for h in current["hosts"])
            if all(map(ready, current["hosts"])) and fresh:
''', '''            operator_ready = continuity(current, same_operator)
            # A fresh status after any supervised transition proves the new
            # operator hears this fleet; the original 120-second clock is unchanged.
            fresh_after = max(after, operator_fresh_after) if same_operator == "supervised" else after
            fresh = all(datetime.fromisoformat(h["status"]["lastSeen"].replace("Z", "+00:00")).timestamp() >= fresh_after
                        for h in current["hosts"])
            if operator_ready and all(map(ready, current["hosts"])) and fresh:
''')
replace('''        scheduler_stopped = True
        run("stop-scheduler",''', '''        scheduler_stopped = True
        scheduler_fault_started = time.time()
        run("stop-scheduler",''')
replace('''            current = snapshot("scheduler-down")
            continuity(current)
''', '''            current = snapshot("scheduler-down")
            continuity(current, "supervised")
''')
replace('''        recovered("scheduler-recovery", resumed, True)
''', '''        recovered("scheduler-recovery", resumed, "supervised")
''')
replace('''                                  operator_before=original_operator, operator_after=new_operator,
''', '''                                  operator_before=original_operator, operator_after=new_operator,
                                  scheduler_operator_transitions=operator_transitions,
''')
replace('''        run("final-operator-log", kube + ["-n", SYSTEM, "logs", "deployment/runtime-operator", "--tail=2000"], tolerate=True)
''', '''        run("final-operator-log", kube + ["-n", SYSTEM, "logs", "deployment/runtime-operator", "--tail=2000"], tolerate=True)
        operator_pods_raw = run("final-operator-pods", kube + ["-n", SYSTEM, "get", "pods", "-l",
            "wasmcloud.com/name=runtime-operator", "-o", "json"], tolerate=True)
        try:
            final_operator_pods = json.loads(operator_pods_raw).get("items", [])
        except (ValueError, AttributeError):
            final_operator_pods = []
        # A deliberate rollout may already have removed the original Pod.
        # Preserve each current replacement's logs/events as well as old-Pod receipts.
        final_targets = {(operator_initial["pod_uid"], operator_initial["pod_name"])}
        final_targets.update((pod["metadata"]["uid"], pod["metadata"]["name"]) for pod in final_operator_pods)
        for index, (uid, name) in enumerate(sorted(final_targets)):
            label = f"final-operator-{index}"
            run(label + "-current-log", kube + ["-n", SYSTEM, "logs", "pod/" + name,
                "-c", "runtime-operator", "--timestamps", "--tail=2000"], tolerate=True)
            run(label + "-previous-log", kube + ["-n", SYSTEM, "logs", "pod/" + name,
                "-c", "runtime-operator", "--previous", "--timestamps"], tolerate=True)
            run(label + "-events", kube + ["-n", SYSTEM, "get", "events", "--field-selector",
                "involvedObject.uid=" + uid, "-o", "json"], tolerate=True)
''')

ast.parse(proposed, filename=str(SOURCE))
(HERE / 'receiving-operator-recovery-run.proposed').write_text(proposed)
(HERE / 'operator-supervision.patch').write_text(''.join(difflib.unified_diff(
    original.splitlines(keepends=True), proposed.splitlines(keepends=True),
    fromfile='a/tools/receiving-operator-recovery-run', tofile='b/tools/receiving-operator-recovery-run')))
(HERE / 'preparation.json').write_text(json.dumps({
    'source': str(SOURCE), 'source_sha256': hashlib.sha256(original.encode()).hexdigest(),
    'proposed_sha256': hashlib.sha256(proposed.encode()).hexdigest(),
    'scope': 'Offline preparation only; no source edits or live execution.',
}, indent=2) + '\n')
