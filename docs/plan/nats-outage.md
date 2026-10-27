# Scheduler NATS outage

The owner rulings below govern scheduler outage recovery.

## 1. Goal

When the scheduler NATS stops, moves to another pod, or comes back, the host and the runtime operator stay up. Each one connects again by itself, with no pod restart. The operator follows the accepted restart window below.

## 2. Fixed rules

- The host and the operator stay up through a scheduler NATS outage of any length. Neither one needs a pod restart to connect again.
- A process that cannot reach NATS says so in its log, with the cause of each failed attempt.
- No liveness probe fails while NATS is only unreachable. A liveness failure means that the process cannot recover by itself.
- The routes of a host keep answering during the outage, as today. Only work that needs the scheduler waits.
- WAMN uses upstream wasmCloud and async-nats as released. There is no WAMN fork, no local patch and no local override of the operator (owner ruling on `wamn-10yt.76`). A change goes through options that the upstream code already has, or through WAMN code.
- The tests are workspace tests that stop and restart a NATS container. No cluster run is part of this plan.

## 3. Evidence still required

See [native alignment](../architecture/native-alignment.md#current-limits) for operator recovery limits.

Accepted window (owner ruling 2026-10-13): through a scheduler outage, Kubernetes restarts the operator with backoff of at most 5 minutes.
The operator recovers when NATS returns. WAMN adds no code and no fork.

Capture the unexplained operator exit with code 0 at the next real scheduler move.
Keep the operator log, kubelet events, and probe results together.
Capture the host log during that move to establish why the host loses its connection.

## 4. Design

### 4.1 The host names the cause

The host builds its scheduler client itself, with async-nats 0.49.1, which is already a direct dependency (`services/host/Cargo.toml:22`). It keeps every option that `connect_nats` sets today and adds three:

1. `name`, set to the host name, so the NATS server's connection list shows which host is which.
2. An event callback that logs under its own tracing target with the client `name`, so its line no longer reads the same as the one of the `wasmcloud:nats` plugin. It also records the time of the last `Connected` event and counts the failed passes since then.
3. The default log filter of the host gains `async_nats::connector=debug`, so each failed attempt logs its server and its error.

Use these diagnostics to capture the cause during a real scheduler move.

### 4.2 The host connects again

Owner decision: use the client's own retry instead of a supervisor or client swap. Capture a real GKE move to establish the cause before reconsidering the candidate below.

The candidate: the host keeps the connect capability: the URL and the TLS file paths. A supervisor task watches the recorded state. When the scheduler client has not connected for a bound while a fresh test connection to the same URL with the same TLS files succeeds, the held client is stuck.

What the host then does depends on what `ClusterHostBuilder` allows at the pinned revision. If the client can be replaced in the running host, the host opens a new client and swaps it in. This is the `wamn-fwzc` pattern: open again from the kept capability, with no restart. If the builder cannot take a new client, a swap needs an upstream change, and the owner decides (section 5).

### 4.3 The operator stays up

The operator is upstream code, and the no-fork rule holds. Three changes stay inside it:

1. The operator runs as today. At runtime it already reconnects forever, so a new pod IP behind the Service reaches it.
2. The unexplained exit with code 0 is measured before anything changes: one outage with the kubelet events, the probe results and the operator log kept.
3. The first-connect exit is the `wamn-10yt.76` gap. With no patch and no override, only an upstream release that retries a network timeout removes it. Until that release, the operator keeps the kubelet restart with its back-off, and each restart tries again (owner ruling 2026-09-29). `wamn-ifh7` records the upstream version that fixes it when one appears.

## 5. Issues

One branch. Each issue lands with its tests. Only workspace tests run.

1. The cause. The host builds its scheduler client as in 4.1. A live workspace test starts NATS in a Docker container on its own network with the alias `nats` and TLS like the chart, and it starts the host against `nats://nats:4222`. It stops the container, removes it, and starts a new one with the same alias, which gets a new IP. The test then checks that the host connects again and that the debug log names each failed attempt. If the test reproduces the stuck client, the log names the cause. A second case pauses the server first, measures what the client does with a dead but open connection, unpauses it, and then moves it.
2. The fix. After issue 1 names the cause: the setting that removes it, or else the candidate of 4.2. The same live test passes with no host restart, and a heartbeat RPC answers after the new container is up.
3. The operator. It waits, and no cluster run is part of it. At the next scheduler NATS move on `wamn-dev`, the operator log, the kubelet events and the probe results are kept, so the code 0 exit gets its evidence from a real move. `wamn-ifh7` lists what to capture.
4. Closeout. `docs/architecture/native-alignment.md` and `docs/operations/cluster-tests.md` describe the reconnect. `docs/operations/gcp.md` loses the host delete of the NATS move workaround.

## 6. Out of scope

- The event NATS (`evt-nats`) and its JetStream clients. They are separate clients with their own outage behavior.
- A NATS cluster of more than one server.
- An upstream pull request, a WAMN fork or a local patch of wasmCloud or async-nats (owner ruling on `wamn-10yt.76`).
- A cluster run. A workspace test cannot start the operator, because it needs a Kubernetes API server.
