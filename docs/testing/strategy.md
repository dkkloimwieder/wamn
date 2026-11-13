# Testing strategy

Test each contract at the lowest real interface that can establish it.
Use the smallest existing test that reaches the changed behavior.
Compile tests, generated examples, database tests, and deployed tests establish different facts.
An interface can be a product Rust function, database executor, component, HTTP route, or CLI; interface testing does not mean running every test through kind.

The expected result comes from the accepted behavior, not the implementation under test.
Name only the boundary exercised: compilation is not execution, a renderer is not an operator, and message delivery is not workflow admission.

Repository lints test dependency graphs and compiler or tool results, never hand-written module rosters or source markers.
Test a requirement through behavior at its owning interface, or record it as untested.

[CLAUDE.md](../../CLAUDE.md) owns the five test rules and integration selection.
[Mutation tests](mutation.md) owns deliberate-defect evaluation; [test results](evidence.md) owns reporting.
[Running tests](../operations/running-tests.md) owns commands, prerequisites, timing, and cleanup.
The tables below select coverage, not permission to run it: kind/cluster execution, including a diagnostic experiment, needs explicit user approval with its purpose and expected duration.
Complete a seconds-long prerequisite test before any cluster step; stop a stage after 30 minutes of silence, not after 30 minutes of total runtime.
The cluster approval restriction and testing budget are recorded in `docs/audit/fixtures.md` and `docs/audit/tests.md`; narrower current user instructions still apply.

## Ownership

Applications own fixtures, commands, expected results, business rules, and workflows.
Platform test support owns invocation, controlled dependencies, environment setup, and failure reporting.
Extend those owners.

An invariant is a rule at a stated observation boundary.
Record its identifier, rule, affected state, observation boundary, enforcing code or constraint, and named tests.
Keep these facts in the application requirements and ordinary test code.

Choose the observation that matches the rule:

- State assertions inspect coherent committed state, such as quantities within their limits.
- History assertions inspect permitted transitions and the original result of a replayed claim.
- Eventual assertions require a result within stated time bounds and recovery assumptions.

A confirmed refusal before commit leaves the named business state unchanged.
List permitted claim or audit changes separately.
Unknown completion is not a refusal.
For critical invariants, test success, refusal or failure, and [detection of a deliberate defect](mutation.md).

## Choose the boundary

| WAMN behavior | Smallest useful boundary | What deployed verification adds |
|---|---|---|
| Mode parsing, selection changes, physical fingerprints | Product parser/planner | Correct configuration passed to execution |
| Supported migration SQL and package ownership | Actual validator and publication interface | Published bytes reach the selected SQL Job |
| Migration effects, retries, adoption and checksums | Actual executor with real PostgreSQL | Image, credentials, network and Job invocation |
| Permissions and runtime grants | Actual authorized operation with its database | Certificate and credential delivery through deployment |
| Event admission and release pinning | Admission operation with PostgreSQL/NATS as required | Host selection and lifecycle wiring |
| Component composition and WIT behavior | Real component composition and invocation | Registry access and wasmCloud placement |
| Resource definitions and ownership | Renderer with semantic output assertions | Kubernetes/operator acceptance and readiness |
| Independent application deployment | Product commands on kind, resources and HTTP responses | Actual Helm and wasmCloud interaction |
| Same-name database reset | CNPG lifecycle with deployed database users | Deletion/recreation, reconnection and event isolation |

A larger test adds an integration observation; it does not repeat the smaller test's whole matrix.
For example, PostgreSQL establishes migration retry behavior, while one SQL-Job case establishes that the container executes the selected artifact with the correct credentials.

## Concrete test designs

### Application data must be connected to the operation

**Bad:** create a separate database, apply a chart containing only a ConfigMap, and make sure that the database row survived.
**Good:** create records in alpha and beta through their application routes, deploy alpha, read the records back, and test the specified beta deployment state remains unchanged.
A Helm-only test tests Helm reapply/recovery and makes no application-data assertion.

Project meaningful state rather than comparing all database rows or Kubernetes metadata.
For code-only deployment, test application data and scheman identity; permit the targeted control writes accepted in `docs/plan/deploy.md` under Deployment ownership.
Release-cache additions, application bindings and targeted grants are not forbidden application-data mutations.
Make sure of beta's effective permissions as well as its release where permission isolation is the requirement.

