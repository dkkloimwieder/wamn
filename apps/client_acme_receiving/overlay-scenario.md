# Acme Receiving overlay

Acme adds inspection behavior to the Receiving base without copying its source.
The [manifest](wamn.k) names an exact `wamn_receiving` dependency through `base_receiving`.
The base and overlay keep separate package versions, migration histories, artifacts, and permissions.
Shared definition ownership belongs in [data access](../../docs/architecture/data-access.md).

## Client-owned state

Acme adds `acme_inspection_required` and `acme_quality_status` to the extensible `receiving.purchase_order` relation.
Both columns and `purchase_order_acme_quality_status_check` remain client-owned.
The quality status is non-null text with exactly `not_required`, `pending`, and `approved`.
A reject operation and `rejected` state are absent.

Acme's purchase-order update exposes only those two client fields.
The shared generator derives its package-targeted typed WIT contract from the Acme declarations.
The component calls Acme's existing SQL accessor directly and keeps JSON conversion at the HTTP and dynamic routing boundaries.
It cannot update the base supplier, status, or received quantities.
It cannot create or delete base purchase-order rows.
Its [initial SQL](migrations/0001_initial.sql) and manifest carry that declared surface.

The [initial SQL](migrations/0001_initial.sql) creates `quality_inspection`.
Its primary key and receipt foreign key use `receipt_id`.
There is no second inspection identifier.
Inspection status is `pending` or `approved`.

## Public operations

The [manifest](wamn.k) defines five operations that the build exposes as authenticated POST routes.

| Path | Operation |
|---|---|
| `/acme/purchase_order/get` | `purchase_order.get` |
| `/acme/purchase_order/update` | `purchase_order.update` |
| `/acme/acme/record_receipt` | `acme.record_receipt` |
| `/acme/quality/load_purchase_order_detail` | `quality.load_purchase_order_detail` |
| `/acme/quality/approve_inspection` | `quality.approve_inspection` |

The Acme receipt operation awaits the declared base operation through a generated typed WIT interface.
Both run in the one host transaction of the item, each through its own `statements` instance.
Acme then runs its [inspection statement](command/record_receipt/record_inspection.sql) and returns the base receipt result unchanged.
A purchase order that requires no inspection gets no row, an approved one gets its `quality_inspection` row, and one that is not approved is refused with `invalid_input` on `purchase_order_id`, so the item rolls back with its receipt.
Acme claims the idempotency key under its own operation, not under the base operation.
The build composes the overlay with its base, so the call reaches the base inside one component. The release folds the base permission into the overlay entry's grant.
The effective release resolves `base_receiving` to one exact implementation.
The overlay's compiled requirement remains separate from that selected implementation.

Clients read inspection details separately through the existing purchase-order query.
A failed detail read does not change a successful receipt result or authorize reposting.

The detail projection combines declared base and client fields for the screen.
Approval identifies the inspection by `receipt_id` and advances the purchase-order revision.
Its result includes `purchase_order_row_version` for a later edit.
The [component's data access code](component/src/data/operation.rs) implements the custom operations and their refusals.

## Committed receipt events

The private `quality.create_inspection` handler responds to committed Receiving receipt events.
Its registration names the exact source package and entity.
It has no public route or permission token.
The registration grants delivery authority without fabricating a caller identity.
The event retains its causation identity.

Repeated delivery for the same receipt must leave one inspection.
A distinct receipt must receive its own inspection.
The [Receiving data access tests](../../tests/receiving-tests/receiving_data_access.rs) run the inspection SQL twice for the same receipt and expect one insert.
Payload recovery remains limited by broker retention.

ERP synchronization remains deferred because this application declares no ERP consumer.
The event path adds no generic outbox or alternate delivery system.
The [event test methods](../../docs/testing/application-tests.md#committed-events) state the observation limits.

## Compatibility with a changed base

Compatibility uses two independent fresh installations.
One contains base A and overlay O, and the other contains additive base B with the exact same overlay artifacts.
Neither installation updates an existing database in place.

Compare the contracts that the overlay uses, the owners of those definitions, and the application permissions.

A compatible additive base does not require rewriting the immutable overlay artifact.
An incompatible consumed contract requires client work and a new package version.
