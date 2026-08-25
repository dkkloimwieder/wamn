# Authoring collapses to the stateless-gate model — demolition spec

**Status: RATIFIED by the owner, 2026-08-25 (`wamn-7qtw`). All four questions in §7 decided.**
This authorizes a Phase-A-style deletion wave, not a refactor.

Everything below is measured at `10bbe597` unless marked as a judgement. Where the
directive's expectation and the tree disagree, the tree is reported.

---

## 1. The premise, and how much of it the tree already holds

Wirings are documents with content-hash identity. A draft is a client-side file — a
studio buffer, a git working tree — not server state. Two verbs plus reads:

| verb | shape |
| --- | --- |
| `gate(document) -> report` | stateless; report immutable, keyed by wiring-hash; idempotent by construction (same hash → same report). No run-row protocol, no retry-resumption, no Pending convergence, no draft tables. |
| `publish(document, report-id)` | catalog row + activation, as landed. |

**Half the premise is already law in the tree.** `deploy/sql/control-portable-store.sql`
carries this comment above the draft section:

> No validated-draft relation (`wamn-pm7k`): the draft concept died with the pivot. The
> wiring document IS the validated artifact and its hash IS the identity.

`catalog.validated_flow_drafts` is already gone. `catalog.wirings` already carries
`wiring_hash text NOT NULL CHECK (wiring_hash ~ '^sha256:[0-9a-f]{64}$')`, is immutable
under triggers on both UPDATE and DELETE, and has no mutable state.

So this is not a new direction. It is finishing one the tree started.

---

## 2. The seven ops mapped

The contract is five commands plus two queries (`crates/authoring/model/src/lib.rs`).

| op | kind | directive says | measured verdict |
| --- | --- | --- | --- |
| `publish` | command | survives | **survives.** Unchanged. |
| `get-report` | query | survives | **survives**, re-keyed — see §2.1. |
| `gate` | — | survives | **does not exist yet.** New verb. See §2.2. |
| `save-draft` | command | collapses | **collapses**, client-side. |
| `read-draft` | query | collapses | **collapses**, client-side. Already returns 501. |
| `validate` | command | collapses | **collapses**, absorbed into `gate`. Already unmounted. |
| `draft-run` | command | collapses | **collapses**, absorbed into `gate`. Already unmounted. |
| `test-set-run` | command | **unstated** | **becomes `gate`.** See §2.2. |

### 2.1 `get-report` survives but its key changes

Reports today are keyed by an opaque `report_id`. `crates/execution/run-state/src/admission.rs`
forces `report_id` to equal the candidate's `gate_report_id`, and requires that to equal
`catalog.wirings.gate_report_id`. The report identity is therefore already *derived from
the candidate*, never minted by the caller.

**`catalog.wirings.gate_report_id` is bare `text NOT NULL` with no foreign key to anything.**
It sits on the same immutable row as `wiring_hash`, which is a real content hash. Two
identifiers for one fact.

**RATIFIED 2026-08-25: `gate_report_id` collapses into `wiring_hash`.** Bare text with no
FK beside a real content hash is two identifiers for one fact. **The report row keys on
`wiring_hash`; the `gate_report_id` column dies.** Touches `catalog.wirings`,
`services/ctl/src/author_wiring.rs`, and the admission statement's parameter shape.

### 2.2 `gate` is `test-set-run`, restated

The directive names `gate` as a survivor and does not mention `test-set-run`. They are the
same verb: `test-set-run` takes a candidate wiring, executes its cases, and produces a
pass/fail report — which is gating. Nothing else in the contract produces a report.

`validate` and `draft-run` are the two unmounted commands whose purpose `gate` absorbs:
`validate` checked a document without executing it, `draft-run` executed one case ad hoc.
Both are strictly weaker than "execute the document's cases and report."

**RATIFIED 2026-08-25: `gate` is `test-set-run` renamed — same judgment, honest name.**
One verb produces reports. The rename lands **with the collapse**, and the wire literal
follows the wiring vocabulary sweep (`wamn-0h0g.26.18`).

---

## 3. What deletes

### 3.1 Draft-persistence tables

| relation | note |
| --- | --- |
| `catalog.flow_drafts` | The draft store. Server-side revision counter, `edited_at` ordering, `definition`/`graph_json` content. Entirely client-side under the premise. |
| `catalog.guard_flow_draft_update()` | PL/pgSQL trigger function enforcing monotonic `revision` and `edited_at`. Dies with the table under the deletion rule. |

