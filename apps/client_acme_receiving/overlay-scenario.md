# Acme Receiving overlay

Acme adds inspection behavior to the Receiving base without copying its source.
The [manifest](wamn.json) names an exact `wamn_receiving` dependency through `base_receiving`.
The base and overlay keep separate package versions, migration histories, artifacts, and permissions.
Shared definition ownership belongs in [data access](../../docs/architecture/data-access.md).

## Client-owned state

Acme adds `acme_inspection_required` and `acme_quality_status` to the extensible `receiving.purchase_order` relation.
Both columns and `purchase_order_acme_quality_status_check` remain client-owned.
The quality status is non-null text with exactly `not_required`, `pending`, and `approved`.
A reject operation and `rejected` state are absent.

Acme's purchase-order update exposes only those two client fields.
It cannot update the base supplier, status, or received quantities.
It cannot create or delete base purchase-order rows.
Its [first migration](migrations/0001_add_inspection_required.sql) and manifest carry that declared surface.

The [inspection migration](migrations/0002_quality_inspection.sql) creates `quality_inspection`.
Its primary key and receipt foreign key use `receipt_id`.
There is no second inspection identifier.
Inspection status is `pending` or `approved`.

## Public operations

The [attachments](publication/attachments.json) expose five authenticated POST routes:

| Path | Operation |
|---|---|
| `/acme/purchase_order/get` | `purchase_order.get` |
| `/acme/purchase_order/update` | `purchase_order.update` |
| `/acme/receiving/record_receipt` | `receiving.record_receipt` |
| `/acme/quality/load_purchase_order_detail` | `quality.load_purchase_order_detail` |
| `/acme/quality/approve_inspection` | `quality.approve_inspection` |

The Acme receipt operation reads client inspection state before calling the declared base operation.
The nested call retains the same caller and enforces the callee permission.
The effective release resolves `base_receiving` to one exact implementation.
The overlay's compiled requirement remains separate from that selected implementation.

This composition supports screen preparation and checks before the base call.
It does not guarantee an inspection condition at the base transaction's commit point.
Atomic client extensions need an explicit transaction contract that the base does not currently provide.

The detail projection combines declared base and client fields for the screen.
Approval identifies the inspection by `receipt_id` and advances the purchase-order revision.
Its result includes `purchase_order_row_version` for a later edit.
The [operation owner](data/src/operation.rs) implements these commands and their refusals.

## Committed receipt events

The private `quality.create_inspection` handler responds to committed Receiving receipt events.
Its registration names the exact source package and entity.
It has no public route or permission token.
The registration grants delivery authority without fabricating a caller identity.
The event retains its causation identity.

Repeated delivery for the same receipt must leave one inspection.
A distinct receipt must receive its own inspection.
The existing [postcommit assertions](../wamn_receiving/tests/postcommit.rs) exercise both requirements through the registered consumer.
They also require bounded failed attempts, a correlated native advisory, and progress for a later valid event.
Payload recovery remains limited by broker retention.

ERP synchronization remains deferred because this application declares no ERP consumer.
The event path adds no generic outbox or alternate delivery system.
The [event test methods](../../docs/testing/application-tests.md#committed-events) state the observation limits.

## Compatibility with a changed base

Compatibility uses two independent fresh installations.
One contains base A and overlay O, and the other contains additive base B with the exact same overlay artifacts.
Neither installation updates an existing database in place.

The [compatibility observer](tests/overlay_compatibility.rs) compares consumed contracts, definition ownership, and application permissions.
It refuses a breaking consumed field or constraint.
The [paired deployed case](../wamn_receiving/tests/route_authentication_live/cluster/postcommit_pair.rs) compares the unchanged overlay across both installations.
It also exercises the receipt consumer in each installation.

A compatible additive base does not require rewriting the immutable overlay artifact.
An incompatible consumed contract requires client work and a new package version.
The separate [upgrade plan](../../docs/plan/upgrades.md) covers deferred changes to databases with retained data.
