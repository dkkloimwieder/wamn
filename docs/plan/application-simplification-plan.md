# WAMN platform simplification — implementation plan

**Scope:** platform simplification. Receiving, Acme, and WMS are consumers and test fixtures, not the completion boundary.
Replaces draft 0.5 and absorbs testing-simplification draft 0.1; no parallel plan. Based on the earlier `40b7c361` review and owner decisions, not a fresh audit. Check what has landed before editing.

## Priority: minimize churn

**Choose the target interfaces first. Generate against that target. Update callers and tests once. Delete replaced code as it becomes unused.** Do not improve the old adapters and then replace them again, or redesign the test suite before the implementation settles.

| Order | Work | Completion |
| --- | --- | --- |
| **1** | Platform native async and target component contracts | Production I/O component boundaries use native async. The Receiving/Acme path establishes the pilot pattern. |
| **2** | General SQLx/data-access and adapter generation | Any existing supported model or operation produces its typed interface and standard adapter without generator edits or copied protocol handlers. |
| **3** | Testing simplification | Reduce platform test setup after the general interfaces settle. Keep UI work deferred and retain real-boundary coverage. |
| **4** | Service consolidation | Remove duplicate runtime/process lifecycles after settling the deployment trade. |
| **5 — later feature** | Transactional application extension | Conditional QC participates atomically without copying base logic. **Not required to complete the refactor.** |

Use existing libraries and the pinned upstream runtime. Breaking internal APIs are acceptable; update consumers together without prolonged dual interfaces. Preserve caller/tenant isolation, admitted SQL, transactions/replay, bounded execution and existing warm-reuse policy. Business behavior stays unchanged except for confirmation below. No fork, new registry, framework or crate-count target.

## 1. Native async and target contracts first

Use the **Acme → Receiving → `wamn:postgres/statements`** path as the pilot for native async WIT and generated bindings. Complete the platform rollout across production I/O capabilities, guests, and their host bindings. Cover outbound HTTP, materializer NATS and delivery calls, and the retained PostgreSQL client interface. Preserve pure synchronous transforms and top-level runtime entrypoints where they do no asynchronous I/O. Use `async fn`/`.await`; remove replaced application `block_on`, executor dependencies and bridging code. Reuse the existing P3 HTTP entrypoint. Confirm required binding/toolchain support without an unrelated runtime upgrade.

Generate typed WIT request/result/error contracts and Rust bindings from the existing supported application declarations. Known component calls carry typed values, not serialized JSON through `run(string)`. The demo operations establish a reusable platform pattern; their success alone does not complete platform generation. Update caller, callee, and dispatch integration together. Generate the pilot contracts once, then extend that emitter in step 2. Do not first improve a wrapper around the old JSON contract. Keep JSON at HTTP and genuinely dynamic boundaries. Preserve per-item correlation, exact numbers, absent/null/value updates, authorization, deadlines, and public JSON behavior except the approved revision format. Revision inputs, results, and conflict details use decimal strings in JSON and numeric values in WIT and the database. Generic palettes need not change. No streaming rewrite or concurrency increase.

Return base receipt results unchanged. Delete `receiving_record_receipt_result` enrichment and unused helpers. Update the declared result and generated clients with the server contract. Preserve per-item refusals and uncertainty when posting itself is unconfirmed.

Owner direction: defer UI changes until simplification steps 1–4 are complete. Then reassess receipt confirmation against the settled contracts. Read details separately. Failure or denial leaves “Receipt posted; additional details unavailable.” Never repost or widen permissions. UI design does not block simplification.

Update build/admission consumers with the real native-call and PostgreSQL path. Working async calls and removed adapters establish completion—not WIT spelling or an assumed speedup. Report concrete API blockers; do not add another executor.

## 2. SQLx/data-access generation next

Generalize `crates/schema/generator` across the CRUD and custom-operation shapes that WAMN already supports. Derive package/module names, fields, revision inputs, result cardinality, nested shapes, and errors from existing declarations and schema contracts. Remove operation-name gates and Receiving-specific assumptions. Reuse existing representations instead of adding a schema language.

Generate or share standard envelopes, primitive conversions, schema-declared validation, correlation, and standard error mapping. Application authors retain business rules, transaction sequencing, and deliberate result transformations.

Move supported operation families in Receiving, Acme, and WMS onto this implementation. Update each boundary with its consumers and delete replaced adapters. Preserve each operation's writable authority, permission path, transaction behavior, and result contract. Acme's update is a distinct package-owned mutation, not a wrapper around Receiving's update.

Completion requires that an author can declare another supported model or operation and obtain its typed interface and standard adapter without changing generator code or copying protocol handlers. Two working example operations do not meet this criterion. Finish this work in epic 2 before the testing refactor; no later epic owns unfinished generation.

Generalization covers existing supported shapes only. Add no universal runtime controller, arbitrary-SQL facility, streaming framework, or transactional-extension machinery.