`catalog.validated_flow_drafts` is **already gone** — do not re-file it.

### 3.2 Composition machinery

`services/scenario-worker/src` totals 5,067 lines. Under the stateless model:

| file | lines | fate |
| --- | --- | --- |
| `store/test_orchestration.rs` | 845 | **deletes.** Reservation/case-run/report lifecycle. |
| `test_set.rs` | 538 | **deletes.** The sequential per-ordinal loop is the resumption protocol, and `evaluate()`'s one production caller lives inside it. |
| `store/admission.rs` | 528 | **~350-400 survive**, re-homed under the gate verb; see §4. |
| `store/drafts.rs` | 70 | **deletes.** |
| `management.rs` | 2,032 | **partially deletes**: `lock_retry_identity`, `read_retry_outcome`, `settle_retry`, `classify_retry`, `test_set_reuse`, `save_draft_route`. |

The retry-identity machinery (`classify_retry` at `management.rs:243`, and the three
`*_retry` helpers) exists solely to make a re-submitted command converge. **Hash-keyed
idempotency makes it dead weight**: the report either exists for that hash or it does not.

### 3.3 Run-plane tables

| relation | fate |
| --- | --- |
| `wamn_run.authoring_test_run_reservations` | **deletes.** `state`, `whole_deadline_at`, `finalized_at`, `command_hash` are all resumption-protocol state. |
| `wamn_run.authoring_test_case_runs` | **deletes.** Resolved by §5.1: effect-free cases make re-execution free, so per-case progress is not worth remembering. |
| `wamn_run.authoring_test_reports` | **deletes.** ~~survives, re-keyed by wiring-hash~~ — struck by the owner ruling of 2026-08-25; see §3.3.1. |

#### 3.3.1 The struck survivor-clause (owner ruling, 2026-08-25)

The two struck clauses above — `test_set.rs` "mostly deletes" and
`authoring_test_reports` "survives, re-keyed by wiring-hash" — were contradictions,
and the wave-65 lane was correct to refuse them rather than force the bead.

**The measurement that refuted them.** `authoring_test_reports`' only production writer
is `insert_finalized_test_report_sql()` in `store/test_orchestration.rs`, which this
same section deletes; its only production reader drives off
`authoring_test_run_reservations`, which this same section also deletes. A table whose
writer, reader and keying all die does not *survive* — preserving its identity while
every one of its collaborators is deleted is a rename wearing a survival ruling. And
"re-keyed by wiring-hash" is `wamn-0h0g.8.5.6`, which is sequenced *after* this bead, so
the clause was circular as well as contradictory.

**Re-derived from the ratified premise.** An effect-free gate's report is *reproducible
from the document*: same hash, same verdict, byte-stable. So this table as durable state
protects nothing — it was the composition machinery's memory for per-ordinal resumption
across effectful cases, and §5.1's effect-free clause deleted the thing it remembered.
The gate's one durable fact is the report row keyed by `wiring_hash` that publish
consumes, which has its own ruled home in the report-on-the-wiring lineage. Persisted
case-level detail is a cache, and a cache for a reproducible computation is not built
ahead of demand.

**Consequences.** All three run-plane `authoring_test_*` tables delete together.
`ReportProjection::Pending` deletes with them — it was reachable only while the
reservation protocol stood. `wamn-0h0g.8.5.6` re-scopes from *re-key the table* to
*the gate verb writes the report row keyed by `wiring_hash`*: construction against the
surviving row, not migration of a corpse. The circularity dissolves because the thing
that made it circular is gone.

**The standing rule this produced, owner-stated:** any "survives" or "re-keys" clause on
flow-era state must name its post-collapse **writer and reader in the same sentence**, or
it is a deletion.

### 3.4 Contract surface

Deletes from `crates/authoring/model/src/lib.rs` (939 lines): `SaveDraft`,
`ValidateDraft`, `DraftRun`, `DraftRunCapture`, `DraftRunReceipt`, `ReadDraft`,
`SaveDraftRefusal`, `DraftRunRefusal`, `ReadDraftRefusal`, and the `SaveDraft`/`Validate`/
`DraftRun`/`ReadDraft` variants of `AuthoringCommand`, `AuthoringCommandKind`,
`AuthoringQuery`, `AuthoringQueryKind`.

