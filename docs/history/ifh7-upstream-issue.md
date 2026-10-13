# wamn-ifh7: draft upstream issue for runtime-operator

Draft for the owner (wamn-ifh7, owner ruling 2026-10-13). It is not filed. The owner files it under their own GitHub account, or does not.

Title: [BUG] runtime-operator exits on a first NATS connect `dial tcp ...: i/o timeout` and crash-loops through a scheduler NATS outage

## Versions

- runtime-operator chart 2.10.3, image `ghcr.io/wasmcloud/runtime-operator:2.10.3` (tag `v2.10.3`).
- First seen on 2.9.0 and 2.10.0.
- `runtime-operator/pkg/wasmbus/nats.go` and `runtime-operator/cmd/main.go` are the same in v2.10.0, v2.10.3 and `main`.
- nats.go v1.54.0 (from `runtime-operator/go.mod` at v2.10.3).
- Scheduler NATS: the chart's bundled NATS 2.12.8, with TLS on (the chart default).

## Reproduction

1. Install the chart with its defaults (operator and bundled scheduler NATS).
2. Stop the scheduler NATS so that its Service address does not answer. Example: the NATS pod or its node is gone, and nothing answers a TCP SYN.
3. While NATS is down, restart the operator pod.

We saw this in three runs (2.9.0 and 2.10.0). The operator container exits with code 1 5 s after start, with this log:

```
unable to create runtime operator ... dial tcp 10.96.167.147:4222: i/o timeout
```

The pod stays in CrashLoopBackOff until NATS comes back. While it is down, the operator reconciles no Host and no workload.

In the same runs, the running operator container also exited with code 0, 64 to 133 s after NATS stopped. We do not know the cause of that exit. The source fails liveness only on a closed connection (`cmd/main.go:349-356`). We report it for completeness. The startup exit is the defect that the source shows.

## Cause (source at tag v2.10.3)

`NatsConnectContext` (`runtime-operator/pkg/wasmbus/nats.go:73-98`) retries a first connection for `NatsInitialConnectWindow` (60 s, `nats.go:41`). It retries only when `worthRetrying` returns true. `worthRetrying` (`nats.go:108-129`) accepts these errors:

- `nats.ErrNoServers`, `nats.ErrTimeout`
- `syscall.ECONNREFUSED`, `syscall.EHOSTUNREACH`, `syscall.ENETUNREACH`
- a `*net.DNSError` with `IsNotFound` or `IsTemporary`

A dial that times out is not on this list:

1. nats.go v1.54.0 `createConn` returns the `dialer.Dial("tcp", host)` error unchanged (`nats.go:2515-2521`).
2. `connect` changes only a "connection refused" error to `ErrNoServers` (`nats.go:2987-2996`).
3. The result is a `*net.OpError` with `Timeout()` true. It is not `nats.ErrTimeout` and not `ETIMEDOUT`.

So `NatsConnectContext` returns after one 5 s dial (`nats.Timeout(5 * time.Second)`, `nats.go:63`). `NewEmbeddedOperator` returns the error, and `cmd/main.go:311-314` calls `os.Exit(1)`.

PR #5536 (commit 69b44598b, "wait out a NATS that is not up yet") added the window for this startup race. But a Service with no live backend, or a moved pod IP, gives an unanswered SYN. That error is not on the list.

No flag or chart value changes this. The retry list and the window are package variables. In `charts/runtime-operator/values.yaml`, `operator.probes` sets only the probes.

In short: a `net.Error` Timeout is not retried.

## Expected behavior

The operator backs off and stays up through a scheduler NATS outage.
