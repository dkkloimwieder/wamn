# Transactional composition and security posture

Oct 10, 2026 · @D Kloimwieder

## Decision

wamn has one way to compose packages inside an application: the caller sequences the callee's exported operations, and a callee joins the invocation's host-owned transaction by asking the host for it with operation-transaction(). Existing read exports that open their own guest transaction do not join; inside a command their begin refuses, as today. Converting them is listed in Deferred. No transaction handle crosses a component boundary. No platform participant or pre-commit mechanism exists; base-declared WIT imports remain as ordinary dependencies.

This is container-managed transaction propagation (Java EE CMT, Spring `@Transactional(REQUIRED)`, .NET `Transaction.Current`, PL/pgSQL nested calls). The callee asks the runtime for the current transaction; the caller decides order.

Security follows the component model: authority arrives only through WIT imports, and publish checks imports per member. A member that does not import the transaction interface cannot touch the transaction.

Status: done 2026-10-15. All four items closed: C `wamn-14bx8`, A `wamn-lqtai`, Cleanup after migration `wamn-ponya` with `wamn-azko6`, and B `wamn-aemxz` (owner acceptance 2026-10-15). The text below is the plan as it was carried out. Earlier status: direction, cut to a core on 2026-10-11 after ten review rounds, and split into three independent items (A composition, B profile screen, C authorization) that ship alone. Core (spike A, then stop reviewing until there is a result): per-member instances of `wamn:postgres/statements`, with member→instance wiring verified in the composed artifact; transaction selection unchanged; `run`/`run-stream` refused inside a host transaction; `client` denied to application components. No version bump on anything: `wamn:postgres` stays 0.3. B is a review screen with no enforcement on it. C is the ACL ruling, pure deletion; grantable administration is a follow-up after C, rescoped then. Cleanup after migration (section below): every deletion is kept and lands after Acme moves off the participant; nothing is lost. Deferred (section below): everything else the reviews added. The design supersedes the participant and pre-commit path in data-access.md; the permission folds of data-access.md:313 and execution.md:20, `ReleaseClosures::expand`, and platform-ui.md §2.3–2.4; `fresh_only`; and the mechanism half of the 2026-09-24 "composition at build" line, not the ruling that nested calls are in-process component calls.

## How this plan is worked

- This plan is the single authority for the next work, as `platform-deploy.md` was for deploy (owner ruling 2026-10-14).
- The top list of `docs/plan/platform-deploy.md` ("How this plan is worked") applies unchanged: decide, do not ask; findings first; at most three issues per epic; the testing budget; one worktree per lane; the version-bump rule; reports of ten lines or fewer at merge.
- Rule 5 reads: `git status --short` on the main checkout is empty, untracked excluded paths aside. The owner's skill installs (`.agents/skills/zaidan/`, `.claude/skills/`, `skills-lock.json`) are in `.git/info/exclude` and are never committed (owner ruling 2026-10-14).
- Code commits never edit this plan. A plan edit is a docs commit that cites its ruling.

## Mechanism

An application is one composed component (`wac`), one store, one invocation, one host-owned transaction per command item. Every member runs in that store.

1. The host begins the transaction before the entry runs and commits or rolls back after it returns (`data-access.md:336`).
2. A member that needs SQL calls `operation-transaction()`. Today the host looks the transaction up by the composed component's id and hands every member the entry's statement set (resources.rs:1342-1363). With per-member instances the lookup is by (component id, import instance): same transaction, each member's own statement set (Security posture).
3. The handle is lent: `commit` and `rollback` on it refuse (`resources.rs:118-126`, `:1496`). `begin` inside an operation refuses.
4. The caller inserts before, between, or after callee operations by call order. There is no other insertion mechanism.

A base package exports its operation as granular functions, not one blob:

```wit
interface record-receipt {
  lock-purchase-order:   func(id: uuid) -> result<purchase-order, error>;
  insert-receipt:        func(po: purchase-order, lines: list<line>) -> result<receipt, error>;
  finish-purchase-order: func(id: uuid) -> result<_, error>;
}
```

An overlay composes them. The real Acme case: the inspection row needs the receipt id, so it runs after Receiving's unit of work, in the same transaction and still under the purchase-order lock (`FOR UPDATE` holds to commit):

```rust
let r = receiving::receive_lines(po_id, lines)?;   // receiving joins the transaction inside; returns receipt id
acme::record_inspection(&r.receipt_id, po_id)?;    // acme joins the same transaction; refusal rolls back the item
```

Runtime cost with no overlay: identical to today. With an overlay: one WIT lift/lower per inserted call, the same crossing the participant pays today. No host dispatch, no registry, no runtime weaving.

**Authorization: the ACL is the primitive, and nothing derives into it (ruled 2026-10-11).** `permission(role, operation)` is a row, authored per tenant, applied on the next request (ruling 2026-09-29). The request check is `caller holds entry`. That is all. What the entry's code calls runs under the entry's authority because its author wrote the call. A misconfigured role is a 403 at request time.

Today both sides of the check are folded through the call graph, and both folds go:

- Required side: publish unions the permissions of every operation an entry reaches (`execution.md:20`, `serving_manifest.rs:157-159`). Deleted. `required = {entry}`.
- Held side: a role's stored permissions are *roots*, and the host expands each root into its published closure on every request (`ReleaseClosures::expand`, `release_closures.rs:60-67`; `HeldOperationGrants::from_rows`, `claims.rs:83-98`). So granting `acme.record_receipt` today also lets that caller invoke `receiving.record_receipt` **directly**. Deleted. `held = roots`.
- Storage: the platform-UI spec *describes* materializing the closure as `required_by` rows with a revoke cascade (`platform-ui.md:107-238`). The code never built that: `app-schema.sql:318-325` stores roots only, every row has `required_by = permission`, and expansion happens at request time. So two separate deletions: the runtime expansion above, and a vestigial column with its self-FK and regex CHECK. No stored closure rows exist to remove.
- `fresh_only` and its fold (`serving_manifest.rs:160-163,327`; `execution.md:20,420,446,829`; refusal at `router_delivery.rs:591`). Deleted. It has meant "requires a PAT" (backwards: the session token is the fresher credential) and now means "refuses a `QueuedService` caller even when it holds the permission." **This is an explicit authorization-policy change, ruled 2026-10-11, not a consequence of composition:** under the ACL a service role explicitly granted such an operation succeeds, because the admin decided it. It is its own epic item with its own positive test. Step-up authentication, if ever wanted, is an entry-level session property with no fold.

**Four things carry the load. Nothing else does.**

1. **ACL** — person → operation, rows, runtime. Both folds deleted.
2. **Instance** — package → its own declared statements, linked at build. The only code boundary. A member cannot name a sibling's statement.
3. Transaction — unchanged in A. `begin_item` opens a host transaction when the entry's released statement set is non-empty (native\_policy.rs:239-244); `fold_call_graph` fills that set with callee statements through declared dependencies (serving\_manifest.rs:329), so an SQL-free Acme entry calling Receiving already gets one. A read never does: `begin_item` runs only for operation types that log intent (`logs_intent`, intent.rs:84-88). A member-union rule was proposed and withdrawn (2026-10-12): `apps/edge_samples` has SQL-free `sample.read` beside SQL `sample.record` in one component and would have lost its two-step claim. When `operations[].dependencies` goes (Cleanup), the begin decision needs a replacement that is not a graph. Candidate: the manifest's declared `transaction` field (`sample.read` declares `transaction = None`; `has_host_transaction`, manifest.rs:161) without the statements-non-empty requirement. Decided at cleanup, not in A.
4. **Refusals inside a host transaction** — `run`, `run-stream`, guest `begin`. Already there.

