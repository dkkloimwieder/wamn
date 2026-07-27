# POC plan — F0–F4 under the callable-flow spec (r6, self-contained)

**r6, 2026-07-26, against `docs/FLOW-SPEC.md` rev 18 and `main` @ `9ddcd7d`.**
Self-contained: no reference to prior plan revisions is needed to implement
it. This revision incorporates the review rounds to date — run context replaces all
payload-threading workarounds (spec §4.4: every completion emission is
`{output, ctx?}`, reads via `context()`, a `ctx` write **replaces** the
document, merge is authored via `merge()`), the two-stage invocation
handshake, `service`-mode F4→F2, query-mode `RETURNING` CTEs, and the
completed mechanical-delta list.

| Flow | Entry | Exposure | Completion | Lands with |
|---|---|---|---|---|
| F0 echo (tiny) | `request` | HTTP attachment | respond | **Wave 1** — the §17 two-commit pure fixture |
| F1 receipt-received | `request` | HTTP attachment | respond or fail | **Wave 1** (exercises a slice across Phases 1–4; the black-box milestone) |
| F2 disposition-recommendation | `request` | internal attachment | respond or fail | **Wave 2** |
| F3 stale-hold escalation | `cron` | cron source + attachment | §9.9 natural completion or fail | **Wave 1** (exercises a slice across Phases 2A–3) |
| F4 disposition-recorded | `event` | event registration (live table) | §9.9 natural completion or fail | **Wave 2** (invokes F2) |

**Wave 2 proves exactly these F4→F2 behaviors and nothing broader**:
occurrence-keyed exactly-once child creation; wake-at-child-release;
revocation gating creation (with the before/after-creation split); and
pre-release cancellation via child-generation seizure. Phase 5's remaining
gates — lost-wake reconciliation, stale `wait_generation` rejection,
post-release child-failure isolation, refusal to cancel a released child,
and Appendix B's saga — are **not** exercised by this POC and close in
Phase 5's own acceptance tables.

## F0 — echo

```
request → shape (transform, pure) → respond 200
```

Exists because F1 is effectful and cannot honestly measure the §9.8
two-commit pure path. F0 is that measurement fixture: admission commit,
release-and-complete commit, nothing else.

## F1 — receipt-received

```
request
  → normalize-receipt          custom, pure (manifest purity: pure → replay)
  → resolve-and-persist        postgres-query CTE, QUERY mode + RETURNING
  → references-valid?          conditional
      false → invalid-reference fail  {code, status: 400}
      true  → evaluate-specs    custom, pure exact-decimal
            → create-holds      postgres-query CTE, QUERY mode + RETURNING
            → shape-response    transform, pure
            → respond 200

normalize-receipt.error → invalid-input fail {code, status: 400}
```

Entry `input-schema`: object shape, required fields, decimal **strings**,
nonempty lines, no unknown properties. Schema violation = 400, **no run**
(spec §4.1). Unknown supplier/site/material = admitted business request →
`fail` (spec §2.2 restored contract: `{code, message?, status}`, default
400) → durable **failed** run with `caller_http_status = 400`.

HTTP attachment (exposure, not graph): `POST /receipts`; `body → root`;
`auth-source: erp-api-keys`; `idempotency: required`; response/run
deadlines.

**Both CTEs run in query mode with `RETURNING`, and their replay result is
pinned deterministic**: plain `ON CONFLICT DO NOTHING RETURNING` returns
**empty** on conflict — a replay after a committed-but-unrecorded attempt
would emit different rows than the original and break §9.2 replay equality
at `respond`. The CTEs therefore use read-back-on-conflict (upsert-`RETURNING`,
or `WITH ins AS (INSERT … ON CONFLICT DO NOTHING RETURNING *) SELECT … UNION
SELECT existing rows`) with `ORDER BY line_no`, so first execution and every
replay return **identical rows in identical order**. Recovery: CTEs
`idempotent-with-key` on natural keys; pure nodes `replay`.