### Migration retry must expose repeated effects

**Bad:** apply `CREATE TABLE IF NOT EXISTS` twice and infer that SQL ran only once.
**Good:** initialize a package-owned value to 7, then execute this supported migration through the actual package executor:

```sql
UPDATE demo_data.progress SET value = value + 1 WHERE key = 'sample';
```

After apply, read 8 and the matching applied migration record; apply the same bytes again and read 8 again.
The sequence 7 → 8 → 8 distinguishes zero, one and repeated effects.
Use an execution-time PostgreSQL failure in an ordinary migration to make sure of rollback of both effects and its completion record; a validator refusal cannot establish transaction rollback.

### Nontransactional outcomes remain explicit

`docs/plan/deploy.md`, SQL execution, specifies one standalone statement in a file beginning with `-- wamn:no-transaction`, with committed `started` then `applied` results.
The resolution and completion owner is `crates/control/package/src/apply_package/migration.rs` (`MigrationResolution`, `resolution_package`, `complete`).
Adoption is distinct from execution; do not treat an adopted record as evidence that SQL ran.

Test at the PostgreSQL executor boundary:

| Case | Required observation |
|---|---|
| Success | Actual schema effect and matching applied package/path/checksum |
| Uncertain completion | A remaining started record prevents silent replay |
| Explicit applied resolution | Completion is accepted without executing the statement again |
| Explicit retry | The recorded bytes run only after the user requests retry; changed bytes are refused |
| Competing execution/resolution | Resolution cannot race the worker still executing the statement |

Use `CREATE INDEX CONCURRENTLY` to reach the actual nontransactional path and inspect index validity.
An error alone does not establish absence of effects; the test must not automatically repair the index before observing the outcome.

### Published execution must not depend on source

**Bad:** make sure that a package-copy helper wrote a directory and assert deployed SQL uses published bytes.
**Good:** publish through the product owner, retain the immutable digest, make the source unavailable, apply the selected release, and inspect its database effect and recorded artifact identity.
`crates/control/environment/tests/application_activation_live.rs` contains this test shape.

Use a real registry when the publication/pull behavior is the subject.
An executor-only test may supply verified artifacts directly but must not assert registry or deployed Job coverage.

### Event admission must inspect the created run

**Bad:** count NATS messages or inspect a passed argument and assert the run selected the correct release.
**Good:** admit events for alpha and beta through the actual admission interface and inspect each run's application and manifest digest.
Make a different release available without selecting it and make sure that the admitting host still uses its loaded release.
Redelivery must preserve an existing run's identity and pinned release under the accepted duplicate-prevention rule.

During rollout, old and new hosts can coexist; do not assume an instantaneous global switch unless the product specifies it.
For deployed reset isolation, the assertion is that pre-reset events cannot create post-reset runs.
The deployed case must use the real workflow-authoring/registration path; direct runtime-row insertion cannot stand in for an unavailable product interface.

### Concurrency must reach the conflicting operation

**Bad:** sleep, observe two tasks started, and infer serialization.
**Good:** hold the actual product-owned lock or transaction with one client, start the conflicting mutation with another, observe that mutation cannot complete, release the first, and inspect the permitted result.
Use explicit synchronization and bounded condition waits; readiness polling must not become automatic mutation retry.
Reset exclusion and concurrent independent application deployment are separate properties and need different observations.

## Independent deployed cases

Reuse the entry points in `crates/control/dev/tests/platform_apply_kind.rs`.
These are intended coverage responsibilities, not an assertion that each current function already has minimal setup.
Any execution remains subject to the approval and preflight rules above.

| Case | Minimal starting state | Distinct observation |
|---|---|---|
| `platform_install_recovery_kind` | Prepared artifacts, empty platform target | Product installation, reapply and platform recovery |
| `application_deploy_kind` | Existing disposable platform, fresh test environment, two application artifacts | Independent updates, rollback, actual routes and retained data |
| `sql_job_kind` | Existing platform, initialized application, published migration | Actual image/credentials execute selected SQL without changing code selection |
| `same_name_reset_kind` | Existing platform, shared test environment with two applications and data | CNPG reset, fresh initialization identity, restored access and reconnection |
| Event isolation | Supported authoring path and event infrastructure | Admission uses each application's loaded release and respects reset |