`operations[].dependencies` retires in Cleanup, not in A: A still relies on it for the fold that feeds `begin_item`. Its consumers and their replacements are listed in Cleanup after migration. `base_dependencies` (package, digest) stays, because composition needs the pin.

What is deliberately not guarded: a package against its own author. Under (2), Receiving's read operation can run Receiving's write statement. That is Receiving's author writing a bug or a lie about the operation type, the same way they could write a wrong WHERE. The platform protects packages from each other and callers from operations; it does not protect a package from itself anywhere else and does not start here. An operator granted a mislabeled "read" holds a write; the profile screen (below) shows the admin exactly that, and code review catches it. Three review rounds built a static analyzer — constant-path rules, soundness gates, call\_indirect and mutable-memory fixtures, type/profile agreement at publish — to enforce this boundary from compiled bytes. WebAssembly memory and tables are mutable, so no static claim about what a component will call can be security evidence, and the reviews kept finding that. The analyzer is withdrawn. The instance already draws the only boundary that matters.

**The effect profile is a review screen, derived from bytes, and nothing enforces on it.** Per operation: which sibling operations it can reach, which statements (via the generated bindings it calls, `record_receipt.rs:18,467`), which tables and columns those read, write, or lock (relations derived from SQL, `data-access.md:164`). Reads matter most here: "granting `acme.receipt_screen` lets a role see `purchase_order.{id,status,cost}`." A role's profile is the union over its granted operations. Imprecision — an export widened to the member union by inlining, `call_indirect`, or a rewritten path — is marked as imprecision and degrades the screen, not security. Other consumers (stateless derivation, connection binding, recovery-class check, edge tiering) are each their own decision when they have a case.

**Dev loop is unchanged.** SQL bytes live in the package artifact, not the component: a statement body change with the same binds and columns leaves the component digest identical; generation re-plans and `push-package` pushes a new package artifact, with no `cargo build`. A shape change recompiles the crate that calls it, as any typed-SQL tool does.

**Why this was wrong before, so it is not re-litigated.** Eight review rounds pushed toward a hand-declared per-operation graph, then a transitive permission closure built from it, then a trust statement about the declarations, then `fresh_only` propagation, then a sound static analyzer to replace the declarations. Each step was defensible alone; the sum was an RPC-style call graph that callers had to hold permissions across and compiled bytes had to prove. Four mistakes: (1) a byproduct of compiled code was hand-recorded; (2) a fact about code was allowed to rewrite the ACL on both sides; (3) the machine boundary (WIT) was conflated with the human boundary (roles); (4) static analysis of mutable Wasm state was asked to be a security boundary when the instance already was one. Standard practice everywhere: the requirement is the entry, the grant is admin data, code trust is per package, and what the code reaches is information.

Cross-application calls are unchanged: a workflow call, separate invocation, separate grant, no shared transaction. Compensation for cross-application failure is a business concern, not a platform one.

## `wamn:postgres` stays 0.3

Today the package has three interfaces (`package.wit`): `types` (pure types), `client` (raw SQL: `query(sql)`, `execute(sql)`, guest-owned `begin`/`commit`/`rollback`, cursors), and `statements` (admitted statements by path: `run`, `run-stream`, `operation-transaction`, plus guest `begin`, `commit`/`rollback` on the resource, and the participant trio `selected-participation`, `select-participant`, `participant-view`).

Amended by the owner ruling of 2026-10-14: Cleanup issue 2 (wamn-ponya.2, 0571d16aa) deleted the participant functions and moved `wamn:postgres` to 0.4, with every importer in the same commit. The owner accepted the dead `select-participant` import in the deployed Receiving 2.2.0 on 2026-10-15. Environments on platform 0.1.8 keep serving 0.3, and every app is rebuilt on 0.4 before an environment moves to platform 0.1.9. The text below records the A-era decision. No version bump (ruled 2026-10-11; we are early and a bump is churn for nothing). `statements` gains one host refusal: `run`/`run-stream` inside a host transaction. The participant functions stay in the WIT and keep working. Fact: Receiving's component calls `selected-participation` on every `record_receipt` (component/src/lib.rs:107, via `participation()` in postgres-statements lib.rs:407), and Acme's participant calls `participant-view` (participant/src/lib.rs:106). A refusal would break current Receiving. Deleted in Cleanup after migration, after verifying no deployed component calls them. Nothing is renamed and no interface is added.

| Change to `statements` | Reason |
| --- | --- |
| Leave `selected-participation`, `select-participant`, `participant-view`, `transaction-view` in place, unrefused | Participant path retired by migration, not by refusal. Current Receiving calls `selected-participation` on every `record_receipt` (component/src/lib.rs:107), so a refusal breaks it. Deleted in Cleanup after migration once the replacement components are deployed; removing them now would force a version bump. |
| **Refuse `run` and `run-stream` while the invocation has a host transaction** | Today `run` goes through `one_shot` on its own checked-out connection with its own commit (`resources.rs:556`, `claims/transactions.rs:374`). A member inside a host transaction could write through it, then return `Err`; the host rolls back and the side write stays. Inside a command, SQL goes through `operation-transaction` only. Same place `begin` already refuses. |
| Keep `begin`, `transaction.commit`, `transaction.rollback` | Receiving's reads use guest transactions (`read.rs:84-163`). `begin` already refuses inside a host-transaction operation (`data-access.md:365`), and the lent handle already refuses `commit`/`rollback` (`resources.rs:118-126,1496`). Nothing to remove. |

Application admission: an application member imports `wamn:postgres/types` and `wamn:postgres/statements@0.3`, as today. `client` is denied to application components in A. Fact: component-policy rows are per package (`wamn:postgres@0.3.0`, lib.rs:147-152), so this is not one line today; it rides on the policy change A needs anyway — matching by interface instance — after which denying `client` to application components is one row. Platform components (apps/platform/data/postgres-sqlx, apps/platform/execution/materializer) keep it. The admission test at component\_admission.rs:639 changes. Until that row lands, A's isolation claim is isolation through `statements` only; raw SQL via `client` is an open hole that predates this design. No version is refused at publish.

A committed-read-only interface for members that must not join the transaction is a possible later addition (Deferred). The instance mechanism below is additive to it.

## Statement identity

Statements are keyed by path on `main` (`2ebd701`): the host's verified sets are maps by statement path and `resolve_statement` takes a path (`statements.rs:69-91,207-215`). The digest-keyed description in earlier drafts was against `49e116e` and is withdrawn; no migration is proposed. The guest never carries SQL; the host owns the bytes and resolves the path in the operation's admitted set. That is the security property.

Open, and not part of this spike: a path is qualified by the owning package only if the path itself carries it. Attribution of a statement to its package in audit (`record-history.sql:200` reads `app.operation`) is deferred host work; the import instance identifies the member when it is wanted.

## Security posture

