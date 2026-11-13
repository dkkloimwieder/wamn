# 88 open items after the crate review

Ruled 2026-11-12: R-04, R-06 to R-12 and the environment spellings carried from tab 03. Under R-01 the revocation rule is still open (Summary). Everything else that was held, carried or unruled is settled by a later tab or by the code at dep7. Checked at `epic/dep7` (b94833fd) against the tab rulings and the code; nothing built or run.

## Settled

| Item | Settled by |
| --- | --- |
| R-01 auth cache and revocation | Tab 17 finding 10: the token map stays as a moka cache with a TTL and drop by principal (R-01 B in effect). Correction (2026-11-12): only the cache is settled. The revocation rule is not ruled (Summary, needs a ruling). |
| R-02 user access tokens | Tab 57 finding 8: no user PATs. |
| R-03 operation with no transaction on the new path | Code at dep7 (fact): `write_log.rs` gives an operation without SQL a two-step claim on its own connection (`WriteLogStore`, used by `execution/host/src/operation.rs`). R-03 A is done. |
| R-05 `wamn-client` | Tab 85 1 A: deleted. |
| Held: one scope record shared by plugins | Tab 25 1 A. |
| Held: signed-file routes on the entry path | Tab 25 2 B1. |
| Tab 26: where file byte routes live | Tab 26 ruling 2026-11-11 (file byte routes and model names). |
| Tab 20 finding 5 | Tab 72 1 A (B). |
| Tabs 24 (8), 27 (3), 31 (1, 3, check 5), 38 (2) | Index note of 2026-11-11 and tab 58. |
| Carried: SQL text in 130 files (tab 02) | U2 (tab 34) and tab 78: SQLx at build for all SQL. |

## Findings

**1. R-04: who owns cross-field manifest checks.**

Fact: tab 74 ruled that the manifest is validated once, at build. It did not say whether the catalog model (Rust) or KCL holds the cross-field rules. Tab 43 keeps the KCL binary; tab 80 1 A makes `wamn.k` the one authored source.

Options:

- A: The catalog model holds every rule that needs only the document. KCL does typing, defaults and its two merge checks. The generator keeps rules that need the catalog or SQL.
- B: KCL holds the cross-field checks.

Recommend A: one owner in Rust, run once at build.

**2. R-06: uncertain operation claims.**

Fact:

- An operation without SQL claims its key in two steps (`write_log.rs`): `CLAIM_SQL` commits the claim before the call, `FINISH_SQL` writes the result after it. Its effects are outside the database, for example an HTTP call through a connection.
- If the host stops between the two steps, the row keeps `result IS NULL`.
- A retry with the same key gets `intent-uncertain` at once, at any age. It gets the same answer while the first call is still running: nothing tells "still running" from "abandoned".
- There is no timeout. `audit_retention.rs::WRITE_LOG_RETENTION_SQL` deletes finished claims after 30 days; "uncertain claims never expire".
- The refusal text says "an operator resolves it" (`http-route/src/entry_response.rs::uncertain`), but `WriteLogStore::resolve` returns an error ("The client sends a new key"). `IntentStore::uncertain` and `IntentStore::resolve` have no product caller.
- Workflow runs: the 2026-10-28 ruling deleted their effect-uncertain path.

Why a timeout must not turn it into "failed": "failed" says the effect did not happen. The call may have reached the other system before the host stopped. A client that reads "failed" retries with a new key, and the effect happens twice. Only the other system knows.

Options:

- A: Implement `resolve` and a resolve verb. An operator checks the other system and records the outcome.
- B: Keep today's behaviour. Fix the refusal text (no operator step). Delete `IntentStore::uncertain`, `IntentStore::resolve`, `OperatorActionBasis`, `UncertainIntent`, their SQLite implementations and `refusal::resolved`.
- C: As B, plus one timeout. Before it, a retry gets `in-progress` (try again later). After it, the answer is a terminal `outcome-unknown`: the client checks the other system, then sends a new key. A late finish still writes the result, and later reads return it. Open claims expire with retention like finished ones.

Recommend C: a definite answer for each state, no operator step, and no false "failed". This replaces the earlier recommendation of B.

**3. R-07: per-record order of event runs.**

Fact:

- An event run is a workflow run started by a row change. The CDC reader publishes the change to the environment's event stream. Native admission (`wamn_postgres/event_batch.rs`) matches it to an event workflow and inserts one `wamn_run.runs` row and one `run_queue` row in one transaction.
- Run ids are `gen_random_uuid()` (`deploy/sql/run-state.sql`); a redelivered event is absorbed by `idempotency_key`.
- `run_queue.stream_seq` defaults to 0. `insert_run_queue_sql` writes only `env_key` and `run_id`; only a test writes `stream_seq`.
- The claim orders by `(available_at, stream_seq, run_id)`. With `stream_seq` at 0 and random run ids, two runs of one record follow no set order. Inference: the claim skips locked rows and has no per-record lock, so two hosts can run both at once.
- `queue/evt.rs::mint_evt_run_id` (`<flow>:evt:<seq>`) has no product caller, only `run-state/tests/queue.rs`. Correction: an earlier version of this tab said it sets the run id.
- Admission already has the stream sequence (`wamn_jetstream.rs`, `stream_seq: ack.sequence`).

