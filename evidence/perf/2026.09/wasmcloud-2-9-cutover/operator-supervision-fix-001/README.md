# Proposed operator recovery proof correction

The unchanged operator container assertion is stronger than the cutover charter.
Native 2.9 supports Kubernetes restarting an operator with a permanently closed NATS connection.
It also retries an established connection indefinitely, so an ordinary 150-second NATS outage does not explain the Receiving005 restart.
Keep the cause unresolved until the previous container log and kubelet events establish it.
Receiving005 remains a failed proof.

Native source at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` establishes these boundaries:

- `runtime-operator/pkg/wasmbus/nats.go:58–70` sets `MaxReconnects(-1)` and a one-second retry wait; its 60-second first-connect window is separate (`:28–41,73–97`).
- `runtime-operator/cmd/main.go:195–205` logs disconnect, reconnect and terminal closure. These callbacks do not exit.
- `runtime-operator/cmd/main.go:323–355` deliberately keeps reconnecting healthy and fails `/healthz` only after terminal closure so kubelet can restart the operator. `/readyz` uses Ping; it does not prove resumed heartbeat processing.
- `runtime-operator/cmd/main.go:309,338–340` uses the signal context and exits 1 on a manager error. A recorded exit 0 is compatible with orderly shutdown, but does not identify who requested it.
- `runtime-operator/cmd/main_test.go:51–74` checks connected and explicitly closed health. `pkg/wasmbus/nats_reconnect_test.go:28–40,46–117` checks unlimited reconnect settings and real subscription recovery across a server restart. Neither is a 150-second operator-container proof.
- The retained distributed `deployment-001/operator.json:102` has `/healthz` liveness every 20 seconds with threshold three, `/readyz` every ten seconds, startup `/healthz` every five seconds with threshold 24, `restartPolicy: Always`, and 45-second termination grace. These are the same probes in Receiving005's Pod snapshot.

The charter's stages 3–4 require NATS/operator recovery, preservation of the native fleet-deaf protection, and strict routing outcomes.
It does not require permanent operator-container identity.
Native source and the published image digest are separate identities; the captured new-container log does not establish the old binary's reconnect behavior or termination cause.
Do not describe the observed restart as an expected consequence of the 150-second interval.

The retained evidence is in main `docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-005/journey/operator-recovery/`:

- `phases.json` records scheduler absence for 150.850 seconds, 16:31:23.681–16:33:54.531 UTC, and all three native fleet-deaf Host guards.
- All three Host identities and host container IDs/restart counts/images remain unchanged through the failure snapshot.
- `0349-scheduler-recovery-operator-pods.stdout` records the same operator Pod UID and image, restart count 1→2, the original container's exit 0/Completed at 16:34:07, and the new container unready. The snapshot begins 16.285 seconds after scheduler restoration; this was not the 120-second deadline expiring.
- `0355-final-operator-log.stdout` contains only the new process startup and leader-election attempt. No previous-container log was captured. The journey's `failure-events.json` contains no operator events from `wamn-system`.
- The retained sampler has 30 requests timestamped inside the stopped interval: eight exact successful application responses and 22 transport failures. Final recovery and the deliberate operator rollout were not reached.

`operator-supervision.patch` changes only `tools/receiving-operator-recovery-run`, against clean source `0388e3bc98d231f1e69538e613ec33689c041653`.
The proposed helper keeps strict process continuity before the fault and keeps every Host identity/process/image check throughout.
During the shared scheduler fault, it permits only a same-Pod, same-image, contiguous container restart with an identified prior container, exit 0/Completed, a previous log showing terminal NATS closure, and a kubelet liveness-restart event for that Pod within this fault.
It records both log and events before asserting the cause; missing receipts, unrelated exits, OOM, missing restart history, and other Pod/image changes still fail.
This is a narrow native-supervision path, not general acceptance of operator restarts.

After a supported transition, the helper waits for operator readiness, fresh Host status written after the transition was observed, Host readiness, and the existing exact 200 application response.
The original recovery clock starts before restoring NATS and remains 120 seconds.
The 150-second outage, every-Host fleet-deaf guard, Host continuity functions, route response classifier, deliberate operator rollout, and separate 120-second rollout recovery remain intact.
The final verdict records any scheduler-phase operator transitions.

At each transition, the helper captures these existing-resource diagnostics through the owned kubeconfig/context:

```text
kubectl ... -n wamn-system logs pod/<observed-operator> -c runtime-operator --previous --timestamps
kubectl ... -n wamn-system get events --field-selector involvedObject.uid=<operator-Pod-UID> -o json
```

Cleanup also snapshots current operator Pods, then captures current logs, previous logs and UID-scoped events for each current Pod and the original Pod.
This preserves evidence from a deliberate replacement even if the old Pod has already disappeared.
Each raw output and exit is retained, including expected failures to read a deleted Pod.
The existing deployment-level current log capture remains.
Each command uses the existing 20-second Kubernetes request timeout and 30-second command bound.
There is no added sleep or longer acceptance deadline; two reads per transition, one final Pod snapshot and three final reads per distinct original/current Pod are the added runtime cost.
The external event NATS, fixed `kind-wamn`, chart values and production code are untouched.

`offline-validation.json` records Python syntax, unchanged route/Host functions and 150/120 constants, the correct continued refusal of Receiving005, and nine synthetic refusal controls.
The synthetic accepted transition proves only the proposed reducer behavior; it does not supply the missing live cause evidence.
No build, live command, source edit or Beads mutation ran.
Root owns application and the next fixed-source live run.