Authority is decided at publish, never per call (ruling 2026-09-24). Inside one store the host cannot tell members apart by caller (`resources.rs:1342`). The component model's own answer: **each member's `wamn:postgres/statements` import is a distinct instance**, and the host binds each instance to that member's statement set. One rule, no tiers: a member that imports `statements` gets its own instance bound to its own statements; a member that does not import it cannot touch the database, because there is nothing to link.

### Per-member import instances

- Each member imports the one interface type `wamn:postgres/statements@0.3.0` under a member-specific instance name in the composed world. The generator names each outer import by the member's position in the composition (`sql-0`, `sql-1`) and records the member digest beside it as provenance, so the name is unique by construction and still derived. A digest alone would collide if identical bytes were instantiated twice; one-component-per-package rules that out today, and the ordinal removes the assumption anyway. The interface id does not change. wac wires each member's import to its outer import.
- The host links one instance per import name (`wasmtime::component::Linker::instance`) and keys the active statement set by (component id, import instance) instead of component id alone (`active_statement_set`, `resources.rs:1353`).
- Both instances lend the same transaction. Only the admitted statement set differs. A member naming a sibling's statement gets `unknown-statement`, as a stranger does today.
- Each member's set is its own operations' declared statements. No per-entry narrowing: authorization is the entry's permission (Mechanism), and ownership is the instance. The OCI-pinned member components and their `statements` declarations are the facts; no second record is kept.
- **Wiring is verified in the composed artifact, not assumed from member digests.** Two imports share one WIT type, so a valid `wac` composition could wire Acme's member to the instance meant for Receiving. Admission reads the composed artifact's instantiation graph and checks that each member's `statements` import resolves to the outer import whose recorded provenance is that member's digest. Structural check on bytes; no declaration. Negative test: swap the two bindings, publish refuses.
- **Admission change, not just linking.** `component-policy` admits capabilities by `namespace:package@version` parsed from the import string (`component-policy/src/lib.rs:140-154,221`). A named instance import has a local name, so policy must match on the import's *instance type*. That is the WIT-native check and the one piece of platform work in the core. The spike proves `wac` composition, policy admission, wiring verification, and the `wash-runtime` Postgres binding together.
- **Fallback if named imports are refused by tooling:** a generated adapter component per member that exports `statements` and imports a platform-only member-tagged interface. One extra in-process hop per SQL call. Deferred unless the primary fails.

This keeps `data-access.md:61-62` intact: an overlay mutates base rows only through a base operation. Acme calls `receiving::receive-lines`; Receiving's code runs Receiving's statements under Receiving's instance. Acme cannot name that statement itself. No per-call policy check runs; the import name is the identity, fixed at link.

### Cost of per-member instances

Expected small, to be measured in the link test; the interface stays single. Every member imports the same WIT interface type, `wamn:postgres/statements`, under a member-specific import name. Generated bindings are identical code; the name is a link-time label.

| Where | Today | Proposed | Delta |
| --- | --- | --- | --- |
| Per call | guest → host trampoline → `active_statement_set(component_id)` | same trampoline → `active_statement_set((component_id, instance))`; the instance closure already holds its member key | one hashmap key becomes a tuple; no extra crossing or copy |
| Instantiation | one `Linker::instance` for `wamn:postgres` | N instances, one per member | paid once per digest when the `InstancePre` is built and cached, not per request |
| Store creation | one host-state pointer | N small pointers | negligible |
| Memory | each member carries its own binding glue (`wac` does not dedupe) | same | none |

Two ways to do it wrong: define the linker per request instead of per digest; or let members import different `wamn:postgres` versions, which would mean N ABIs. One version per release is the rule.

Prior art: WASI scopes `wasi:filesystem` by preopen — one interface, several instances with separate authority. Inference: the per-call analysis follows from how wasmtime implements component imports (a closure per import definition); not measured. The link test is the place for a before/after microbenchmark if a number is wanted.

### Laws

1. A member's instance binds to the union of its own operations' declared statements. This is wider than today's per-entry binding (`operation.rs:661`): Receiving's record\_receipt code could name void\_receipt's statement, and a Receiving read could too. Accepted: a package trusts its own code, as a crate does; a read that writes is its author's bug, and the profile screen shows it. Composition never widens a set to another package.
2. Mutating base-owned business state goes through the base's exports (`data-access.md:61-62`). Reading or locking base relations within a member's own admitted statements is allowed; Acme already locks `purchase_order` for its overlay-owned fields.
3. Exports re-check their preconditions from the database. A sibling in the same store can forge any record it passes (separate memories, shared data), so typestate from a sibling is not proof.
4. Members never share host resources. Each obtains its own from its own import instance; none crosses an export (`components.md:44`).
5. Authorization is the ACL row for the entry, on both sides: required = {entry}, held = the role's rows. The call graph never adds to or subtracts from either. What the entry reaches is the author's responsibility and the admin's information.

### What does not change

- Admission embeds each member unchanged under its digest (`components.md:47`).
- Cross-application: workflow call, separate invocation.
- Recovery model: unchanged.
- Row isolation: RLS by tenant on the authenticated database role (`data-access.md:66`).

### What is accepted

- Per-member audit attribution is host work, not a side effect: record history reads `app.operation` (`record-history.sql:200`). Deferred.
- One `operation_transaction()` lookup per member instead of per operation. Negligible.
- Inference to confirm in the spike: `wac` renames a member's import without touching its exports, and wasmtime accepts two instances of one interface type under two names in one component. Documented component-model behavior; not exercised in wamn today.

## Retired

The participant path inverted control: the base called out to an extension it had to predict. Every new need was a new slot in the base, a regenerate, and a republish. Everything in this table is a kept deletion: it lands in Cleanup after migration, after Acme's entry owns the inspection SQL and the replacement Receiving and Acme components are deployed. Until then the code stays and keeps working; current Receiving still calls it.

| Item | Where | Replacement |
| --- | --- | --- |
| `pre_commit`, `participant`, `pre_commit_required` manifest keys | `data-access.md:156,308-316` | Caller sequencing |
| Generated no-op participant crates | `data-access.md:316` | None needed |
| `select-participant`, `selected-participation`, `participant-view`, `transaction-view` | `package.wit:159-168`, `resources.rs:1231,1371`, `transaction_views.rs` | `operation-transaction` in each member |
| Participation intent in the request hash | `data-access.md:334,375` | Hash of the entry's canonical item alone |
| `execute_with_pre_commit`, `RecordReceiptPreCommit` | `apps/wamn_receiving/data/src/record_receipt.rs:365-380,569-585` | Granular exports |
| Acme's participant implementation | `apps/` Acme package | Acme sequences Receiving's exports |

The step language and the generated `Steps` trait discussed on 2026-10-10 are not adopted. They solved the same problem with a new DSL where call order already does it.

