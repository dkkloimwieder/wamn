# Application tests

An application owns its fixtures, expected results, business assertions, and native test crate.
Selected public commands need tests through the real guest, capability, transaction, and authenticated route boundaries.
Private operations use their actual caller or ingress.
See [database tests](database-tests.md) for state and authority observations.

The [owned delivery tests](../operations/delivery.md#owned-application-acceptance) invoke the same application cases with supplied release artifacts.
Those cases compare the freshly minted manifest with the exact candidate before publication.
They report success only after application assertions and owned resource cleanup pass.
Receiving retains baseline overlay compatibility, while WMS retains composed label delivery and partial-completion cases.

The [local saved-edit case](../operations/running-tests.md#local-saved-edit-acceptance) runs the existing Receiving developer command against owned services.
It keeps an authenticated mutation through code and SQL edits, and it observes the changed application response.
Invalid SQL must refuse while the previous candidate remains usable.
A schema edit must create a new disposable database with the edited schema and no retained business rows.
The case also requires unchanged system database grants, restored source, and successful cleanup.
Client tests separately establish the state reset after target replacement.

For requested build measurements, complete correctness acceptance first.
Use separate clean worktrees and separate Cargo targets for the starting and changed revisions.
Keep the selected application, package commands, profiles, dependency cache, and build concurrency equivalent.
Measure a full build from an empty target before measuring saved edits with that target populated.
Record application compilation separately from required helper compilation and from the time between saving and serving an edit.
Require the expected authenticated application result and successful cleanup before accepting an edit measurement.
Record the source, tools, machine limits, commands, and sample count without a fixed latency threshold or permanent report requirement.

## Receiving commands

The [Receiving history tests](../../apps/wamn_receiving/tests/receiving_command_histories_live.rs) exercise `receiving.record_receipt` and `purchase_order.update`.
The [local test](../../apps/wamn_receiving/tests/route_authentication_live/local_business.rs) supplies real components, authenticated HTTP, and disposable PostgreSQL without cluster or image setup.
See the [focused commands](../operations/running-tests.md#local-application-business-tests).
Their required observations are:

- Accepted receipts accumulate without exceeding the order, with matching status, revisions, and committed receipt history.
- Exact remaining quantity succeeds, while excess quantity and invalid status refuse without partial business writes.
- The same key and body recover the original receipt, while changed bodies refuse, including during overlapping same-key calls.
- Competing receipts whose sum exceeds the remaining quantity cannot both commit.
- Failure after an observed intermediate write rolls back all receipt business writes.
- An independently confirmed commit with suppressed response permits captured retry without posting a second receipt.
- Stale revisions and denied callers refuse without an unauthorized business mutation.

Use a [deliberate business defect](mutation.md) to establish that the intended assertion detects it.
Contention controls must retain other constraints that still protect the business rule.

## WMS operations

The [local WMS test](../../apps/wamn_wms/tests/local_business.rs) runs the existing operation and replay assertions with real components, authenticated HTTP, and disposable PostgreSQL.
It retains contention, conflict, exact replay, adjust, split, merge, and aggregate assertions.
Its initial revision exceeds JavaScript’s safe-integer limit, so inputs, results, and conflict details must preserve exact decimal strings.
The deployed case retains label delivery and committed results after a label failure.

## Fresh overlay compatibility

The [paired installation test](../../apps/wamn_receiving/tests/route_authentication_live/cluster/postcommit_pair.rs) compares base A plus overlay O with base B plus the same overlay artifacts.
Each combination is an independent fresh installation.
The [Acme test owner](../../apps/client_acme_receiving/tests/overlay_compatibility.rs) observes consumed schema requirements and exercises the breaking candidate.

An additive base must satisfy the unchanged overlay's requirements and named operations.
A breaking combination must refuse for its named schema requirement or ownership conflict.
An unrelated build failure does not establish compatibility refusal.
Keep overlay artifact digests unchanged throughout the comparison.

This tests fresh-install compatibility, not an upgrade of an existing database.
Do not apply a migration suffix to an installed predecessor or activate an upgrade candidate.
The installed-schema observer does not claim production schema-admission enforcement.

## Committed events

The [post-commit test](../../apps/wamn_receiving/tests/postcommit.rs) uses the actual broker, materializer, and private `quality.create_inspection` handler.
After the first completion, redelivery must reach the handler again and leave one inspection without resetting its business state.
A distinct valid receipt creates its own inspection.
Publishing twice is insufficient when broker deduplication prevents the second handler delivery.

Deliver a routable poison event followed by an independent valid event.
Observe the correlated native broker termination advisory and the later inspection within the declared retry and time bounds.
Inspect the actual source, registration, deliveries, business state, and resource restoration.
The native failure record is a broker advisory, not a fabricated application dead-letter row.
Payload recovery is limited by source retention.
These outcomes require stated recovery assumptions and do not promise immediate progress or platform-wide exactly-once effects.

## Record history

The Receiving tests observe stamps and the log through real routes and commands:

- The [route tests](../../apps/wamn_receiving/tests/route_authentication_live/routes.rs) refuse a supplied stamp at its JSON pointer, and a service principal stamps its own id.
- In the same tests, `receiving.record_receipt` stamps the order and its receipt with one instant, and an Acme overlay update stamps the base columns.
- The route tests also fold the purchase order history that the server served. No served image carries an Acme column, and the fold matches the purchase order that the database holds.
- In the same tests, a route caller who holds every grant except the history token gets `permission-denied` before the component runs.
- The [session tests](../../apps/wamn_receiving/tests/route_authentication_live/sessions.rs) show that one person stamps the same id through a session token and through a PAT.
- The [materializer test](../../apps/wamn_receiving/tests/route_authentication_live/materializer.rs) shows that `quality.create_inspection` stamps `wamn:materializer` as `created_by`.
- The materializer test also makes sure that CDC publishes no history table row. The purchase order entry and the line entry of the receipt carry the event `txid` in the low 32 bits of their transaction id.
- The [data access test](../../apps/wamn_receiving/tests/receiving_data_access.rs) installs the declared triggers with its own SQL and tests insert, update, and no-op stamps.
- In the same test, an Acme overlay update logs a changed-column diff that folds to the effective base and overlay row. The Receiving history read returns only its base columns. An idempotent receipt replay appends no entry.
- The [history panel tests](../../apps/wamn_receiving/ui/tests/history.rs) page a history read and fold its rows. They show a row at each entry, an unavailable row, a new read after the head position moves, and rows that do not fold.

`wamn dev up` with a logging package remains unexecuted.
The [dev command test](../../services/ctl/tests/dev_command.rs) runs it with `apps/wamn_receiving`, but its refusal comes before standup.

See [database tests](database-tests.md#record-history) for the fixture rules and limits.

## Operator outcomes

Use the [projection tests](../../crates/schema/generator/tests/client_projection.rs) for declared bindings and screen coverage.
The [request tests](../../crates/client/core/tests/request.rs) and [draft tests](../../crates/client/tui/tests/draft.rs) inspect typed inputs and outgoing bytes.
The [submission tests](../../crates/client/tui/tests/submission.rs) and [screen tests](../../crates/client/tui/tests/screen.rs) own shared outcome behavior.

The [Receiving UI tests](../../apps/wamn_receiving/ui/tests/) drive production events, requests, reducers, and rendered state in-process.
They cover Receiving and Acme posting, separate optional reads, QC refusal, and route-specific recovery.
The existing local component/PostgreSQL journey sends one Acme receipt through the real operator client.
It reuses the backend fixture and its QC, rollback, and replay assertions.
Process tests retain terminal restoration, signals, and password masking.

Retain these observations through generated and application-owned operator code:

- Identical generation inputs produce byte-identical output.
- Each callable operation has a screen, a not-exposed entry, or a requires-composition entry, and each declared error case has a rendering.
- Missing compatible record-read or revision bindings block submission, and an unbound required read input blocks invocation.
- Exercise all four combinations of required/omittable and nullable/non-null fields, including `purchase_order.update.supplier_id`.
- Inspect actual outgoing bytes for typed canonicalization and nested row bounds, plus opaque cursors that reset on filter or sort changes.
- Unsupported input types block submission rather than becoming unrestricted text fields.
- Pending submissions block a second submission, and allowed retries preserve the captured command body exactly while rebuilding authorization headers.
- A direct claim-backed route permits captured retry, while a composed route offers refresh with a warning about repeated effects.
- Compare first-attempt authorization refusal with retry refusal after uncertainty, which must retain the captured intent and its uncertainty.
- Confirmed partial completion shows the committed result and failure, spends the submission, and prevents whole-command resubmission.
- Unknown errors, malformed responses, and transport failures do not become editable refusals.
- Target replacement resets data-dependent state and blocks submissions even when generated contracts are unchanged.
- An intact previous target remains usable after failed replacement, while an invalidated target remains unavailable until matching activation succeeds.
- Old-session responses cannot enter a new session, and restarting a pending submission neither resubmits it nor claims server cancellation.
- Receiving composition retains projection, line editing, location selection, and submission interaction parity before replacing hand-written screens.
- WMS renders the label key from the served response contract.
- WMS displays a committed movement with label failure only from served completion evidence, and otherwise displays an unknown outcome.

State or revision calls do not promise the original result on replay.
Do not add automatic mutation retries or infer route replay safety from operation names.
