# Execution conformance ladder

A graduated set of live-execution POCs (epic `wamn-ojm`) that prove flows execute
CORRECTLY on the **deployed** runner — outside a bench harness — climbing from a
single node to branching logic. Each rung is a small, repeatable, mutation-tested
conformance proof that the next rung extends.

The rungs are gated on the live runner (`deploy/platform/runner.yaml`, wamn-fqg.8 — see
[run-queue.md](run-queue.md) § *Production runner*): the same `run-worker` service
that closes the `dispatcher → run_queue → runner` chain.

| Rung | Flow | Proves |
|------|------|--------|
| **1** (wamn-ojm.1) | `webhook-in → respond` | one node dispatched, run + node_runs recorded, result echoes input |
| **2** (wamn-ojm.2) | `webhook-in → transform{upper} → transform{reverse} → respond` | correct sequencing + payload threading across several nodes |
| **3** (wamn-ojm.3) | `webhook-in → conditional{true/false} → yes\|no → respond` | correct routing — the taken branch is selected, only it runs, its output merges |

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

The proof is rung-parameterised (`--rung <N>`): each rung is one or more
`RungCase`s, each carrying its fixture, seeded input, and the expected execution
`chain` — the ordered `(node_id, output, port)` list the runner must record.
`assert_run` is chain-driven: it checks the terminal state, the final result, and
every `node_runs` row against the chain (id, seq, status, **output port**, output,
and threading). A rung is one case (rungs 1/2) or several (rung 3 runs a true and
a false case over one flow); overall PASS requires every case to pass. `--setup`
registers every rung's flow so one ephemeral schema serves the whole ladder.

### Rung 1 — `webhook-in → respond`

The fixture `deploy/gates/ladder/rung1.flow.json` is a `manual`-trigger passthrough:

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

### Rung 2 — linear transform chain

The fixture `deploy/gates/ladder/rung2.flow.json` is a `manual`-trigger linear flow:

```
webhook-in (in) ──► transform{op: upper} (t1) ──► transform{op: reverse} (t2) ──► respond (out)
```

The flowrunner's legacy `transform` arm reads `config.op` (`upper` / `reverse`)
over the string payload, so the seeded input is a JSON **string**. For
`"abcDEF"` the deployed runner records:

| seq | node | input | output |
|-----|------|-------|--------|
| 0 | `in` | `"abcDEF"` | `"abcDEF"` |
| 1 | `t1` | `"abcDEF"` | `"ABCDEF"` |
| 2 | `t2` | `"ABCDEF"` | `"FEDCBA"` |
| 3 | `out` | `"FEDCBA"` | `"FEDCBA"` |

and `runs.result_json = "FEDCBA"`. `ladderproof` asserts two properties over
these rows:

* **Sequencing** — the `node_runs`, ordered by `seq`, are exactly `in, t1, t2,
  out` at seq `0..3`, each `success` on the `main` port.
* **Threading** — each node's recorded `input_json` equals the *prior* node's
  recorded `output_json` (or the run input at seq 0). This is what makes it a
  multi-node proof: the payload was threaded node-to-node, not recomputed from
  the trigger.

`upper` and `reverse` happen to commute on the *final* result, so a reordered or
dropped node leaves `result_json` unchanged — but it breaks the recorded
sequence/threading, which the per-row asserts catch (the in-place-reorder
mutant). The proof is the trace, not a final-result-depends-on-order property.

### Rung 3 — conditional branch + merge

The fixture `deploy/gates/ladder/rung3.flow.json` is a `manual`-trigger diamond:

```
                 ┌─true──► transform{expression: yes} (yes) ─┐
webhook-in (in) ─► conditional{expression: take} (cond)      ├──► respond (out)
                 └─false─► transform{expression: no}  (no)  ─┘
```

`out` is the **merge** node — both branches feed it, but the engine's single-token
walk means only the *taken* branch reaches it, so it runs once. The `expression`
config routes `cond`/`yes`/`no` to the standard-library JMESPath nodes (the
`conditional{expression}` node emits the payload UNCHANGED on the `true`/`false`
port by the predicate's truthiness — NOT the legacy passthrough-on-main arm);
each branch `transform` extracts its own field, giving a distinct output per path.

The seeded input carries the predicate and both branch payloads:
`{ "take": true|false, "yes": "took-yes", "no": "took-no" }`. The proof runs
**two** cases, one per direction:

| case | `cond` port | branch node | result |
|------|-------------|-------------|--------|
| `take: true` | `true` | `yes` → `"took-yes"` | `"took-yes"` |
| `take: false` | `false` | `no` → `"took-no"` | `"took-no"` |

Each case's `node_runs` is exactly `in, cond, <branch>, out` — length **4**, so the
untaken branch's node never runs. `ladderproof` asserts the ROUTING:

* **Port** — `cond`'s recorded `output_port` is `true` / `false` matching the
  predicate (rungs 1/2 carry `main` throughout; the chain now pins the port at
  every node, strengthening them too).
* **Single branch** — the chain length is 4 and the seq-2 node id is the taken
  branch (`yes` or `no`); a node_run for the other branch would break the count,
  and a run down the wrong branch breaks the node id + the merged result.
* **Threading** — `cond` passes the object through unchanged, the branch transform
  extracts its field, and `out` echoes it; each node's recorded input is the prior
  node's recorded output.

The in-place edge-swap mutant (swap the two `cond` edges' `from-port`s in the live
graph) routes the `true` input to `no` and vice versa — a WRONG-ROUTING failure
the gate catches (wrong branch id + wrong result), the rung-3 analog of rung 2's
reorder mutant.

## Gates

* **Unit / drift-guard** — `cargo test -p wamn-gates ladderproof`: each committed
  fixture parses + validates under the same `wamn-flow` engine the runner compiles
  it with, the rung-2 case pins the upper→reverse chain + threading relation, and
  the rung-3 cases pin the branching diamond (the `cond`/`yes`/`no` config, the
  `true`/`false` from-port edges, the merge) + both routing directions (cond port,
  branch node, result).
* **Local end-to-end** — a throwaway Postgres + a background `run-worker` process +
  `ladderproof --rung 3 --setup` (which drives both branch cases; then `--rung 2`
  and `--rung 1` client-only for the regressions). Repeatable + mutation-tested
  (`scratchpad/mutate_ojm3.py`): a fixture drift-guard, a gate **port** assert, and
  an in-place edge **swap** proving the gate catches a runner that routed down the
  wrong *branch*, not just its own arithmetic.
* **In-cluster gate of record** — `deploy/gates/ladderproof-job.yaml` (`--rung 3`, and
  `--rung 2` / `--rung 1` for the regressions) drives the real `deploy/platform/runner.yaml`
  service over the shared Postgres. See [build-and-test.md](build-and-test.md) §
  *[EXEC-LADDER.1/2/3]*.

No guest or host change — `ladderproof` is gates-only (the flowrunner already
dispatches `webhook-in`/`transform`/`respond`/`conditional{expression}` via the
wamn-nodes catch-all), so the runner reuses the fqg.8 `wamn-host` image and only
the `wamn-gates` image is rebuilt.