Idempotency (ruled 2026-10-12): a key identifies one request to one operation. Acme's `record_receipt` has SQL of its own (the inspection statement), so it claims under its own operation, `acme.record_receipt`, as an ordinary `idempotent_by = claim` command with `transaction = explicit_per_input`. It no longer inherits Receiving's claim: `idempotent_by = {inherited = …}` (wamn.k:55,104) existed for the participant-era SQL-free orchestrator, and dies with it (Cleanup). Consequences, all accepted: (1) a key recorded through `receiving.record_receipt` and resubmitted through `acme.record_receipt` is a second request to a second operation — it runs, Acme's rules apply, and Receiving's row rules decide whether a second receipt exists; the base write entry in a client application is therefore not a routed target (authoring rule; ACL after C). (2) Participant-era keys stored under `receiving.record_receipt` are not replayed by Acme's entry at all; a retry through Acme runs again. Receiving and Acme are portfolio applications on wamn-dev with no production data. The request hash drops the participation term (intent.rs:139-147) and is the canonical item alone. `write_log` stays keyed (operation, idempotency\_key), operation versionless (app-schema.sql:630-632), so a retry across a release of the same operation answers the stored result. Cached replay across two entry points with different business rules is exactly what own-operation claims rule out.

## Cleanup after migration

Landed 2026-10-15 (epic wamn-ponya): 2d5250b29, 0571d16aa and bbb2d663c. The participant path, inherited claims, `effect_free_operation_dependencies` and inherited effects are deleted. The fold and `operations[].dependencies` stay, because nothing proves that the authored `transaction` field can replace the fold, and no published app has an SQL-free orchestrator to test it. Bead wamn-azko6 carries the replacement and the deletion of the consumers. `wamn:postgres` moved to 0.4 under the owner ruling of 2026-10-14.

Ruled 2026-10-11: deletions are kept, not dropped. They land after the Acme migration, with no version bump. A deletion is written here the day it is decided and leaves only by landing. The rule for each: delete when nothing published imports or calls it; if something still does, migrate that first rather than bump a version.

- Participant path: the four functions in `statements` (`selected-participation`, `select-participant`, `participant-view`, `transaction-view`); the host code behind them (resources.rs:1220-1240, :1371, transaction\_views.rs); `participation()` and `participant_view()` in postgres-statements lib.rs:407-427; manifest keys `pre_commit`, `participant`, `pre_commit_required`, `transaction = participant`; `idempotent_by = inherited` (`CommandIdempotence::Inherited`, `InheritedClaimDeclaration`, manifest.rs:235-262,1313-1420; `claim_operation` in intent.rs:61,370 and the serving manifest route); generated no-op participant crates; Acme's participant implementation; the participation term in `item_input_hash` (intent.rs:139-147) and its `RecordReceiptPreCommit` plumbing. Precondition: Acme's `record_receipt` owns the inspection SQL, claims under its own operation, and depends on `receiving.record_receipt`; the replacement Receiving component no longer calls `participation()` (today component/src/lib.rs:107 does, unconditionally); the registry shows no deployed component importing the four.
- `client`: the deny row for application components lands in A. Cleanup removes the fixtures that still import it (apps/platform/fixtures/prior-commit; the `fresh_only` live test goes with C) and the `client@0.1.0` admission test at component\_admission.rs:639.
- `operations[].dependencies`: every consumer on main — serving\_manifest.rs:302-375 (`fold_call_graph`: statements, permissions, `fresh_only`, participant; permissions and `fresh_only` go with C), component\_facts.rs:933-956 (pin-conflict check; participant→import derivation), engine component\_admission.rs:299 (embedded-base digest check), control/lib/component\_admission.rs:394 (contract participant match), component-composer/src/lib.rs:81-100 (wires base operations into the overlay from the declaration), publish\_release/components.rs:191,267,298,487 (dependency-closure resolution and effect check). Replacements before deletion: the embedded-base digest check and closure resolution move onto `base_dependencies` (package, digest); the composer reads members from the application manifest (`components`, `base_dependencies`); the rest die with the participant. Prerequisite for deleting: an SQL-free orchestrator calling an SQL-bearing member is admitted and runs in one host transaction without the fold. Today it cannot: `has_host_transaction()` requires local statements (manifest.rs:161) and validation admits only `(no local SQL, transaction = None)` (manifest.rs:1238-1240), so the authored `transaction` field is a candidate replacement, not a proven one. Prove it or keep the fold. `lower_statement_set` (operation.rs:661-690) is keyed by member instance in A.
- Documentation: data-access.md:305-336 participant path and :313 fold; components.md:43-50; execution.md:212-214 attribution switch; the platform-ui.md amendments listed below.

Not in this list because they do not wait on migration: C's deletions (both permission folds, `required_by`, `fresh_only`). Those are C.

## Authoring discipline

The one thing composition cannot avoid: a base package exports operations at the granularity a composer needs. Receiving's `record_receipt_in` is six `generated::*` calls in a fixed order (`record_receipt.rs:456-585`). Naming them is not the work; making each safe alone is. `finish_purchase_order.sql` bumps `row_version` on any open purchase order whether or not a receipt was inserted; as an export it needs a precondition it does not have today. Expect one such check per export.

Database state cannot prove that an earlier step ran *in this command*: an existing receipt is not proof that this invocation inserted it. Two mechanisms make an export safe alone, in order of preference:

1. **Coarse exports.** Export a complete unit of work, not a SQL step: `receive-lines(po, lines)` does lock, validate, insert, and finish itself. Acme inserts before or after it, not between. This covers most composition and needs no new mechanism. The granular trio in the Mechanism section is an illustration, not the recommended surface.
2. **Member-local invocation state** where a composer must insert between steps. Receiving records in its own linear memory what it did this invocation (`lock-purchase-order` records the locked id; `insert-receipt` refuses unless Receiving itself locked that id). Siblings cannot forge it: memories are separate, and the store is fresh per request. Under trusted warm reuse the existing request-local-data rule applies (`execution.md:228`).

- Export a step only if it is safe alone. Each export re-reads what it depends on (status, lock, existence) rather than trusting its input record (Law 3).
- Keep the whole-operation export too (`record-receipt.run`). Most callers want it. Granular exports are for composers.
- Export names are a public contract. Renaming or removing one breaks dependents at their publish, named by export, the same way the workflow gate names a failing step. Never at runtime.
- Effects stay outside the transaction. Exports do SQL and in-process logic. Delivery is after commit via CDC, as today.
- Ordinary commands that nobody composes stay one function. Granularity is opt-in per package, not a platform requirement.

Exposure. A declared operation is reachable by a route and by workflow targets, which resolve declared operations from admitted facts (`publish_release/components.rs:140`). Reachability is not authorization: with `required = {entry}` and `held = rows`, an operation no role is granted cannot be invoked by anyone through either path. Scenario 1's constraint therefore holds by not granting `receiving.receive-lines` to operator roles; no exposed-operations list is needed. Receiving adds nothing for an Acme UI to call its operations; the application attaches routes, and the caller needs the row for that operation.

## Scenarios

Base: Receiving (`purchase_order`, `receipt`, `receipt_line`, command `receive-lines`). Overlay: Acme. All members of one application share that application's database; a second database is a separate connection and never part of the operation transaction.

### 1. Constrain — the overlay narrows what the base allows

Acme forbids partial receipts: a receipt must cover every open line of the purchase order. Receiving allows partial receipts (it already refuses over-receipt itself with `quantity_exceeds_remaining`, `validate_receipt_line.sql:60`, so that is not an overlay constraint).

```rust
// acme entry (imports statements)
let r = receiving::receive_lines(po, lines)?;                 // base does its work, PO lines locked
let open = acme::select_open_lines(po)?;                      // acme's own SELECT, same transaction
if !open.is_empty() { return Err(AcmeError::PartialReceipt(open)); }   // Err → host ROLLBACK
```

