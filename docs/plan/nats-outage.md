# Scheduler NATS outage

Updated through: 2026-09-29, `main` at `4db74cb1c`. Accepted by the owner on 2026-09-29, with the rulings recorded on `wamn-gdex` and `wamn-ifh7`.

## 1. Goal

When the scheduler NATS stops, moves to another pod, or comes back, the host and the runtime operator stay up. Each one connects again by itself, with no pod restart. Today the host can stay disconnected until someone deletes its pod (`wamn-gdex`), and the operator exits and then crash-loops until NATS returns (`wamn-ifh7`).

The pattern is the one of `wamn-fwzc`. There, identity kept a database connection that Postgres closed, and only a restart helped. The fix kept the capability to connect, opened a closed connection again on its next use, and made `/healthz` take that path. A live test ended the sessions and waited for health to come back.

## 2. Fixed rules

- The host and the operator stay up through a scheduler NATS outage of any length. Neither one needs a pod restart to connect again.
- A process that cannot reach NATS says so in its log, with the cause of each failed attempt. Today the host logs `nats: IO error` without the cause.
- No liveness probe fails while NATS is only unreachable. A liveness failure means that the process cannot recover by itself.
- The routes of a host keep answering during the outage, as today. Only work that needs the scheduler waits.
- WAMN uses upstream wasmCloud and async-nats as released. There is no WAMN fork, no local patch and no local override of the operator (owner ruling on `wamn-10yt.76`). A change goes through options that the upstream code already has, or through WAMN code.
- The tests are workspace tests that stop and restart a NATS container. No cluster run is part of this plan.

## 3. Current state

Measured on `main` at `4db74cb1c` on 2026-09-29.

### 3.1 The host

| Place | Today |
| --- | --- |
| Client | `services/host/src/host.rs:723-736` calls `connect_nats` of wash-runtime (`crates/wash-runtime/src/washlet/mod.rs:640-728` at the pinned `fd2bbc0`). It passes the URL, the TLS files and `connect_retry`, and hands the client to `ClusterHostBuilder::with_nats_client` (`host.rs:1082`). The builder keeps the client for the life of the process. |
| Options | `connect_nats` sets an event callback, the TLS files and the request timeout. It sets no `max_reconnects`, `connection_timeout`, `reconnect_delay_callback` or `name`, so the async-nats 0.49.1 defaults apply (`async-nats-0.49.1/src/options.rs:103-121`). |
| First connect | `connect_nats` retries for `--nats-connect-timeout`. The GKE value is 60 seconds (`deploy/gcp/values-host-base.yaml:124`). |
| Reconnect | async-nats never stops: `max_reconnects` is `None`, and it gives up only on an authentication or authorization error (`connector.rs:256`, `:345-355`, `:373-376`). The delay is `min(2^(n-1)` ms, 4 s) (`connector.rs:173-181`). |
| DNS | Every attempt looks the name up again with `tokio::net::lookup_host` (`connector.rs:402`, `lib.rs:1689-1690`). A new pod IP behind the Service name reaches the client on its next attempt. |
| TLS | TLS is on by default in the chart (`charts/runtime-operator/values.yaml:24-27`). The host mounts `/runtime-cert` as a whole Secret, not by `subPath` (`templates/runtime/deployment.yaml:426-427`), so the kubelet updates the files in place. |
| The log line | `WARN NATS client error err=nats: IO error` comes from the callback at `washlet/mod.rs:679`. async-nats sends one `ClientError` event for each failed pass over its server list, with only the error kind (`connector.rs:262-265`). The cause is logged only at debug level on the target `async_nats` (`connector.rs:377-381`), and no deployment enables it. One pass is the 4 s delay plus a fast failure, so the line repeats every 4 s. |
| Probes | `/livez` checks the command loop, and `/readyz` checks ingress capacity (`washlet/mod.rs:366-395`, `host.rs:1098`, `:1120`). Neither depends on NATS, on purpose. |
| Test | `services/host/tests/native_lifecycle_live.rs:299-310` stops a NATS binary for 50 s and starts it again on the same loopback address, without TLS and without a name. The host answers a heartbeat after it. The test is ignored and needs `WAMN_HOST_LIVE_NATS_SERVER_BIN`. |