Survives: `PublishValidatedDraft`, `GetReport`, `GetReportRefusal`, `TestSetRun` (as
`gate`), `TestSetRunReceipt`, `TestSetRunRefusal`, `ReportProjection`.

**`ReportProjection::Pending` deletes.** There is no pending state in a stateless gate.
This is what supersedes the `last_attempt_at` rider.

### 3.5 Adjacent relations — one survives, one deletes

`catalog.authoring_command_audit` **survives** — it audits *who ran what*, which is
orthogonal to draft persistence. Do not sweep it.

Per the 2026-08-25 standing rule (§3.3.1), a survivor-clause must name its post-collapse
writer and reader in the same sentence, so: it is **written** by `INSERT_COMMAND_AUDIT_SQL`
at `services/scenario-worker/src/management.rs:56` and **read** by the query at
`management.rs:70`, both under the `scenario-worker` principal, and both survive the
collapse because they record command attribution rather than draft or composition state.

`catalog.draft_safe_connection_grants` is a **separate finding, not a survivor.** It has
**zero production DML** — no INSERT, UPDATE, or SELECT anywhere in the tree. Its only
appearances are inside the authoring role's privilege probe
(`services/scenario-worker/src/authoring.rs`): one positive assertion that the role holds
SELECT on it, and two negative assertions that the role holds neither INSERT nor UPDATE.
It is a table whose entire purpose is to be named in an assertion about privileges on it.

**RATIFIED 2026-08-25: it deletes in THIS wave.** Under effect-free gating its *concept*
is void — gate runs touch no connections — and it had no production DML to begin with. The
probe arm dies with it **in the same commit**, per deletion rule 9, along with its
`architecture/state-owners.json` row (which lists `scenario-worker` as a reader, true only
in the `has_table_privilege` sense).

---

## 4. What `wamn-0h0g.8.5.4` reduces to

`8.5.4` landed 2,171 lines at `523750fc`. Under hash-keyed stateless reports:

**Survives** — candidate resolution by wiring hash (`store/admission.rs`), and the
finding that keying on `wiring_hash` yields every admission parameter from
`catalog.wirings`. That is exactly the stateless model's lookup, built early.

**Survives** — the behavioural route guard that replaced the self-scanning source scan.
It asserts unmounted kinds return an empty-bodied 501 after authorization. The wave deletes
four kinds, so the guard's inventory shrinks; the guard itself is correct and stays.

**Deletes** — the sequential per-ordinal reserve→admit→poll→evaluate→finalize→reconcile
loop, at-most-once-per-ordinal bookkeeping, and crash-after-commit convergence. All three
are the resumption protocol.

**Deletes** — `run_deadline_at` read-back and the frozen `platform_revision` constant.
Both exist only so an exact retry does not diverge.

**Honest accounting: roughly 350–400 of the 528 lines in `store/admission.rs` survive; the
~900 lines of composition and retry machinery do not.** The bead's most valuable output was
never the loop — it was the measurement that `wiring_hash` is a sufficient key, which is
the premise of this spec.

---

## 5. The open question: does case execution need server-side state beyond the report row?

**Answer: yes, one thing — and it is not what the current machinery provides.**

Three sub-questions, measured separately.

**(a) Does producing a report need stored state? No.** Cases ride the document itself
(`WiringDocument.cases` in `catalog.wirings.graph_json`). Inputs, expectations, and the
binding world are all derivable from the document plus the catalog. Nothing needs a
reservation to *know what to run*.

**(b) Does idempotency need stored state? Only a uniqueness constraint.** "Same hash → same
report" is enforceable by a primary key on the report row. Two concurrent `gate()` calls for
one hash race; the loser's INSERT conflicts and it returns the winner's report. That is one
constraint, not a protocol — no `command_hash`, no `state` column, no deadline.

**(c) Does *partial execution* need stored state? This is the real question, and the
answer depends on a fact the directive does not settle.**

Test cases invoke real components, which perform real effects. If `gate()` executes ten
cases and the process dies after case seven, a retry with no stored state re-runs all ten —
**re-performing seven cases' effects.** The current per-ordinal machinery exists precisely
to prevent that, and `wamn_run.authoring_test_case_runs` is where it remembers.

So there are exactly two coherent positions, and they are not equally cheap:

