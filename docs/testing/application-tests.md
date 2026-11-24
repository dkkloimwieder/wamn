# Application tests

An application owns its fixtures, expected results, business assertions, and native test crate.
Selected public commands need tests through the real guest, host service, transaction, and authenticated route boundaries.
Private operations use their actual caller or ingress.
See [database tests](database-tests.md) for state and authority observations.

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

## Fresh overlay compatibility

An additive base and a breaking combination need a second base that no candidate carries, so they are not cluster cases.

This tests fresh-install compatibility, not an upgrade of an existing database.
The installed-schema observer does not assert production schema-admission enforcement.

## Committed events

Deliver a routable poison event that blocks its handler, followed by an independent valid event.
The materializer acknowledges each event after it admits the run, so the poison retries happen in the workflow queue, not in the broker.
Observe the blocked handler and the later inspection within the time bound.
Inspect the actual source, registration, consumer drainage, business state, and resource restoration.
Payload recovery is limited by source retention.
These outcomes require stated recovery assumptions and do not promise immediate progress or platform-wide exactly-once effects.

## Record history

The Receiving tests observe stamps and the log through real routes and commands:

- The [route tests](../../apps/wamn_receiving/tests/route_authentication_live/routes.rs) refuse a supplied stamp at its JSON pointer, and a service principal stamps its own id.
- In the same tests, `receiving.record_receipt` stamps the order and its receipt with one instant, and an Acme overlay update stamps the base columns.
- The route tests also fold the purchase order history that the server served. No served image carries an Acme column, and the fold matches the purchase order that the database holds.
- In the same tests, an operator who holds every grant except the history token gets `permission-denied` before the component runs.
- The [data access test](../../tests/receiving-tests/receiving_data_access.rs) installs the declared triggers with its own SQL and tests insert, update, and no-op stamps.
- In the same test, an Acme overlay update logs a changed-column diff that folds to the effective base and overlay row. The Receiving history read returns only its base columns. An idempotent receipt replay appends no entry.

See [database tests](database-tests.md#record-history) for the fixture rules and limits.

## Operator outcomes

The [web runtime tests](../../web/runtime/test/classification.test.ts) test response outcomes.

The local component and PostgreSQL journey sends Acme receipts through the published route.
It uses the backend fixture and its QC, rollback, and replay assertions.

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
- Old-session responses cannot enter a new session, and restarting a pending submission neither resubmits it nor reports server cancellation.
- Receiving composition retains projection, line editing, location selection, and submission interaction parity.
- WMS renders the label key from the served response contract.
- WMS displays a committed movement with label failure only from served completion evidence, and otherwise displays an unknown outcome.

State or revision calls do not promise the original result on replay.
Do not add automatic mutation retries or infer route replay safety from operation names.