Each case prepares its own minimal state through existing product owners.
A SQL-Job case must not depend on previous quota exhaustion, application rollback or two destructive resets.
Reset needs installed applications and data, not a prior migration test's leftover source versions.
Share command capture and artifact location helpers, not a mandatory history of earlier scenarios.

An attached-cluster test cleans up only its owned environment, source edits and temporary resources; it does not delete the supplied platform.
Use isolated targets for cluster-wide fault injection that could affect another test.
Retain one fresh-platform integration path separately from focused diagnosis.
One source-publication case can establish source wiring; other cases can consume immutable artifacts prepared by the existing producer.

## Rebuild only the affected subject

This table identifies preparation for approved verification, not a request to start a cluster run.
Use `docs/operations/running-tests.md` for actual commands and integration requirements.

| Changed input | First useful test | Artifact implication |
|---|---|---|
| Test assertion/reporting | Affected test target | No unchanged product-image rebuild |
| Migration policy/planning | Validator/planner/publication tests | Changed native targets; deployed executor image only for its integration test |
| SQL execution or grants | Real PostgreSQL | The executable or Job image must include the fix when tested deployed |
| Job arguments, credentials or registry inputs | Input/render tests; approved SQL-Job case | Affected chart/image inputs only |
| Helm rendering or host configuration | Semantic renderer; approved relevant kind case | Selected chart and changed host artifact |
| CNPG reset sequencing | Relevant database tests; approved reset case | Actual reset CLI and Job inputs |
| Business operation | Application/runtime tests | Application and affected composed dependents, not unrelated services |

Retain compatible build outputs and immutable artifacts.
A test-only change does not invalidate unchanged image inputs; an executor fix does.
Use existing command timings to separate preparation, compilation, publication, deployment waits and assertion execution.
A split into named cases establishes no speedup until an independently run case avoids the unrelated work.
For defect detection, use `mutation.md` at the owning interface. Source inspection alone does not establish refactor tolerance or faster execution.

## Diagnose and decide

| Cause | Evidence and response |
|---|---|
| Product regression | A retained behavior fails through valid inputs; fix its owner and the focused detector |
| Test defect | Wrong setup, response shape or unrelated assertion prevents the intended observation; correct that side |
| Obsolete behavior | An accepted decision removed the contract; delete the assertion and exclusive support |
| Missing prerequisite | Tool, artifact, credential, trust or server input is invalid; repair preparation and report non-execution |
| Unresolved requirement | The plan does not define the expected result; resolve it before an expensive run |
| Pre-existing defect | Reproduce a matching failure at the base with comparable inputs; retain its issue and evidence |
| Suspected nondeterminism | Equivalent inputs sometimes fail; investigate scheduling, resources and cleanup rather than retrying to green |
| Missing product interface | Keep required coverage explicitly incomplete; do not implement the missing product operation inside the test |

Group failures by cause, not target count.
A TLS failure before permission evaluation establishes nothing about the permission test and does not by itself distinguish product wiring from test setup.
Compilation-blocked tests can be reviewed for relevance but remain unexecuted.

| Disposition | When | Required change |
|---|---|---|
| Keep | Suitable detector for retained behavior | Preserve its accurate target |
| Strengthen | Correct path, weak observation | Add a result that distinguishes the real defect |
| Rewrite | Retained rule, implementation-spelling assertion | Observe behavior; demonstrate that harmless refactoring passes and the relevant defect fails |
| Move lower | A cheaper interface establishes the same contract | Demonstrate detection at the cheaper interface; remove the duplicated expensive matrix and exclusive preparation |
| Split | Unrelated scenarios share mutable history | Run the selected case alone from its own minimal state, without earlier scenarios or their leftovers |
| Consolidate | Same contract and boundary repeated | Retain detection of the relevant defect; remove the weaker assertion and exclusive support |
| Delete | Removed contract, redundant assertion or test-only machinery | Remove unused registrations, builds and support; identify any remaining coverage gap |
| Defer explicitly | A required interface is unavailable | Track in Beads and report as unexecuted |