## F2 — disposition-recommendation

```
request
  → recommend-disposition      custom component, zero imports
  → respond 200
```

Contract (adopts the existing component's vocabulary — `decision` matches
the catalog column):

```json
in:  { "hold": {}, "history": [], "decision": "accept" }
out: { "recommendation": "reject", "confidence": "0.93", "matched": false }
```

No id echo — the disposition id stays in F4's run context (below), so F2's
contract is exactly recommendation logic. The component's manifest declares
`purity: pure` (spec §10.3) — the trusted assertion authorizing `replay`. Named component edits: accept
`decision`; `confidence` f64 → decimal string; add `matched`.

Internal attachment with caller policy allowing F4, **actor mode
`service`** — the event materializer admits F4 with no client principal to
inherit, F2 is pure and needs no caller authority, and a synthetic
inheritable event principal is machinery with no customer.

## F3 — escalate-stale-holds

```
cron
  → cutoff-at-48h      time-shift over scheduled-at (RFC 3339 in; -48h)
                       ctx: "@"        -- time-shift already emits
                                       -- {"cutoff": value}; wrapping it
                                       -- again would double-nest
  → next-stale-hold    postgres-query LIMIT 1: stale AND NOT escalated,
                       param context().cutoff
  → found?             conditional over rows
      false → §9.9 natural completion (unwired port)
      true  → mark     transform "@"  ctx: "merge(context(), {hold: rows[0]})"
            → notify-manager   http-request, params context().hold,
                               occurrence-keyed
            → escalate-head    postgres, id = context().hold.id
            → next-stale-hold  (loop)

notify-manager.error → notification-failed fail
```

The graph contains no schedule and no respond. Cron source + attachment
(exposure): `schedule: 0 2 * * *`; `timezone: America/New_York`;
`catch-up: skip`; `target: escalate-stale-holds`. Entry input (normative):
`{"scheduled-at": …, "fired-at": …}` — **the 48h cutoff derives from
`scheduled-at`**, so a delayed or catch-up firing selects the same holds as
an on-time one.

Run context carries `{cutoff, hold}` across the effectful nodes — the
single-input model loses them otherwise (`http-request` emits its own
envelope). A `ctx` write replaces the document, so `mark` merges explicitly
to keep `cutoff`.

**Ordering: notify before escalate.** A terminally failed notification
leaves the hold **un-escalated** — the next tick reselects it; the retry
path is the selection predicate. The inverse window is two different windows: a **crash** before
`escalate-head` recovers the same run (same occurrence key — no duplicate,
or a provider-deduped redispatch); a **terminal** failure between notify and
escalate kills the run, and the next tick reselects under a new key —
**at-least-once across run deaths**. Stated honestly; a durable per-hold business key is the recorded upgrade if
bounded delivery ever becomes a requirement. Hard-fail-aborts-drain is the
recorded deliberate decision: `fail` discards the frontier; remaining holds
wait for the next tick.

## F4 — disposition-recorded

```
event
  → capture            transform "@" (pure pass-through)
                       ctx: "{disposition: new}"
                       -- a ctx expression sees the node's OUTPUT; after the
                       -- query, `new` no longer exists — capture it first
  → load-hold-context  postgres-query, QUERY mode (hold + material + history)
  → shape-input        transform: build {hold, history, decision} for F2 —
                       history = dispositions STRICTLY PRIOR to the current
                       one: decided_at < current.decided_at, tie-broken by
                       id < current.id (deterministic under equal
                       timestamps). Id-exclusion alone is wrong: this run
                       executes async after commit, so LATER dispositions
                       may already be committed and must not enter history
  → invoke-recommendation  invoke-flow → F2 (service mode)
  → record-comparison  postgres-query CTE, QUERY mode + RETURNING,
                       keyed by context().disposition.id
  → shape-callback     transform over RETURNING rows + context()
  → notify-erp         http-request, occurrence-keyed

invoke-recommendation.error → recommendation-failed fail
notify-erp.error            → callback-failed fail
```

