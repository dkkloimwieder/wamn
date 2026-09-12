# Application tests

An application owns its fixtures, expected results, business assertions, and native test crate.
Selected public commands need tests through the real guest, capability, transaction, and authenticated route boundaries.
Private operations use their actual caller or ingress.
See [database tests](database-tests.md) for state and authority observations.

## Receiving commands

The [Receiving history tests](../../apps/wamn_receiving/tests/receiving_command_histories_live.rs) exercise `receiving.record_receipt` and `purchase_order.update`.
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

## Operator outcomes

Use the [projection tests](../../crates/schema/generator/tests/client_projection.rs) for declared bindings and screen coverage.
The [request tests](../../crates/client/core/tests/request.rs) and [draft tests](../../crates/client/tui/tests/draft.rs) inspect typed inputs and outgoing bytes.
The [submission tests](../../crates/client/tui/tests/submission.rs) and [screen tests](../../crates/client/tui/tests/screen.rs) own shared outcome behavior.

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