| Generate/share | Keep as application code |
| --- | --- |
| SQLx verifier inputs, typed statement accessors and guest adapters. | Business rules, calculations, lock order and command sequencing. |
| Request/result/error types, field validation, boundary codecs and correlation. | Claim handling and transaction scope; reuse the existing runner. |
| Primitive conversion, optionality and permitted error details. | Deliberate transformations between database rows and public results. |

Migration SQL remains schema authority. SQLx verification and guest execution consume the **same SQL corpus** through the existing data-access seam. No new driver, SQL language or extra transaction around generated handlers.

Compile offline using existing `.sqlx` metadata. The local application command refreshes it when SQL, effective schema or verifier inputs change—not for Rust-only edits. Use existing setup and actual verifier targets. CI checks against a fresh schema before refreshing metadata. No second cache or impact-analysis system.

Delete replaced parsers, error mirrors and conversions after callers move—not whole files containing business logic. Coordinate with manifest simplification; add no duplicate declarations.

## 3. Testing: proportionate checks, less machinery

**Test changed behavior, not historical implementation details.** Reuse assertions/fixtures; add tests only for changed behavior or concrete uncovered risks. No duplicate suite per adapter or exhaustive coverage prerequisite.

| When | Minimum validation |
| --- | --- |
| **While editing** | Compile affected native/guest targets and run focused existing unit/property tests. No full workspace or cluster sweep after every move. |
| **Async/SQL boundary lands** | Reuse real guest + PostgreSQL cases covering the changed path: success/refusal, direct/nested authorization, rollback/replay and cancellation/resource cleanup. Run contention cases when transaction/locking behavior is touched. |
| **Generator/client boundary lands** | Check representative exact values, absent/null/value, rejected fields and correlated outcomes against independent expectations. Verify offline build, local SQL refresh and CI stale-metadata refusal. |
| **Process/deployment boundary lands** | Run the relevant startup/shutdown and HTTP/queue cases, plus a packaged smoke test against the changed topology. |
| **Integrated refactor lands** | Run affected suites and one real Receiving/Acme client-to-database journey on the integrated artifacts. Reuse this result; do not repeat it for every subchange. |

Use existing generation fixtures for materially different supported shapes, including a non-Receiving model with different fields and revision naming. Compile affected consumers and reuse focused behavior assertions. Add no exhaustive combination matrix, separate suite per generated operation, or repeated cluster campaign.

When deferred UI work starts, test that failed or denied detail reads preserve posting success without reposting. Resolve failures introduced by the change; report unrelated baseline failures without requiring a repository-wide repair. Skipped, unexecuted or zero-case runs are not passes.

After client interfaces settle, move ordinary screen checks into existing reducers/event handlers and Ratatui `TestBackend`/`Buffer`. Assert requests and displayed state. Delete equivalent PTY cases; retain process tests for password echo, terminal restoration and shutdown. Do not rewrite Python solely for uniformity.

Keep useful properties and saved regressions; delete obsolete-shape snapshots and bookkeeping checks that protect no distinct behavior. Mocks do not replace database/authority tests. Follow test-database isolation in `docs/operations/running-tests.md`.

**No new harness framework, mandatory mutation campaign, coverage quota, benchmark campaign, full DST/replay build-out or Kani.** Reuse CI checks; ordinary test output and a short change summary suffice. No evidence registry or handwritten proof report.

## 4. Consolidate service lifecycles

Use the existing host as the combined HTTP and queue service. Remove duplicate lifecycles as their responsibilities move.
Do not place the two existing service bootstraps beside each other.

### Deployment decision

The owner approved one always-running combined service type. Multiple replicas remain supported.
HTTP and queued work share replica scaling and process failure, but keep separate hard admission limits.
This removes wake-from-zero and its standby resource saving. Guest pool reclamation remains separate from process lifetime.
Retire wake-from-zero. Beads records the approved deployment decision.

### One runtime owner

Keep `services/host/src/main.rs` as the process entry point.
Build the runtime, plugins, credentials, artifact source, `RouterDriver`, and readiness state once in the existing host.
Move queue-specific delivery, claim, and completion functions from `services/executor/src/lib.rs` into the existing execution library.
Pass shared runtime objects and cancellation into that adapter. Remove the executor bootstrap, probe listener, signal handler, and binary.
Retain the host’s native wasmCloud integration and HTTP ingress.
A host without a release remains valid. Enable queue execution only with a loaded release and complete queue credentials.

One shutdown owner stops new HTTP admission and queue claims, drains active work within existing limits, then cleans up resources and telemetry.
Preserve separate credential classes, tenant and release scope, deadlines, transaction cleanup, claim generations, and completion authority.
Update deployment, build, development, delivery, and test consumers in the same increment as each removed lifecycle.
This includes the executor image and deployment inputs in `Dockerfile`, `deploy/platform`, and `crates/control/lib/src/delivery`.

### Due work and authoring

