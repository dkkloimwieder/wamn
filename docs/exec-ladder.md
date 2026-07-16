# Execution conformance ladder

A graduated set of live-execution POCs (epic `wamn-ojm`) that prove flows execute
CORRECTLY on the **deployed** runner — outside a bench harness — climbing from a
single node to branching logic. Each rung is a small, repeatable, mutation-tested
conformance proof that the next rung extends.

The rungs are gated on the live runner (`deploy/runner.yaml`, wamn-fqg.8 — see
[run-queue.md](run-queue.md) § *Production runner*): the same `run-worker` service
that closes the `dispatcher → run_queue → runner` chain.

| Rung | Flow | Proves |
|------|------|--------|
| **1** (wamn-ojm.1) | `webhook-in → respond` | one node dispatched, run + node_runs recorded, result echoes input |
| 2 (wamn-ojm.2) | multi-node linear (transform chain) | correct sequencing of several nodes |
| 3 (wamn-ojm.3) | branching | port selection / conditional routing |

## The proof (`ladderproof`)

`crates/wamn-gates/src/ladderproof.rs` is a pure DB **client** — the
f1proof/apiproof shape. Unlike `runnerbench` (which instantiates the flowrunner
**in-proc** via `RunWorker` and drives the claim loop itself), `ladderproof`
never touches the component: it seeds ONE run the dispatcher way (write-ahead
`dispatched` row + queue row) and then WAITS for the **separately-deployed**
`run-worker` service to claim it, drive it, and record the result. It asserts
nothing about *how* the run was driven — only that the deployed runner produced
the correct terminal state. That separation is the "outside a bench harness"
point of the ladder.

### Rung 1 — `webhook-in → respond`

The fixture `deploy/ladder/rung1.flow.json` is a `manual`-trigger passthrough:

```
webhook-in (in) ──► respond (out)
```

A `manual` trigger means nothing auto-fires it — the proof seeds the run
directly, isolating the **runner** (the subject) from the trigger machinery
(cron/outbox, already gated by the dispatcher). Both nodes are passthrough, so
for a seeded input `X` the deployed runner produces:

* `runs.status = completed`, `runs.result_json = X` (the last node's payload),
  `trigger_source = manual`;
* two `node_runs` — `in` (seq 0) then `out` (seq 1), both `success` on the `main`
  port, each output echoing `X`.

`ladderproof` asserts exactly that. `--setup` provisions a fresh ephemeral schema
and registers the flow (the LOCAL self-contained path); without it, ladderproof
is a client against a schema the deploy pipeline already provisioned — so re-runs
never drop the schema out from under the live runner.

Because a directly-seeded manual run gets no doorbell, the runner picks it up on
its next idle poll (the poll-backoff backstop, ≤ the runner's max idle interval);
`--timeout-secs` covers that plus the drive.

## Gates

* **Unit / drift-guard** — `cargo test -p wamn-gates ladderproof`: the committed
  fixture parses + validates under the same `wamn-flow` engine the runner compiles
  it with, and describes the rung.
* **Local end-to-end** — a throwaway Postgres + a background `run-worker` process +
  `ladderproof --setup`. Repeatable + mutation-tested (fixture drift-guard, the
  echo assert, and an in-place broken-flow swap proving the gate catches a runner
  that drove the wrong node trace).
* **In-cluster gate of record** — `deploy/ladderproof-job.yaml` drives the real
  `deploy/runner.yaml` service over the shared Postgres. See
  [build-and-test.md](build-and-test.md) § *[EXEC-LADDER.1]*.

No guest or host change — `ladderproof` is gates-only (the flowrunner already
dispatches `webhook-in`/`respond`), so the runner reuses the fqg.8 `wamn-host`
image and only the `wamn-gates` image is rebuilt.