Why the host did not connect again on GKE is not known. The source shows a client that tries every 4 s, resolves the name each time and reads current TLS files. An `IO error` is a refused or reset TCP connection, a TLS handshake failure, a missing `INFO`, a broken pipe, or a server `-ERR` other than an authorization violation (`connector.rs:707-731`). A DNS failure would read `DNS error`, and a 5 s timeout would read `timed out`. So the name resolved, and the TCP or TLS step failed fast. A new host process with the same Secrets connected at once. The cause was dropped before the log, and the incident kept no debug log.

Two places print the same line: `washlet/mod.rs:679` for the scheduler client and `plugin/wasmcloud_nats/conn.rs:307` for the `wasmcloud:nats` plugin, whose server on GKE is the event NATS. Only the tracing target tells them apart, and the incident record does not name it.

### 3.2 The runtime operator

| Place | Today |
| --- | --- |
| Install | Chart `oci://ghcr.io/wasmcloud/charts/runtime-operator` 2.10.0 with `deploy/infra/values-wamn.yaml`, image `ghcr.io/wasmcloud/runtime-operator:2.10.0`. The chart has `restartPolicy: Always`, a startup probe (`/healthz`, 5 s × 24), a liveness probe (`/healthz`, 15 s delay, 20 s × 3) and a readiness probe (`/readyz`, 10 s × 3). WAMN sets no probe values. |
| Options | `pkg/wasmbus/nats.go:58-71`: `Timeout(5s)`, `ReconnectWait(1s)`, `MaxReconnects(-1)`, `PingInterval(1m)`, `MaxPingsOutstanding(1)`. Released `runtime-operator/v2.8.0` has the same `MaxReconnects(-1)`, which came with upstream `3ee85fd8` (v2.4.0). |
| First connect | `NatsConnectContext` retries for 60 s, but only on the errors that `worthRetrying` names: no servers, `ErrTimeout`, connection refused, host or network unreachable, and a DNS name that does not exist yet (`nats.go:108-129`). A `dial tcp ...: i/o timeout` is a general network timeout and is not on the list, so the call fails after one 5 s dial. `main.go:311-314` then exits 1. This is the upstream gap of `wamn-10yt.76`. |
| Liveness | `/healthz` fails only when the connection is closed for good (`main.go:319-356`). With `MaxReconnects(-1)` the connection never closes, so an outage alone does not fail liveness. |
| Flags | `nats-url`, the credentials and TLS flags, `nats-tls-first` and the manager flags (`main.go:86-147`). No flag sets the reconnect count, the first-connect window or the retry list. `NatsInitialConnectWindow` is a package variable. |
| Needs | The operator reads host heartbeats on `runtime.operator.heartbeat.>` and sends `workload.start`, `workload.status` and `workload.stop` to hosts. Without NATS it reconciles no Host and no workload. |

The source is the local checkout `f9b37fc`, whose chart and app versions read 2.10.0. No release tag of 2.10.0 is in the local wasmCloud clone, so the match with the image is by version string only.

The first-connect crash-loop follows from the source. The exit with code 0, 64 to 133 s into an outage, does not. Code 0 means the manager stopped on a signal, which is an outside SIGTERM, for example from the kubelet. The source fails liveness only on a closed connection, which `MaxReconnects(-1)` prevents. `docs/architecture/native-alignment.md:55-57` records it as the unresolved fault-time liveness delay. The results of that run under `~/.cache/wamn-sweeps/as5u` no longer exist.

### 3.3 Measured outage behavior of the host

Measured on 2026-09-29 by `services/host/tests/scheduler_nats_move_live.rs` at `0d5c962cc` and later, on one machine with Docker. NATS 2.12.8 runs with TLS as in the chart, under the alias `nats`. The host runs in a container on the same network.

| Case | What the host does |
| --- | --- |
| Move | NATS stops, its container goes, and a new one starts under the alias on a new IP after about 20 s. Each failed attempt logs its cause: `DNS error: ... Temporary failure in name resolution` while no container has the alias, then `IO error: Connection refused (os error 111)`. The host connects again 0.3 to 0.5 s after the new server is up, with no restart, and a heartbeat RPC answers. |
| Pause | NATS is paused, so it keeps its TCP connections open and answers nothing. The host logs nothing for 180.5 s. Then the client closes the connection, because three pings are outstanding: a ping goes every 60 s (`ping_interval`, `options.rs:111`), and more than two outstanding pings end the connection (`lib.rs:235`, `:513`). After that, each attempt reads `timed out` after the 5 s `connection_timeout` (`options.rs:104`), one every 5 s. On unpause, the next attempt connects, and a heartbeat RPC answers 5 ms later. The move that follows behaves as in the move case. |
| Both | `/livez` answers 200 during the outage, and the host container does not restart. |