- The check is a **post-condition in the same transaction**. It runs after the base, under the base's `FOR UPDATE` lock, so it cannot race. A pre-check before `receive-lines` could.
- The constraint holds because operator roles are granted `acme.receive` and not `receiving.receive-lines`. The ACL is the enforcement, against routes and workflow targets alike — after C. Under A alone `ReleaseClosures::expand` still makes a reached operation's permission effective when the entry is granted, so until C lands the constraint depends on the application not routing `receiving.receive-lines` as a target of its own. The platform does not make an overlay mandatory; the admin does, by what they grant.
- A constraint the overlay wants enforced even against direct base calls belongs in the schema (a `CHECK` on an overlay-owned column) or in the base.

### 2. Extend — the overlay adds data and behavior around the base

Acme records a quality inspection when the purchase order requires one, and adds `acme_inspection_required` and `acme_quality_status` to `purchase_order`. The existing participant SQL has three outcomes (`record_receipt_participant.sql:17-19`): not required → no row; required and approved → inspection row; required and not approved → refuse, rolling back the receipt. All three must survive.

```rust
let r = receiving::receive_lines(po, lines)?;
match acme::inspection_outcome(po)? {                         // reads and locks purchase_order via Acme's instance
    NotRequired => {}
    Approved    => acme::insert_inspection(&r.receipt_id, po)?,
    NotApproved => return Err(AcmeError::QualityNotApproved),  // host ROLLBACK
}
```

- The overlay-owned column uses the existing field-extension model (`data-access.md:57-58`). No change.
- `approve-inspection` is an Acme-only operation with its own route. It never touches Receiving.
- This is the current Acme participant, expressed as two calls.

### 3. Adjacent — the overlay sits beside the base and never enters its command

Lot tracking keeps `lot` and `lot_assignment` tables keyed by `receipt_line.id`, has its own routes, and wants to act when a receipt is recorded.

- Reads of base data: committed, through its own statements instance. Lot tracking is a member for schema and routes, not for the receiving command.
- Reaction to a receipt: **after commit**, through the existing CDC/history path, as a workflow trigger. Not in the transaction. Eventually consistent by design.
- If lot assignment must be atomic with the receipt, it is not adjacent; it is case 2, and the app's entry calls it.
- Analytics in a second database: a member bound to that connection (inference: the same shape as environment connection bindings for blobstore). It consumes CDC; it is never in the operation transaction.

### 3b. Mid-transaction — the overlay must act between base steps

Acme assigns a bin per line, and Receiving's insert must store the bin. Call order around a single `receive-lines` cannot do this; the overlay has to run inside the base's sequence.

A mid-transaction *constraint* usually needs nothing: rollback is whole-item, so a refusal after `receive-lines` ends in the same state as a refusal between its steps. Only wasted work differs. A mid-transaction *extension that later base steps consume* needs the base to expose the seam. Two shapes, steps first.

**Steps (default).** The base exports its unit of work and the steps that make it up; the unit is implemented by the steps.

```wit
receive-lines:         func(po: uuid, lines: list<line>) -> result<receipt, error>;  // = the three below
lock-purchase-order:   func(po: uuid) -> result<purchase-order, error>;
insert-lines:          func(po: uuid, lines: list<line>) -> result<receipt, error>;
finish-purchase-order: func(po: uuid) -> result<_, error>;
```

```rust
let p = receiving::lock_purchase_order(po)?;
let bins = acme::assign_bins(&lines)?;                       // Acme's instance, same transaction
let r = receiving::insert_lines(po, with_bins(lines, bins))?;
acme::insert_inspection(&r.receipt_id)?;
receiving::finish_purchase_order(po)?;
```

Safety: Receiving keeps a per-invocation state machine in its own linear memory: `Idle → Locked(po) → Inserted(po, receipt) → Finished`. Each step consumes the state it requires and produces the next; a step called twice, out of order, or for a different purchase order or receipt than the state names refuses. That gives exactly-once and correct-receipt, not merely something-happened-earlier. A sibling cannot forge it: memories are separate. A fresh store per request resets it; trusted warm reuse falls under the existing request-local-data rule (`execution.md:228`). Callers that need no seam call `receive-lines` and never see the steps. Granular steps stay experimental until a real overlay needs a seam; coarse, independently valid operations remain the default.

**Import (per-iteration or private-state seams only).** The base imports an interface and calls it where variation belongs; it ships a no-op default, and the application links the overlay's implementation instead.

```wit
// receiving world
import line-policy: interface { on-line: func(line: line) -> result<line, error>; }
```

Use it when the seam is inside a loop the base owns (once per line during validation), or when the base's intermediate state should not cross the boundary as a record. It is a declared extension point, so the base author decides where. That is unavoidable in any model; it is the same as a Rust crate exposing a trait. Unlike the retired participant path it is a plain build-time link: no selection, no view, no attribution switch, no generated no-op crate.

What is still not covered: inserting between base steps the base did not export. The base names its seams, as steps or as imports. Prior art says those are finite and designed (SAP BAdIs, Dynamics event handlers, Odoo inheritance points): pricing, allocation, per-line policy, approvals.

### 4. Mix — one client application

Acme application = Receiving + Acme + LotTracking + Analytics.

| Member | Imports | In the receive transaction | Routes it owns |
| --- | --- | --- | --- |
| Receiving | statements | yes, called by Acme's entry | none routed directly |
| Acme | statements, entry | yes: tolerance check, inspection row | `receive`, `approve-inspection` |
| LotTracking | statements | no; reacts via CDC workflow | `assign-lot`, `lot-history` |
| Analytics | statements, on the analytics connection | no; consumes CDC | `dashboards` (reads) |

One entry, one transaction, two members in it. Everything else is after commit.

### What the scenarios show

- Constrain and extend are both "call the base, then do your thing, return Err to refuse". Same shape, different intent.
- Adjacent is not composition; it is the existing event path. The model does not need to cover it.
- The new authoring burden is the base exporting a complete unit of work that returns its ids, plus its steps where composers need a seam, guarded by local state.
- Mid-transaction insertion is covered by base-exported steps guarded by member-local state, with imports for per-iteration seams. The base names its seams; nothing else can.

## Amendments to `platform-ui.md`

The Platform UI spec (`docs/plan/platform-ui.md`, main `b7f69a1`) predates the ACL ruling and carries the permission closure through its text. The code already took the simple side: `app-schema.sql:318-325` says every permission row is a direct selection, `required_by = permission`, and "no row holds a closure derived from a release"; the host expands at request time instead. So `required_by` is a vestigial self-referencing column, and the spec's reconciliation sections describe behavior nobody built. This spec is the authority; `platform-ui.md` follows it in its own worktree, citing this section.