Concretely: the entry emits the normative `{event, new}`; the `capture`
pass-through stores `{disposition: new}` into context before the payload
chain moves on — the disposition id, decision, and inspector survive every
downstream envelope. Business input (column name matches the catalog):

```json
{ "event": "insert",
  "new": { "id": "…", "hold_id": "…", "decision": "accept",
           "inspector_id": "…", "decided_at": "2026-07-26T14:03:22Z" } }
```

Event registration (exposure, live table — versioning deferred per spec
§5.7): `entity: dispositions`; `operations: [insert]`;
`target: disposition-recorded`. The run records `registration_id` **in its
dedicated `runs` column** and the registration content hash **in the trusted
invocation context** (spec §15.1/§5.7 — two homes, not one); causation,
seq, entity, table stay out of the business payload.

`record-comparison` upserts `disposition_reviews` —
`disposition_id` **unique**, `recommendation`, `matched`, `confidence` —
`idempotent-with-key` on the disposition id from context. `shape-callback`
builds the ERP payload from the `RETURNING` row; the callback rides the
occurrence key (event admission dedup — spec §6.1
`evt:{registration}:{seq}` — guarantees one run per event, and the
occurrence key covers within-run replay; business-derived callback keys are
the recorded upgrade).

## Named mechanical deltas

- **Run context plumbing** (spec §4.4): `RunContext.context` field; the
  `ctx` config key evaluated by the SDK over node output; the `context()`
  expression function; the custom-node invocation envelope gains `context`
  in and `ctx` out.
- **Two-stage invocation interface** (spec §11): `begin → admitted{run-id} |
  rejected`, `wait`, `cancel` — `flow-http` holds the run-id while waiting.
- **`time-shift` accepts RFC 3339 input** (today: epoch-ms only) — the
  normative cron input is RFC 3339.
- **`normalize-receipt` / `evaluate-specs` extracted as deployable
  custom-node components** with Service endpoints (`flowrunner`'s custom
  dispatch is an in-cluster HTTP hop; deleting `poc/webhook-f1` orphans the
  logic otherwise), manifests declaring `purity: pure`.
- **RawSql minimal grant (exact seam)**: the flag is an in-guest boolean
  initialized false with no deployment toggle. The POC delta: the runner
  reads a per-project-environment config value `raw_sql_enabled` (project-env
  config row, plumbed into `NodeCtx::raw_sql_enabled`), and the POC
  environment sets it true. That is the entire grant — richer per-role
  wiring is out of the POC's scope.
- **From-zero catalog fixture**: `quality_holds` unique natural key; stable
  **line identity `(receipt_id, line_no)` unique** (holds alone did not
  close the replay hole); `disposition_reviews` entity.
- **`disposition-node` component**: contract edits per F2.
- `poc/webhook-f1` deleted; F3 loses its terminal respond; all exposure
  lives outside the graphs.

---

# §T — Testing plan

Layers: structural (Phase 1) / scenario (harness + doubles, spec §13) / e2e
(gate proofs). **f1proof parity is business-result parity against the real corpus**:
fixtures in `test-support/fixtures/f1fixture.rs` (burst = 20 receipts, 3
out-of-spec, 4 holds), exercised by `tests/system/src/f1proof.rs` and the
gate `deploy/gates/f1proof-job.yaml`. Surviving assertions: the admitted
burst's business results — holds created (count + identities), out-of-spec
detection, persisted rows, response payloads. Rewritten assertions: every
malformed-request case (malformed input is now 400 with **no run**;
`docs/poc-f1.md`'s contrary expectation and its stale task references are
superseded by this plan and updated with the fixture refresh).

## T0 — cross-cutting

