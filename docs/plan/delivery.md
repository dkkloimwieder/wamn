# Development and delivery

This proposal shortens local development and separates release automation from deployment.
It does not change application authority or business contracts.
Current commands belong in [operations](../operations/README.md), and current test methods belong in [testing](../testing/strategy.md).

## Local execution without publication

A development session will retain its disposable services and application database between edits.
It will run saved source without an OCI upload, publication endpoint, permanent release record, or handwritten result document.
Local loading and registry retrieval will share the existing validators, component loader, capabilities, and authorization.
The generated application description will identify components, contracts, SQL, and wiring in both paths.

Cargo will continue to own Rust dependencies and targets.
The implementation will use coarse package or stage reuse before adding finer invalidation.
It will not introduce another dependency graph, application model, or replacement for Cargo freshness.
A missing or stale required output must cause regeneration or refusal before execution.

The planned repeat work depends on the changed input:

| Change | Work in the development session |
|---|---|
| Rust implementation | Rebuild affected targets and load their changed artifacts after import and contract checks. |
| Named SQL | Regenerate and prepare the affected SQL, then exercise the affected command or query. |
| Contract, route, or wiring | Regenerate the affected package and consumers. Package-wide invalidation is sufficient. |
| Schema or migration input | Recreate only the owned disposable application database, then regenerate and prepare SQL. |
| Permissions or bindings | Apply the target configuration, inspect its authority, and retire obsolete resources. |

Code-only changes will reuse unchanged schema, generated contracts, and SQLx metadata.
SQL changes will reuse the schema environment when its inputs remain compatible.
Schema changes will reset the owned disposable target.
This proposal adds no support for changing an installed database with retained data.

The replacement artifact must finish building before it replaces the serving candidate.
The previous candidate remains usable while its target remains intact.
An invalidated target makes the session unavailable until replacement succeeds.
Restarting the process is sufficient.
Replacement must bound completion or cancellation and must never silently repeat interrupted mutations.
A database reset invalidates client records, revisions, cursors, drafts, and pending submissions.

The session needs explicit reset and clean-check operations over its own resources.
The [test database isolation rules](../operations/running-tests.md#test-database-isolation) apply to setup, execution, and cleanup.

SQLx metadata supports offline compilation of the selected SQL corpus.
The workflow will refresh affected metadata when SQL or schema inputs change.
Release qualification will use the pinned SQLx CLI and actual verifier targets with `cargo sqlx prepare --check`.
It will not compile every possible feature combination.
Metadata does not establish permissions, locking, or rollback behavior.
Reuse must include the actual inputs to each check, including changed grants or checker behavior where relevant.

## Release and deployment automation

The selected CI service will invoke the same repository commands that an authorized operator can run directly.
Source hosting, CI execution, artifact storage, and deployment targets remain independent choices.
Provider configuration will supply triggers, credentials, job order, and retention.
It will contain no second implementation of WAMN validation or new CI abstraction layer.

Change checks precede integration.
Qualification and publication use a selected integrated revision, initially from `main`.
A release tag or explicit invocation can also select a candidate.
A successful review build does not qualify different integrated bytes.

| Boundary | Proposed automation |
|---|---|
| Integration | Run the relevant existing tests for changed behavior, including deployed checks when the changed boundary requires them. |
| Qualification | Use pinned tools and locked dependencies, reconstruct clean schema state, and exercise the selected SQL, contracts, and deployment artifacts. |
| Publication | Publish only a candidate whose required checks execute and pass. |
| Deployment | Fetch the selected release's exact artifacts, establish configuration, and exercise readiness plus an authenticated interaction. |

The deployment identity remains the scoped `effective_release_id` and immutable manifest digest.
The manifest identifies the exact artifacts to fetch.
Deployment must not rebuild them.
Qualification records must name that same release.

Requalification does not change a release identity.
A build with different contents needs a distinct identity through the existing release path.
One source commit can therefore produce different releases.
A source revision, CI run identifier, or completion timestamp does not establish deployment precedence.

Deployments must serialize within each environment.
Before activation, the requested release must still be the environment's selected deployment.
A late task cannot activate a superseded selection.
This uses the existing release and activation order, without another ordering service or approval process.

Dependency and build caches remain reusable.
A cached local pass does not replace the selected revision's required qualification.
Uploading an inactive artifact for deployment tests does not qualify it or activate a shared environment.

Automation will record source revision, executed commands, release identity, manifest digest, and deployed release.
It will retain relevant test logs, failure inputs, and referenced artifacts.
The runtime does not require a CI run identifier.
Publication credentials must stay out of artifacts, logs, and jobs that execute untrusted changes.
A content digest identifies bytes but grants no deployment authority.

Code replacement can reuse a compatible database.
A schema-changing deployment must provision a new target or refuse an unsupported existing-data upgrade.
It must not silently reset a database or imply that code rollback reverses database changes.
The separate [upgrade design](upgrades.md) remains deferred.

## Acceptance of this workflow

A saved code edit must run without registry access, publication, database recreation, or repeated online preparation of unchanged SQL.
Invalid SQL, incompatible contracts, forbidden imports, and missing required output must still refuse before use.
The empty-cache and incremental paths must agree on deterministic generated output and validation results.
This includes removed operations and corrupt cached output.

A schema change must affect only the owned disposable target.
The test runner must enforce the linked database isolation rules before execution.
Required checks must reject actual failures, unavailable prerequisites, and zero expected cases.
Compilation, ignored cases, and explicit skips do not establish executed success.

Qualification, publication, and deployment must agree on release identity, manifest digest, and pinned artifacts.
Two builds must not overwrite one release with different contents.
A superseded deployment must not activate after finishing late.
The same commands must also run without a source-host or CI-provider API.

The relevant real application command and packaged application cases must execute.
Existing [application methods](../testing/application-tests.md) remain the basis for these checks.
The workflow needs before-and-after timings for code, SQL, and schema edits.
It needs no fixed latency threshold or synthetic 200-operation application.

## Conditional extensions

Finer caches, database templates, remote caches, and precise test selection require a measured problem after coarse reuse.
Broader deterministic testing requires a named state or failure guarantee that existing tests cannot cover economically.
The open `wamn-54b0` work retains three scoped possibilities:

- Supply time explicitly to time-dependent run-state SQL, identified as D2b in the earlier design.
- Control event scheduling over real run-state SQL, identified as D2.
- Record synthetic guest effects and fix guest clocks or randomness for a named test, identified as D3–D5.

These labels are design references, not newly created tasks.
The current [deterministic test limits](../testing/deterministic.md#replay-limits) continue to apply.
No production effect capture or full-platform simulator is required.

Formal verification, Bolero, broad fuzzing, and automated mutation campaigns remain deferred.
Advanced attestations, GitOps controllers, canaries, and automatic rollback need an actual deployment or supply-chain requirement.
The proposal requires no coverage percentage, multi-environment rollout program, or repeated performance campaign.