Do not keep obsolete test installers merely because old tests call them.
Do not remove a dependency still needed by other consumers.
A deletion is not complete coverage simply because the remaining suite passes.

## Enforce and evaluate changes

Use ordinary test review, existing run approval and the owning Bead; report results through `evidence.md`, without an additional format.

**At review:** identify the retained behavior, realistic defect, lowest useful interface and any existing detector or preparation made redundant.
Inspect the actual assertions and setup, not the test's name.
Reject unnecessary cluster dependencies, mandatory preceding scenarios, source-spelling assertions and duplicated expensive matrices.
Preserve meaningful data and authorization tests; delete obsolete expectations rather than restoring removed product machinery.

- For changes to input → producer → consumer or setup → operation → cleanup paths, review the complete path.
- Before a long scenario, exercise the smallest affected boundary. Build and publish authored fixtures through the product. Run external commands against the pinned executable. Exercise lifecycle operations through minimal creation and deletion. Exercise runner changes with short real processes that establish the working directory, exit status and cancellation.
- Compilation alone does not complete setup or cleanup work. Record demonstrated execution in the existing issue.
- Replace misleading mocks or assertions about source structure instead of adding duplicate coverage.

**Before cluster execution:** an approval request identifies the additional integration observation that cheaper tests cannot establish and the focused results already available.
A failed cluster run alone does not justify another full replay.
Diagnose the cause first, then choose the smallest relevant reproduction; genuine CNPG or Helm failures may need an approved focused kind case.
Existing integration-selection requirements still apply.

**For runner or selection changes:** verify mechanical behavior through existing outputs and tests: the requested case exists and executes, focused selection prepares only its required inputs, and prerequisite tests precede cluster work.
Inspect the selected commands and executed work rather than inferring selectivity from a flag.
Whether assertions detect the same behavior remains a review judgment, not a source-pattern rule.

Evaluate representative changes when their test or preparation owner is being changed:

| Exercise | Evidence required before reporting improvement |
|---|---|
| Migration-policy correction | The validator/publication test detects the violation without platform builds or cluster setup |
| Migration retry correction | Real PostgreSQL shows 7 → 8 → 8; deliberately repeating a committed migration fails the relevant assertion |
| Focused SQL-Job case | The selected case runs alone on its supplied disposable platform, using its own environment; no platform reinstall, quota recovery, prior rollback or reset sequence |
| Harmless implementation refactor | A local rename or helper extraction preserves the test result; the relevant behavioral defect still fails |
| Test-only assertion change | The affected test is rebuilt as necessary; unchanged product-image inputs do not trigger image rebuilds |
| Test deletion or consolidation | The retained detector still catches the relevant defect; unused registrations and exclusive setup are removed, with any remaining coverage gap explicit |

Use `mutation.md` for representative defect tests, not a mandatory experiment for every minor edit.
Cluster-based evaluation still requires approval; source review alone cannot establish runtime savings.
Compare comparable inputs and cache conditions, separating cold setup from warm execution.
Use existing timings to identify work removed and time saved; set time expectations from measurements rather than an arbitrary universal duration.
Fewer tests, deleted lines and a passing full run are not substitutes for these observations.

**Completion criterion:** a testing improvement is complete only when its reported reduction in coupling or execution work is demonstrated at the affected boundary, redundant assertions and exclusive preparation are removed, and retained behavior remains covered or explicitly untested.
New names and a passing combined run do not establish that improvement.
Record the result in the existing issue; unresolved tests remain incomplete rather than being closed on intent.

## Target and results

Use real application and platform functions through existing Rust test owners.
Generated claim tests complement full-command execution and do not replace it.
Required tests must execute their named cases and pass.
An absent case, ignored case, or explicit skip cannot establish success.
Each guard must name the runner that executes it.
A required runner must refuse a missing, renamed, or unbuilt subject.
A plan test establishes command selection, not the results of those commands.
Use `evidence.md` for execution reports and Beads for findings.

**Final decision rule:** Keep a test when it detects a realistic retained defect at the appropriate interface and cost; change or remove it when another existing test establishes the same contract more directly, without hiding any required untested behavior.