| Section | Today | Becomes |
| --- | --- | --- |
| §2.2 roles "may hold additional permissions required by those selected operations" | closure clause | deleted |
| §2.3 invariant "caller must hold every operation the released call graph reaches"; `authorize_released_operation` checks entry + fold | both folds | `caller holds entry`. The flow diagram loses its fold step. |
| §2.3 `required_by` model; §2.4 grant/revoke closure semantics; lines 1076–1078 | closure | grant inserts a row, revoke deletes it. No "still required by", no refusal of a derived row. |
| §4.1 key `(tenant, role, permission, required_by)`; migration `0002` adding the column | vestigial | drop `required_by`; key is `(tenant, role, permission)`; the self-FK and its regex CHECK go |
| §4.1 Release reconciliation (530–589) | recompute closures per release | gone. One rule stays: a stored reference the serving release does not serve grants nothing. The row is kept; history already records who granted it and when (`wamn_record_history_stamp`); `permission.list` marks it **deprecated**. The admin removes it when they choose. |
| §2.5 Non-grantable operations | the ACL-editing operations (`role.*`, `permission.*`, `user_role.*`, `user.*`) are reserved to `admin` | Ruling: everything is grantable. Which roles administer which other roles is a tenant's business decision like any other grant. The one platform rule is no escalation beyond yourself: an ACL-editing operation may confer only what its caller already holds, with `admin` treated as the full set (it has no rows, so a row-subset test would pass vacuously). Prior art: Kubernetes RBAC escalation prevention; AWS IAM permission boundaries. Scope: the ruling is recorded here; the code is a follow-up after C, rescoped then (cut 2026-10-11 so C stays pure deletion). Until it lands, §2.5 behavior is unchanged: ACL-editing operations stay reserved to `admin`. The follow-up as sketched, to be re-examined: (1) a third host-route authority, `Granted`, so the route keeps an operation identity for the ACL check (`registered_operation()` returns one only for Admin today, host\_route.rs:421-423; a Member route would be open to every authenticated member); (2) `is_host_route_operation` stops refusing grants of these operations (application\_route.rs:168); (3) the escalation check in each handler; (4) `UserRoleGrant` takes `OPERATION_GRANT_LOCK_SQL` like `PermissionGrant` (application\_route.rs:217,281). `permission.mine` stays readable by any session. |
| `permission.list` and the role grid (1534–1580) | roots, effective rows, `still_required_by`, `admin_only`, `grantable` | one row per served operation, one toggle, `deprecated` where unserved; once B exists, the derived profile (reaches, reads, writes) beside it as information |
| 1292, 1746 copying rows between environments | "does not copy derived rows" | rows copy as-is; nothing is derived |
| Decision log 1758 "Permission model — accepted" | closure accepted | superseded 2026-10-11, pointer here |

Unchanged and still right: stable versionless references (`sealed_operation_reference`); `admin` holds everything; grant timing (§2.6); host-owned writes with `wamn_app` SELECT-only on the auth tables (§2.1); the materialized administrative hierarchy (§2.8); role slugs and the CLI verbs.

Code that goes with the spec change: release\_closures.rs; the `ReleaseClosures` use in claims.rs:83-98 and route\_authentication.rs; role\_permissions.rs:54 passing `current.closure(reference)`; `required_by` in the DDL, migration and the eight files that reference it; the permission fold and `fresh_only` in serving\_manifest.rs. Nothing is added. Grantable administration is the follow-up.

## Deferred — build when a consumer exists

Four review rounds each found a way the previous mechanism was incomplete, and each got a mechanism in reply. Most are correct and none has a consumer. They are recorded here so they are not re-litigated, and so nothing ships without a stated need.