1. **Cases are effect-free by contract.** Gate execution runs against components in a
   sandbox that refuses effects, so re-execution is free and `authoring_test_case_runs`
   deletes with everything else. Truly stateless. **This requires a contract statement the
   tree does not currently make, and an enforcement point that does not exist.**
2. **Cases may perform effects.** Then per-case durable state survives in some form, and
   "stateless" describes the *report identity*, not the execution. `authoring_test_case_runs`
   survives, re-keyed by wiring-hash instead of by reservation.

### 5.1 RATIFIED — position 1, as a constitutional clause

**Gate cases are effect-free by contract.** Owner ruling, 2026-08-25.

> A gate is a **judgment about a document**, not an execution of it. Effects belong to
> admitted runs under run identity. A report must be reproducible from the document alone,
> or the hash-keyed idempotency is a lie.

**Enforcement, and this is what makes the clause checkable rather than aspirational:** gate
cases invoke components in a **no-effect execution mode**. The case runner refuses — typed,
at validation or at invocation — any case whose path reaches an effectful operation. The
enforcement point is the **effect-posture fact `wamn-0h0g.21.9` minted at admission**:
`catalog.component_library.effects`, the validator's derived projection of a component's
imports onto the authority packages that leave the host. A component with a non-empty
`effects` projection cannot be reached from a gate case.

That fact is already load-bearing and already guarded: `wamn-0h0g.21.10` established that a
projection no validator derived is refused at publication (`2d81e5f1`) and on the serving
path (`d7f36905`). The gate refusal is a third reader of the same fact, not a new mechanism.

**The justification, recorded verbatim as the clause's own reasoning:**

> assume it and the first effectful case silently double-fires.

**Consequence:** `wamn_run.authoring_test_case_runs` **deletes** with the rest. Re-execution
is free, so there is nothing to remember. §3.3's "contested" marking is resolved.

**Demand-gated future surface, not this wave:** effectful case-testing — integration against
real bindings — is a *different product surface*. Its trigger is a client asking for it, and
it arrives **with run identity**, not with a resurrection of the per-ordinal machinery. Do
not cite this spec as precedent for rebuilding reservations.

---

## 6. Risks and refuted expectations

- **`gate` does not exist and `test-set-run` was unlisted.** §2.2 treats them as one verb.
  If that is wrong, the mapping changes and §4 changes with it.
- **The wave is a wire change on a registered drift gate.** The authoring contract crate is
  a drift gate; deleting four of seven ops moves it deliberately. The generated authoring
  client regenerates in the same commit.
- **`gate_report_id` → `wiring_hash` touches the admission statement's parameter shape**,
  which `crates/execution/run-state/tests/admission_live.rs` pins at 17 parameters. Expect
  that pin to move.
- **`ReportProjection` is `deny_unknown_fields` with a frozen-literal contract test.** The
  test currently covers the `Finalized` variant only; deleting `Pending` is still a wire
  change and the survivor should be frozen as a whole-value literal.
- **Deletion rule 9 applies**: each subject takes its tests, guards, fixtures, registry rows
  and doc references in the same commit. One bead per subsystem, not per file.
- **`architecture/state-owners.json` rows delete with their relations**, and
  `tests/conformance/tests/state_ownership.rs` moves with them. That file's write-set pin
  already fails at `:1612`, so **a correction there is invisible until the unregistered
  relations are resolved** — do not read a green run as proof.

---

## 7. What ratification decided — owner, 2026-08-25

All four carried. This section is the record, not an open list.

1. **§5.1 — gate cases are EFFECT-FREE BY CONTRACT.** Constitutional clause, enforced
   through the `.21.9` effect-posture fact at admission. `authoring_test_case_runs` deletes.
2. **§2.2 — `gate` IS `test-set-run` renamed.** Rename lands with the collapse.
3. **§2.1 — `gate_report_id` COLLAPSES into `wiring_hash`.** The column dies.
4. **§3.5 — `draft_safe_connection_grants` DELETES in this wave**, probe arm same-commit.

**This spec is ratified and authorizes the wave.** One bead per subsystem, under Phase-A
deletion rules: each subject takes its tests, guards, fixtures, registry rows and doc
references in the same commit. Partitioning into lanes is a collision-domain question to be
**measured, not inferred from this section's headings** — the subsystems interlock through
`AuthoringCommand` and `management.rs`, and a lane split that breaks compilation is worse
than one lane.