Neither case reproduces the stuck client of GKE. The GKE log showed `IO error` every 4 s for 30 minutes. That is a fast refusal, not the `timed out` of a server that does not answer. A hypothesis, not established: the Service had no ready endpoint, and such a Service answers with a reset. `wamn-gdex` lists what to capture at the next real move.

## 4. Design

### 4.1 The host names the cause

The host builds its scheduler client itself, with async-nats 0.49.1, which is already a direct dependency (`services/host/Cargo.toml:22`). It keeps every option that `connect_nats` sets today and adds three:

1. `name`, set to the host name, so the NATS server's connection list shows which host is which.
2. An event callback that logs under its own tracing target with the client `name`, so its line no longer reads the same as the one of the `wasmcloud:nats` plugin. It also records the time of the last `Connected` event and counts the failed passes since then.
3. The default log filter of the host gains `async_nats::connector=debug`, so each failed attempt logs its server and its error.

After this, one outage run shows the cause. It is the first step, because the fix depends on it.

### 4.2 The host connects again

This section is a candidate, not the design. No supervisor and no swap is built until issue 1 names the cause. If a setting removes the cause, the setting is the fix and this section is dropped.

The candidate: the host keeps the connect capability: the URL and the TLS file paths. A supervisor task watches the recorded state. When the scheduler client has not connected for a bound while a fresh test connection to the same URL with the same TLS files succeeds, the held client is stuck.

What the host then does depends on what `ClusterHostBuilder` allows at the pinned revision. If the client can be replaced in the running host, the host opens a new client and swaps it in. This is the `wamn-fwzc` pattern: open again from the kept capability, with no restart. If the builder cannot take a new client, a swap needs an upstream change, and the owner decides (section 5).

### 4.3 The operator stays up

The operator is upstream code, and the no-fork rule holds. Three changes stay inside it:

1. The operator runs as today. At runtime it already reconnects forever, so a new pod IP behind the Service reaches it.
2. The unexplained exit with code 0 is measured before anything changes: one outage with the kubelet events, the probe results and the operator log kept.
3. The first-connect exit is the `wamn-10yt.76` gap. With no patch and no override, only an upstream release that retries a network timeout removes it. Until that release, the operator keeps the kubelet restart with its back-off, and each restart tries again (owner ruling 2026-09-29). `wamn-ifh7` records the upstream version that fixes it when one appears.

## 5. Issues

One branch. Each issue lands with its tests. Only workspace tests run.

1. The cause. The host builds its scheduler client as in 4.1. A live workspace test starts NATS in a Docker container on its own network with the alias `nats` and TLS like the chart, and it starts the host against `nats://nats:4222`. It stops the container, removes it, and starts a new one with the same alias, which gets a new IP. The test then checks that the host connects again and that the debug log names each failed attempt. If the test reproduces the stuck client, the log names the cause. A second case pauses the server first, measures what the client does with a dead but open connection, unpauses it, and then moves it. Section 3.3 gives the results.
2. The fix. After issue 1 names the cause: the setting that removes it, or else the candidate of 4.2. The same live test passes with no host restart, and a heartbeat RPC answers after the new container is up.
3. The operator. It waits, and no cluster run is part of it. At the next scheduler NATS move on `wamn-dev`, the operator log, the kubelet events and the probe results are kept, so the code 0 exit gets its evidence from a real move. `wamn-ifh7` lists what to capture.
4. Closeout. `docs/architecture/native-alignment.md` and `docs/operations/cluster-tests.md` describe the reconnect. `docs/operations/gcp.md` loses the host delete of the NATS move workaround. Cluster stages are noted as pending.

## 6. Out of scope

- The event NATS (`evt-nats`) and its JetStream clients. They are separate clients with their own outage behavior.
- A NATS cluster of more than one server.
- An upstream pull request, a WAMN fork or a local patch of wasmCloud or async-nats (owner ruling on `wamn-10yt.76`).
- A cluster run. A workspace test cannot start the operator, because it needs a Kubernetes API server.