| Layer | Positive | Negative |
|---|---|---|
| structural | all five fixtures parse and pass §3 | mutations trip named predicates, incl. all-paths-`fail` → `no-response-node` (rev-14 Resp rule) |
| scenario | entries synthesize from the normative §4 shapes | `"old": null` rejected (absent = omitted) |
| e2e | all five run from the reprovisioned from-zero database | no fixture references `Trigger`, `Flow::entry`, or the old event input |

## T-CTX — run context (Phase 3 gates, proven on F3/F4 shapes)

| Positive | Negative |
|---|---|
| a `ctx` write **replaces**: a later write without `merge()` provably drops prior keys | error-port emissions never mutate context |
| context reconstructs identically on boundary recovery (pure replay) | a child run starts with empty context regardless of the parent's |
| an effectful node's context-resolved params land in `attempt_input_ref` — recovery sees what the attempt saw | — |

## T1 — F0/F1 (Phase 4: the black-box milestone)

| Layer | Positive | Negative |
|---|---|---|
| e2e (F0) | two commits exactly (§9.8) | any third commit fails the criterion |
| scenario | valid receipt → holds created, `respond 200`, outcome hash recorded | unknown supplier → `fail` 400, durable failed run, envelope `caller_http_status = 400` |
| e2e | `POST /receipts` valid → 200, shaped payload (from `RETURNING`, not a row count) | malformed → 400 **no run**; bad key → 401 no run |
| e2e | **HTTP unknown-supplier case: `invoke-result::failed` adapts to HTTP 400 with the envelope** — the admitted-fail path proven over the wire, not scenario-only | — |
| e2e | duplicate key post-release → stored outcome; across promotion (attachment unchanged) → stored outcome | in-flight → 409 no second run; different body → `idempotency-key-reused`; attachment edited → `idempotency-scope-changed` |
| e2e | disabled attachment: prior outcome retrievable (§6.2 lookup order — auth under route-selected policy proven) | disabled attachment: new request → 404 no run |
| recovery | committed-but-unrecorded fault injected **independently for each CTE** — once for `resolve-and-persist`, once for `create-holds` (after the database commit, before `node_runs` success; kill-mid-CTE only exercises rollback+reinsert) → recovery redispatches with the same key; the read-back-on-conflict CTE returns **byte-identical rows in `line_no` order**; the final response and outcome hash equal the crash-free run; exactly one logical row set | kill post-CAS → waiter recovers outcome, no duplicate release |

## T2 — F2 (Wave 2)

| Layer | Positive | Negative |
|---|---|---|
| structural | request entry + zero-import component + terminal respond | undeclared emit port → `infrastructure-failure` |
| scenario | `{hold, history, decision}` → deterministic result, `confidence` a string | missing field → 400 no run |
| e2e | F4→F2 under **service mode**: child runs as F2's identity, caller audited | a flow not in `allowed-callers` → rejected at runtime authorization |
| e2e | disable F2 **after** child creation (F4 parked) → existing child completes, wake proceeds (revocation gates creation only, spec §12.2) | disable **before** child creation → invoke gets `callee-revoked` → `recommendation-failed` |
| recovery | crash mid-`recommend-disposition` dispatch → recovery **re-dispatches** the node (manifest `purity: pure` authorizes `replay`) and the run completes | the same crash must **not** produce `effect-uncertain` — that outcome would mean the purity override silently failed and the custom default applied |

## T3 — F3 (Phases 2A–3)