Options:

- A: Required. Admission writes `stream_seq` and a record key on the queue row; the claim takes one run per record at a time, in stream order.
- B: Not required. Delete `stream_seq`, its place in the claim order and index, and `mint_evt_run_id` with its test.

Product decision. Example: a receipt's status changes received, then inspected, then stored within a second, and each change starts the same workflow. With B the three runs may run in any order or at once. Default (delete over add): B, unless such workflows must see changes in order.

**4. R-08: loopback in the connection network policy.**

Fact: `connection_http/network_policy.rs::allows` refuses only IPv4 link-local and the AWS IPv6 metadata address. Loopback and unspecified addresses are allowed. A project admin sets a connection's `base_url`.

Options:

- A: Keep loopback allowed.
- B: Refuse loopback and unspecified addresses. Local development targets use the dev path (tab 70).

Recommend B: this is a run-time egress check, not an apply-time check.

**5. R-09: TLS on the edge device.**

Fact: the edge device is the on-site computer at a plant that runs `services/edge` (`wamn-edge`, default build): one application release, a device loop (serial or Modbus) and a forward of each sample to the platform over HTTPS. Its local ingress serves the application's routes over plain HTTP; `services/edge/edge.example.toml` sets `listen = "0.0.0.0:8080"`. Tab 58 did not rule TLS.

Options:

- A: Listen on loopback by default; document a TLS front.
- B: TLS on the edge device itself, which needs certificate delivery.

Recommend A.

**6. R-10: two models in `wamn-catalog`.**

Fact: tabs 74 and 75 delete the flow artifact types, `sql_lex` and the effect projection; `profile.rs` stays on `pg_query`. The crate still holds the authored and build model beside the release and run-time model.

Options:

- A: Two modules in one crate.
- B: Two crates (about 220 import lines move).

Recommend A.

**7. R-11: route reuse age in the web front.**

Fact: the age delays new routes and rollback by the same amount. An earlier proposal makes `web.rs::Releases` hold the last good value.

Options:

- A: A few seconds.
- B: Longer.

Recommend A, as one value in the web front's configuration.

**8. R-12: upload size limit.**

Fact: `FileStore::stage` streams multipart into staging with no size bound; `file_route.rs` sets none.

Options:

- A: One host configuration value bounds `stage` (as tab 27 made pool size a host configuration value). The default is yours to set.

Recommend A.

**9. One environment, five spellings (tab 03 finding 9, carried).**

Fact:

- Spellings: `<org>--<project>--<env>` (registry key), `evt.<org>.<project>.<env>` (subject), `EVT_<n>_<org>_<n>_<project>_<n>_<env>` (stream), `<project>_<env>` (message id), a `__` form for CDC objects.
- Hand-written copies at dep7: the registry key in `control/registry/src/types.rs` and again in `identity/platform/src/connection_projection.rs` and `connections/administration.rs` (twice); the stream name in `control/provision/src/name.rs` and `apps/platform/events/wire/src/lib.rs`; the subject prefix in `control/environment/src/event_streams/plan.rs`.
- Tab 82 1 A orders every name org, environment, project.

Options:

- A: One name module in `wamn-registry` builds each spelling from the one environment value (each target system keeps the characters it allows). The hand-written `format!` copies call it.

Recommend A.

## Work, not rulings

- The `kind` loop measurement (tabs 70 1 A, 72 1 A, 86 5 A): a step of the implementation plan (owner, 2026-11-12). Default: local development moves to `kind`, and the host's local modes go.
- Whether the fork `dkkloimwieder/pg-walstream` can be public: yes (owner, 2026-11-12), so tab 87 finding 1 is A.
- Test lists for tabs 01–10: written 2026-11-12, in each tab.

## Rulings

2026-11-12, all ruled:

- R-04 A: the catalog model holds every rule that needs only the document; KCL does typing, defaults and its two merge checks; the generator keeps rules that need the catalog or SQL.
- R-06 C: a timeout separates `in-progress` from a terminal `outcome-unknown`; a late finish still writes the result; no operator step; `IntentStore::uncertain`, `IntentStore::resolve`, `OperatorActionBasis`, `UncertainIntent`, the SQLite implementations and `refusal::resolved` go; open claims expire with retention.
- R-07 B: workflows never get an ordering guarantee; `stream_seq`, its place in the claim order and index, and `mint_evt_run_id` with its test are deleted.
- R-08 B: the connection network policy refuses loopback and unspecified addresses.
- R-09: TLS on the edge device ingress is a need (owner); A: the ingress listens on loopback by default, and a TLS front is documented.
- R-10 A: two modules in one `wamn-catalog` crate.
- R-11 A: route reuse age of a few seconds, one value in the web front's configuration.
- R-12 A: one host configuration value bounds `FileStore::stage`; the default is the owner's to set.
- Finding 9 A: every environment spelling comes from one name module in `wamn-registry`; the hand-written copies go.