Keep due-work discovery in the existing durable queue loop as it moves into the host.
Preserve bounded discovery, durable concurrent claims across replicas, due times, retries, leases, and recovery.
Remove dispatcher and waker processes, doorbell paths, scaling privileges, secrets, images, and manifests only where their responsibilities disappear.
Retain shared broker and operator resources that other services still use. Add no scheduler, supervisor, registry, or compatibility layer.

Keep local authoring on the existing control-library path.
Remove obsolete local child-process assumptions; retain process helpers only for real remote-boundary tests.
Keep a thin authenticated remote endpoint for retained clients, with the same authorization, audit, and retry rules.
Identity and CDC remain separate. Do not change application contracts, numeric representations, UI behavior, or transactional extensions.
The deferred fractional-quantity error limitation and revision-limit suggestion remain outside this epic.

### Focused validation and closure

Reuse the epic-3 application tests and existing host lifecycle and executor automation cases.
Exercise combined startup, HTTP and queued execution, and shutdown with active work.
Move surviving assertions to their new owner and delete obsolete process/setup assertions together with the replaced code.
Run one integrated packaged Receiving/Acme journey after the topology settles, rather than after each move.
Report removed processes and configuration, executed checks, and remaining limitations for owner review.
Do not set a crate-count target or expand the later extension epic before that review.

## 5. Transactional application extension

This separately scheduled feature follows simplification. Beads epic `wamn-kc2v` owns its implementation and dependencies.
Build one reusable participation mechanism. Receiving and Acme supply the first consumer, not platform-specific hook logic.

### Interface and native linking first

Define one base-owned extension point before commit, its exact application/release binding, and an execution-only transaction view.
The view permits admitted statements within the owner's transaction. It grants no independent commit or rollback operation.
First establish that actual components can pass and use this view through the pinned native linker.
Native resource transfer support alone does not establish support for the application call path.
Use an unpredictable opaque typed token for the view. The host enforces its authority and lifetime because the token is copyable guest data.
The tested borrowed-resource contract failed with `mismatched resource types` on the current pin. This result does not establish a general native-resource limitation.
Keep bounded view state inside `wamn:postgres`, referencing its existing transaction. Add no signing infrastructure, database table, or resource/token compatibility path.
Reuse the existing PostgreSQL transaction implementation and native dispatch. Add no fork, manual dispatch path, or second transaction manager.

### Transaction and authority ownership

One runner owns one transaction and commit decision per command item.
Limit each participant to its own admitted statements and permissions under the original caller, tenant, and compatible database identity.
Sharing a connection grants no additional authority. Match the exact participant operation and invocation, original caller, tenant, release, database identity, and deadline on every use.
Token possession alone grants no access. Participants cannot delegate their view.
Revoke access on return, trap, cancellation, deadline, or transaction completion. Coordinate revocation with active SQL before finalization.
Keep access sequential on the same held connection. A copied token cannot prolong access or transaction lifetime.
Keep deadlines and cancellation cleanup within the existing transaction lifecycle.

### Application policy and completion

Acme evaluates quality control (QC) in ordinary application code.
When inspection is not required, succeed without QC writes. When requirements are satisfied, perform permitted work and succeed. Otherwise, refuse.
Evaluate commit-time conditions under appropriate locks. Preserve existing base invariants and lock order.
The application owns entrypoint exposure. Permitted direct base calls retain base behavior. Optional confirmation remains post-commit.

Required extension failure before commit rolls back the whole item.
Finalize the claim and result only after extension success. Replay identifies the complete extended intent and returns its stored result without repeating writes.
Changed intent refuses. A lost commit response remains uncertain.
Update affected generated contracts, bindings, callers, and replaced conversions together.

### Focused acceptance

Reuse existing local component/PostgreSQL tests for the three QC branches, joint rollback, same-intent replay, and changed-intent refusal.
Cover participant authorization, expired access, cancellation, and relevant contention through those same fixtures.
Run focused compilation and affected cases. Add no cluster campaign or separate suite per generated operation.
Do not add a general hook registry, conditional-rule language, distributed transactions, cross-item transactions, queued continuations, UI redesign, or numeric-contract changes.

## Completion

**The refactor finishes after platform-wide steps 1–4, not the extension feature.** Example applications supply evidence, not a scope limit. General generation must satisfy section 2 before step 3 starts. Target interfaces replace old adapters; tests need less setup; obsolete code and processes disappear. Preserve required offline/distribution artifacts. Broader generated-file and workspace cleanup stays separate unless directly necessary.

Update callers, tests and deletions together; one active refactor per shared area. Update current documentation and summarize changes, deletions, test results and gaps. Investigate suspected performance regressions without making a benchmark or speedup a prerequisite.

**Success is less code and less work to make the next application change—not more generated scaffolding, more gates or another feature program.**

### Source basis

Earlier reviewed source: [`40b7c361`](https://github.com/dkkloimwieder/wamn/tree/40b7c36124a92d7e1d2c2a2e475d1228226879ec); application plan 0.5; supplied testing-simplification plan 0.1; subsequent owner decisions. No current implementation or execution claim.