| Layer | Positive | Negative |
|---|---|---|
| scenario | N stale holds → N iterations (select → notify → escalate via context), §9.9 completion | delayed firing: cutoff from `scheduled-at` → selection identical to on-time |
| scenario | zero holds → unwired false port → §9.9 completes | notify fails terminally on hold 2 of 3 → run `failed`, hold 2 **un-escalated and reselected next tick** |
| scenario | **crash** after notify success recorded, before escalate → recovery resumes the **same run**: escalation proceeds, **no re-notification** (the occurrence record short-circuits); crash mid-notify (started, no success) → redispatch with the **same key**, provider-deduped | **terminal failure/cancellation** after notify but before escalate → run dies; **next tick** reselects the still-un-escalated hold with a **new** occurrence key → duplicate notification permitted: **at-least-once across run deaths, exactly-once-per-key within a run's recovery** |
| e2e | fires from the 2A cron attachment; run id `{flow}:cron:{generation}:{tick}` | no firing while disabled; activation events audit the transitions |
| e2e **(lands with 2B)** | schedule edit → generation+1; same tick under two generations = two identities; disable→enable gap governed by the new generation's `catch-up: skip` | — generation-transition machinery is §7.5's 2B refinement; these rows gate 2B, not the POC's Wave 1 |

## T4 — F4 (Wave 2)

| Layer | Positive | Negative |
|---|---|---|
| scenario | seeded insert → context captured, shaped input, F2 double, keyed `disposition_reviews` row, shaped callback | insert omits `old`; delete carries `old` only |
| scenario | `shape-input`'s `history` provably **excludes the current disposition** (the seeded fixture would produce a self-referential match otherwise) | — |
| recovery | ERP callback **sent-but-unrecorded**: sink observes the POST, crash before success → recovery redispatches with the **same occurrence key**; the sink double implements key-dedup and asserts **two transport attempts, exactly one committed sink-side business effect** | a different key on redispatch, or two committed effects, fails — identical keys alone prove nothing about effective-once |
| e2e | real insert → exactly one run per `(registration, seq)`; **the run inspected: `registration_id` in its dedicated `runs` column, the registration content hash in the invocation context — two homes, both asserted** | redelivery (same seq) → no second run |
| e2e | **scope discrimination: equal `seq` under a different registration (and under a different tenant) → distinct runs** — the dedup key is the full scope, not the sequence | — |
| e2e | crash after child release, before parent checkpoint → child recovered by occurrence key; one child, one review row | parent cancelled while child parked → child cancelled via generation seizure |
| recovery **[r6 — the create-or-recover discriminator]** | fault injected **between child-run creation and the parent's occurrence/wait recording**, then replay the parent occurrence → create-or-recover finds the existing child: **exactly one child run** | two children after the replay fails the gate — that is a broken create-or-recover transaction, invisible to any post-release test |
| invariant **[r6 — wake-at-release]** | kill immediately after the child's release commit, then inspect both rows: `child.caller_released_at` set **⇒** parent wait cleared — the same-transaction invariant means **no intermediate state is ever observable**; recovery then resumes the parent | child released with parent still waiting (or vice versa) is an invariant violation — if only eventual resumption can be proven, the Wave-2 claim narrows to that, explicitly |

## T-NR — the `never-replay` gates (Phase 3, Wave 1 — not Wave 2)

Synthetic scenario fixture (spec Phase 3 gate): a `never-replay` effect
against a counting sink. **(a) sent-but-lost**: sink observes exactly one
call; crash before the completion write; recovery → `effect-uncertain`;
sink stays at one — the dangerous window, proven. **(b) crash-before-send**:
zero calls; `effect-uncertain`; the cheap sibling. **(c) the purity
control**: the same crash against a custom node **without** `purity: pure`
must yield `effect-uncertain` — this is what makes T2's replay row
discriminating; if every custom node were incorrectly replayed, (c) fails
while T2 still passes.

## T5 — measurement hooks (Phase 6)

F0 = pure two-commit; F1 = effectful request; F3 = queued/cron;
F4+F2 = idempotent child. Appendix B's saga stays a separate synthetic.

## What the POC deliberately does not prove

Event-exposure versioning (live-table registration); fine-grained promotion
impact (the coarse rule is what T1/T3 exercise); cache generations; and
Phase 5's full closure — the gates named in the Wave-2 scope note (lost-wake
reconciliation, stale `wait_generation` rejection, post-release
child-failure isolation, released-child cancel refusal) **and** Appendix B's
saga all close in Phase 5's own acceptance tables, beyond this POC.