| Item | Why it was proposed | Why it waits |
| --- | --- | --- |
| Committed-read-only interface ("Reader") | A foreign member that reads but must not join the transaction | No such member exists. The better shape is push: the entry reads and passes records; the component imports nothing. Additive when needed. |
| Step exports + per-invocation state machine (§3b) | An overlay that must act between base steps | No overlay needs a seam. Acme's case is an after-insertion. The written answer stands; nothing is built. |
| Exposed-operations list in the application manifest | Hide a base operation from routes and workflows so an overlay constraint cannot be bypassed | Resolved by the ACL, not deferred. With required = {entry} and held = rows, a base operation that no role is granted is unreachable by route or workflow. No list needed. |
| Per-member audit attribution | Record which member ran a statement | Record history reads `app.operation`; a session setting per instance is host work with no consumer yet. |
| Adapter component per member | If named instance imports are refused by `wac` or policy | Only if the primary link test fails. |
| Participant-era idempotency compatibility branch | Old Acme claims carry `participation_intent` in the stored hash | Portfolio applications on `wamn-dev`, no production data. Conflict accepted at the boundary. |
| Transitive permission closure (caller holds every reached operation's permission) | Stop an overlay author exposing a cheap operation that calls an expensive one | Retired, not deferred (ruled 2026-10-11). The requirement is the entry; what it reaches is a review artifact. The escalation it guarded against is an authoring bug the artifact makes visible. See Mechanism for the full reasoning; do not reintroduce without a case the artifact cannot catch. |
| Per-entry narrowing of member statement sets | Least privilege inside one invocation | Ownership is already per instance (Receiving's statements run only under Receiving's code). Narrowing to the entry's reach adds nothing a caller can exploit; it only shrinks a set the member's own code controls. |
| SQL-free entry admission rule | A pure orchestrator command with no local SQL | Acme's real entry owns the inspection SQL once the participant goes (wamn.k:51-108), so it is an ordinary command with a base dependency. No pure orchestrator exists. Falls out of the derived graph anyway when one does. |
| Other consumers of the effect profile (ACL review screen, stateless derivation, connection binding, recovery-class check, edge tiering) | The derived profile answers each question someone answers by hand today | The extraction is built once for the two consumers this spec needs. Each other consumer is its own decision when it has a case; listed so nobody re-declares what the bytes say. |
| Converting existing read exports to operation-transaction() so a command can call them and see in-flight state | Nested reads inside a command | Receiving's reads use guest begin/commit (read.rs:84-163), which refuses inside a host transaction. No composer needs a nested read yet; Acme passes records. Supported limitation of spike A; convert per read when a composer needs one. |

### Over-verification removed

Things earlier drafts verified that WIT or OCI already prove:

- **A hand-written per-operation call graph, in any form.** Three drafts in turn: a new `uses` key; "WIT alone is the graph"; keep the existing `operations[].dependencies` and trust it. All withdrawn. WIT proves which sibling exports a member *can* reach, not which export calls which, and carries no SQL mapping — but the compiled module's static `call` graph from each export to each lowered import does say which export calls which. That is a byproduct of the code and is extracted post-build; nobody writes it down. `operations[].dependencies` retires. What remains declared is what cannot be derived: `base_dependencies` (which package, which digest) and `statements` (which SQL an operation owns).
- **A transitive permission requirement built from that graph.** Caller holds the entry's permission; what the entry reaches is the author's responsibility and the reviewer's information. Making it a requirement doubled the role-configuration surface for no security the per-member instance did not already provide. Reasoning in Mechanism.
- **Tier declaration in the manifest.** A member's capability is its imports. No import, no capability. Nothing to declare.
- **Re-hashing admitted SQL at bind.** Already gone on `main`; statements are path-keyed and the release is OCI digest-pinned.
- **A separate "release records per member" fact. Same principle as the 2026-10-11 ruling that package facts have one owner, the OCI registry (20efa90).** The admitted member components, pinned by digest, are the record.
- Byte-identical generated output as an acceptance test. Replaced by unchanged flow tests; generated code changes shape (instance names) even with the WIT version fixed.
- **A new interface name (`transaction`) and removing `begin`/`commit`/`rollback`.** `statements` minus the participant trio and `transaction-view` is the same thing with no rename. Guest `begin`/`commit`/`rollback` stay: Receiving's reads use them (`read.rs:84-163`) and they already refuse inside a host-transaction operation, which is the only property needed.

## Spike and acceptance

Result (owner acceptance 2026-10-15): all four items are closed. The checklists below stay as written, and the commits are the record.

- C, authorization: epic `wamn-14bx8`, 89e57dc3c, 132b413f1, 99fc8c5b6 and 2464b06f9.
- A, composition: epic `wamn-lqtai`, 8d04837c7, 15ecd4b5e, aec1a85e9 and 20c66f535.
- Cleanup after migration: epic `wamn-ponya`, 2d5250b29, 0571d16aa, bbb2d663c, 9083d4f55 and c71437d7f. The fold and `operations[].dependencies` went in 8278c9fa2 (`wamn-azko6`), because the host now begins an item transaction from the authored `transaction` field.
- B, profile screen: epic `wamn-aemxz`, 3bde5d2bc, aeb976ed3 and a0b0c8503. By the owner ruling on `wamn-j157o`, the per-role view is on the project screen and the role grid has no profile (3b5da9d51).

Left open and not scheduled: `wamn-xt0tg` and `wamn-b71vg` (wire contracts, with the next contract bump), grantable administration (needs an owner sentence), `wamn-t4hrr` (repo-policy drift) and `wamn-j7qz7` (PAT overlays).

Three independent items, each shippable alone. Earlier drafts let B and C ride on A; they are not prerequisites for proving per-member transaction propagation, and A runs with the current ACL untouched. Each item's tests are its own gate.

**A. Composition spike** (what this spec exists for; current ACL retained for the experiment)

- [x] Link (gating): two fixture members import `wamn:postgres/statements@0.3` as ordinal-named instances with digest provenance; wac composes it; component-policy admits it by instance type; admission verifies each member's import resolves to its own outer instance; the wash-runtime Postgres binding serves two instances with two statement sets on one transaction. All through the real host. Test: `crates/execution/host/src/operation/native_policy/tests/authenticated.rs::native_member_statement_instances`; measurements and wiring on `wamn-lqtai.1`.
- [x] Wiring (negative): swap the two members' Postgres bindings in the composed artifact; publish refuses. Same bytes per member, different graph. Test: `crates/execution/host/src/operation/native_policy/tests/authenticated.rs::native_member_statement_instances` (`swap_statement_bindings`, refused `StatementInstanceMismatch`).
- [ ] Authority: Acme names a Receiving statement through Acme's instance; refused `unknown-statement`. Receiving's instance runs Receiving's statements under Acme's entry. Not ticked: `crates/execution/host/src/operation/native_policy/tests/authenticated.rs::native_member_statement_instances` tests this on fixture members only, not on the real Acme and Receiving.
- [x] Real Acme behavior: Acme's `record_receipt` owns the inspection statement (moved from the participant) and depends on `receiving.record_receipt`. Entry calls Receiving's existing operation, evaluates the inspection outcome, inserts the `quality_inspection` row when approved. Not required → receipt committed, no row; approved → row; not approved → refused, nothing committed, including the receipt. One `write_log` entry on commit. No step-shaped export is needed for A. Test: `apps/wamn_receiving/tests/route_authentication_live/local_business.rs`::command_histories (`acme_inspection`); `wamn-lqtai.3` close.
- [ ] Idempotency: `acme.record_receipt` claims under its own operation. An unchanged key replayed through Acme answers the stored result within and across a release. Cross-entry: the same key submitted through `receiving.record_receipt` and then `acme.record_receipt` is two claims; the second runs Acme's inspection logic (asserted) and the outcome on the receipt rows is recorded, not asserted. A participant-era key resubmitted through Acme runs again (no `idempotency_conflict`: different operation). Not ticked: `apps/wamn_receiving/tests/route_authentication_live/local_business.rs`::an_acme_key_replays_across_a_release simulates the release by copying the claim.
- [x] Atomicity: a member inside a host transaction calls `statements.run`; refused. Same for `run-stream`. The same statement through `operation-transaction` succeeds. Outside a host transaction, `run` and guest `begin` work as today. No admitted Postgres write survives a rolled-back item by any path; external effects are out of scope (CDC after commit). Test: `crates/platform/runtime/src/plugins/wamn_postgres/operation_transaction/tests.rs::the_host_owns_the_operation_transaction_and_the_guest_works_in_it`; rollback in `crates/execution/host/src/operation/native_policy/tests/authenticated.rs::native_member_statement_instances`.
- [x] Reads unchanged: `receiving.location_list`, `receipt_screen`, `purchase_order_history` pass with their guest begin/commit untouched and no import change. Test: `apps/wamn_receiving/tests/route_authentication_live/local_business.rs`::command_histories (`reads`); `wamn-lqtai.2` close.
- [x] Nested reads (supported limitation): a command calling an existing read export that uses guest `begin` gets the existing refusal, not uncommitted state. Recorded, not fixed in A; converting reads to `operation-transaction()` is listed in Deferred. Test: `crates/execution/host/src/operation/native_policy/tests/authenticated.rs::native_member_statement_instances` (nested child `begin` refused); `wamn-lqtai.2` close.
- [ ] Begin unchanged: `sample.read` (edge\_samples) still claims in two steps beside SQL `sample.record`; an SQL-free entry depending on an SQL operation gets a host transaction through the fold as today; a read never does. Publish: an application member importing `wamn:postgres/client` is refused once policy matches by interface instance; platform components still pass. No version is refused. Not ticked: no test runs `sample.read`; `crates/platform/runtime/src/plugins/wamn_postgres/write_log/tests.rs::the_two_step_write_log_keeps_the_intent_rules` covers two steps in general, and 8278c9fa2 replaced the fold.
- [x] Compatibility: an application with no overlay passes its flow tests unchanged. Evidence: WMS, which has no overlay, passed `local_business` unchanged (`wamn-lqtai.2` and `wamn-lqtai.3` close).
- [x] Measure: per-call and per-instantiation cost of two instances versus one, on the link fixture. Evidence: numbers on `wamn-lqtai.1` notes.

**B. Profile screen** (independent of A and C; nothing enforces on it)

- [x] Derived profile on the real Receiving and Acme components: acme.record\_receipt → receiving.record\_receipt, and per export the statements its bindings reach with the tables and columns each reads, writes, or locks. Shown per operation and per role in the admin UI. Imprecision (an export widened to the member union) is displayed as such. Keyed by release, not by component digest: SQL changes without recompiling the component (Statement identity), so the profile joins component reachability with the serving release's statement facts. Test: an SQL-only change to one statement changes the shown profile with no component rebuild. Test: `crates/schema/generator/src/profile.rs::an_sql_only_change_to_one_statement_changes_the_profile_with_no_component_rebuild`; per role on the project screen (3b5da9d51).

**C. Authorization** (your rulings of 2026-10-11; independent of A and B)

- [x] Both sides: an operator role holding only `acme.record_receipt` calls it and succeeds; the same role calling `receiving.record_receipt` directly is refused (passes only once `ReleaseClosures::expand` is gone). Test: `apps/wamn_receiving/tests/route_authentication_live/local_business.rs`::an_entry_grant_runs_the_entry_and_grants_nothing_it_calls.
- [x] `fresh_only` deleted: a `QueuedService` principal explicitly granted a formerly `fresh_only` operation **succeeds**; the same principal without the grant is refused by the ACL alone. Test: `crates/execution/workflow/src/queue/automation_live.rs::a_queued_service_runs_a_formerly_fresh_only_operation_only_with_its_grant`.
- [x] Not in C: grantable administration (`Granted` authority, escalation check, grant lock) is a follow-up after C lands, rescoped then. C leaves ACL-editing operations reserved to `admin` as today; its tests are the two above. Evidence: ACL-editing stays `admin` (`wamn-14bx8.4`); grantable administration is still open and needs an owner sentence.

Pass A: per-member instances with wiring verification, transaction selection unchanged, `run`/`run-stream` refusal in a host transaction, `client` denied to application components — one epic, `wamn:postgres` still 0.3. Fail on the link test: try the adapter fallback before anything else. Fail on rollback or authority: stop; the model is wrong. B has no failure mode that matters; a worse screen is a worse screen. Pass C: both folds, `required_by`, `fresh_only` deleted, with the platform-ui.md amendments — one epic, pure deletion. Follow-up after C: grantable administration, rescoped once C is in. Then Cleanup after migration, in its own change. Nothing from Deferred rides along with any of them.

Open questions:

- Does `wac` rename a member's imports in place, or does it need an adapter per member? The link test answers it.
- Does a step-shaped operation (`receive-lines` returning a typed record) fit the handler signature as declared today? The graph-and-admission test answers it.
- Does a second connection for a member (analytics) take the connection binding shape? Confirm before the Analytics row in Scenarios is treated as settled.

## Epics

Order (owner ruling 2026-10-14). C goes first because it is pure deletion and A also changes `serving_manifest.rs`. Nothing from Deferred rides along with any epic.

1. C, authorization, pure deletion (Spike and acceptance C): `wamn-14bx8`.
2. A, composition spike (Spike and acceptance A). Scoped at C's close.
3. Cleanup after migration (section above). Lands after Acme moves off the participant.
4. B, profile screen (Spike and acceptance B).

## Evidence

Citations are against `main` at `49e116e` read 2026-10-10, rechecked against main 2ebd701 on 2026-10-11 where marked. Main 5bce582 (15 further commits, 2026-10-11) changes none of the cited files.

| Claim | Source |
| --- | --- |
| Transaction looked up by composed component id; same statement set for every member | `crates/platform/runtime/src/plugins/wamn_postgres/resources.rs:1342-1363` |
| Lent handle; `commit`/`rollback` refuse on it | `resources.rs:112-126`, `resources.rs:1496+` |
| `begin` refuses inside an operation | `crates/platform/runtime/wit/deps/wamn-postgres-0.3/package.wit` (`begin` doc comment); `data-access.md:365` |
| One `statements` interface carries reads, transaction, participation | `package.wit:131-200` |
| Host owns begin/commit/rollback; transaction never crosses a wiring edge | `docs/architecture/data-access.md:306,336` |
| Participant path: slots, views, no-op crates, attribution switch | `data-access.md:308-335` |
| Cross-component borrowed resource failed with `mismatched resource types` | `data-access.md:330`; `docs/history/application-simplification-plan.md:145` |
| Resource handles refused at the operation boundary | `docs/architecture/components.md:44` |
| Members embedded unchanged by digest; grant fold | `components.md:47-50`; `data-access.md:313` |
| Receiving's command is six ordered SQL calls | `apps/wamn_receiving/data/src/record_receipt.rs:456-585` |
| Trusted warm-instance reuse exists, opt-in by digest | `docs/architecture/execution.md:212-214`; `docs/operations/deployment.md:629-646` |
| Authorization at publish, never per call; nested calls are component calls | project ruling 2026-09-24 |
| Statements are keyed by path on main (2ebd701); earlier digest keying was at 49e116e | crates/platform/runtime/src/plugins/wamn\_postgres/statements.rs:69-91,207-215 (main 2ebd701) |
| SQL bytes come from the admitted release fact, not the guest | crates/execution/host/src/operation.rs:661-690 (lower\_statement\_set) |
| Overlay mutates base rows only through a base operation; statements planned under package-derived grants | docs/architecture/data-access.md:56-62,164,277-278 |
| Participation intent is part of the request hash | crates/platform/engine/src/operation/intent.rs:147; data-access.md:334,375 |
| Capability admission is by namespace:package@version parsed from the import string; wamn:postgres admitted whole, including raw-SQL client | crates/platform/component-policy/src/lib.rs:140-154,221; engine/src/component\_admission.rs:639 (main 2ebd701) |
| Receiving refuses over-receipt itself (quantity\_exceeds\_remaining) | apps/wamn\_receiving/command/record\_receipt/validate\_receipt\_line.sql:60 |
| Acme participant has three outcomes: not\_required, approved, quality\_not\_approved | apps/client\_acme\_receiving/command/record\_receipt\_participant/record\_receipt\_participant.sql:17-19 |
| Admission requires exports to equal declared operations plus embedded members' exports | crates/platform/engine/src/component\_admission.rs:253-272 |
| Compose test builds Acme over the real Receiving base and asserts the composed component embeds the exact base bytes | 145fec2 (wamn-2l5wi), crates/schema/generator/tests/build\_live.rs |
| Package facts have one owner, the OCI registry; no database stores package verification; the artifact's existence is the proof of its checks (owner ruling 2026-10-11) | 20efa90, 69c1cba (wamn-vavs4.11); docs/plan/platform-deploy.md R2, R11 |
| Admission reads per-operation dependency declarations; a declared dependency is not an import; publish folds the call graph from admitted facts | crates/platform/engine/src/component\_admission.rs:287-300 (main 5bce582) |
| statements.run executes as one\_shot on its own checked-out connection and commits independently of any host transaction | resources.rs:556,588; claims/transactions.rs:365-380 (main 5bce582) |
| Receiving reads use guest begin()/commit() | apps/wamn\_receiving/data/src/read.rs:84,92,103,115,142,163 |
| Required side folds permissions and fresh\_only across the call graph | docs/architecture/execution.md:20,420,446,829; crates/catalog/model/src/serving\_manifest.rs:157-163,327 |
| Held side expands a role's root permissions through the release closure on every request | crates/catalog/model/src/release\_closures.rs:1-14,60-67; crates/platform/runtime/src/plugins/wamn\_postgres/claims.rs:76-98 |
| Permission closure materialized in storage as required\_by rows with revoke cascade | docs/plan/platform-ui.md:107-238 (§2.3, §2.4) |
| ACL-editing host routes carry HostRouteAuthority::Admin before any ACL row is consulted | crates/catalog/model/src/host\_route.rs:167-235 |
| Release builds use thin LTO, so generated accessor boundaries do not survive to wasm | Cargo.toml:259 |
| fresh\_only refuses a QueuedService caller even when it holds the permission | crates/platform/engine/src/router\_delivery.rs:568-591 |

Prior art for callee-joins-ambient-transaction: Java EE CMT and Spring `@Transactional(REQUIRED)`, .NET `TransactionScope`, PostgreSQL SPI/PL‑pgSQL, Rails joinable `transaction`, Mnesia nested transactions. No Wasm platform (Spin, wasmCloud, Golem) does this; each hands out connections per component.
