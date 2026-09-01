# WAMN Receiving Base and Client Overlay — POC Scenario

**Status:** proposed POC scenario  
**Purpose:** prove that WAMN can ship and upgrade a platform-owned Receiving application while a client independently adds schema, API, business behavior, integration, and UI.  
**Relationship:** read alongside `wamn_base_application_poc_revised.md`. Both documents use the immutable platform base plus independently owned client overlay model and together supersede the earlier client owned copy decision.

## Naming and operation identity

### Singular identifiers and `snake_case`

Every wire-level technical identifier uses a **singular noun** and **`snake_case`**:

```text
purchase_order
purchase_order_line
receiving.record_receipt
/acme/purchase_order/:id/quality
```

This applies to data model, operation, route segment, event, package-local identifier, JSON property, SQL relation, and generated function name. Collection behavior is represented by an array or by an action such as `query`, never by pluralizing the data model.

Generated language type names may follow language convention while remaining derived from singular identifiers:

```text
purchase_order     → PurchaseOrder
record_receipt     → ReceivingRecordReceiptInput
```

Hyphenated technical identifiers are not used.

### Package, module, and operation

A **package** is the WAMN application ownership, version, and compatibility boundary. A **module** or **domain** is only source-code and operation organization inside a package and is not part of the public package identity.

Use **package**, unqualified, only for that WAMN application boundary. Other identities are named explicitly:

```text
component artifact      # immutable Wasm bytes identified by digest
UI artifact             # immutable static frontend bundle identified by digest
TypeScript/npm package  # generated frontend distribution artifact
Rust module or application domain
```

The POC has one platform package and one client overlay package. `receiving`, `quality`, and `integration` may be separate domains and component artifacts inside `client_acme_receiving`; they are not independently versioned client overlay packages.

Canonical operation identity:

```text
<package_id>@<package_version>::<local_operation>
```

Local operation forms:

```text
<data_model>.<crud_action>
<domain>.<custom_action>
```

The closed generated CRUD action set is:

```text
get
query
create
update
delete
```

A custom action uses a singular `verb_noun` name:

```text
receiving.record_receipt
quality.approve_inspection
quality.load_purchase_order_detail
```

Examples:

```text
wamn_receiving@1.1.0::purchase_order.get
wamn_receiving@1.1.0::purchase_order.update
wamn_receiving@1.1.0::receiving.record_receipt

client_acme_receiving@3.0.0::receiving.record_receipt
client_acme_receiving@3.0.0::quality.approve_inspection
```

The platform and client may use the same local operation name because their package identities differ. A route, wiring, generated client, or registered caller binds an exact package coordinate; a client operation does not implicitly override a platform operation.

Source code may use a local import alias:

```text
base::receiving.record_receipt
acme::receiving.record_receipt
```

The alias is local ergonomics only. The effective release records the exact package identity and version.

A client overlay that calls a base operation uses a base dependency alias plus contract requirement metadata. The requirement records what the client artifact was compiled and tested against; the effective release resolves it to one exact platform package version and component artifact. The requirement is not a separately versioned operation identity, and no operation executes against a version range.

### Transport-independent actions

Operation names describe behavior, not HTTP verbs:

```text
purchase_order.create
purchase_order.update
purchase_order.delete
```

An HTTP binding may map them as:

```text
POST   /purchase_order      → purchase_order.create
PATCH  /purchase_order/:id  → purchase_order.update
DELETE /purchase_order/:id  → purchase_order.delete
```

`update` has partial-update semantics for the POC:

```text
field absent              → unchanged
field present with null   → set NULL when permitted
field present with value  → set value
```

### Array-shaped invocation

Every registered operation accepts an array and returns an array. A single-record call uses a one-element array; there is no separate singular and batch operation family.

```text
purchase_order.get([key])
  → [result]

purchase_order.query([query])
  → [page]

purchase_order.create([record])
  → [result]

purchase_order.update([change])
  → [result]

purchase_order.delete([key_with_revision])
  → [result]

receiving.record_receipt([command])
  → [result]
```

Each input carries a stable `request_id`; output preserves input correlation and order. A query output contains a bounded page whose collection field remains singular:

```json
{
  "request_id": "purchase_order_query_1",
  "value": {
    "item": [],
    "next_cursor": null
  }
}
```

Every outer input item executes independently using `per_input` semantics. Cross-item atomicity is unsupported in the POC.

```text
receiving.record_receipt([command_1, command_2])

means:
  transaction_1 → command_1 → result_1
  transaction_2 → command_2 → result_2
```

One command item may still contain several records and process them atomically inside its own transaction. Every operation has an item-count and total-byte bound; bulk file transfer remains a later payload or streaming concern.

### Names do not imply authority or behavior

The operation registry separately records:

```text
kind
owner_package
visibility
permission
input_item_semantics (`per_input` in the POC)
transaction_behavior
input_contract
output_contract
error_contract
effect
connection_requirement
```

Neither `purchase_order.update` nor `receiving.record_receipt` gains authority or transaction semantics from its name alone.

## 1. POC outcome

The POC installs one exact platform base and one exact client overlay into one project environment:

```text
wamn_receiving@1.0.0
+ client_acme_receiving@3.0.0
→ effective_release_r17
```

The platform and client contribute to one effective application while retaining separate ownership:

| Platform owns | Client owns |
|---|---|
| Base migration stream | Client migration stream |
| Base data model and field | Client field and data model |
| Base CRUD and read contract | Client CRUD and projection |
| `wamn_receiving@1.0.0::receiving.record_receipt` | Client command and BFF operation |
| Base permission and RLS requirement | Client operation permission |
| Base component artifact, wiring, and route | Client component artifact, wiring, and route |
| Optional default UI artifact | Client UI artifact |

The client does **not** edit platform migration history or platform-owned definitions. The platform does **not** edit client-owned definitions.

Ownership is evaluated at the affected-definition level. Upgrade verification refuses a platform change whose direct or dependent effects alter, remove, conflict with, or invalidate a client-owned definition, including a client-owned field stored in a platform-owned relation.

This layered model replaces the earlier client owned copy model. It is not a source fork and does not introduce a general override or plugin system.

The POC includes only a narrow `effective_release_builder`: it combines one exact platform application package version and one exact client overlay application package version, validates definition ownership and consumed operation contracts, detects conflicts, and emits exact effective bindings. A general-purpose extension resolver remains outside the POC.

## 2. Simplest Receiving base

The platform base supports:

```text
import purchase_order data from CSV
→ query and open purchase_order
→ enter received quantity
→ post one atomic receipt
→ inspect receipt history
```

### Base data model

```text
purchase_order
purchase_order_line
receipt
receipt_line
item
location
```

### Base application operation

```text
purchase_order.get
purchase_order.query
purchase_order.update
receipt.get
receipt.query
receiving.record_receipt
```

Each operation uses the array-shaped invocation contract.

**Owner-ruling correction, 2026-08-29:** the base exposes one real
`purchase_order.update` operation so optimistic-concurrency behavior is
proved without expanding authority to `create` or `delete`.

**Owner-ruling correction, 2026-08-29:** CSV purchase-order import is
demand-gated and is not a base POC operation; Phase 1 implements only
`receiving.record_receipt` as its custom command.

`wamn_receiving@1.0.0::receiving.record_receipt` owns the authoritative database transaction for each command item:

```text
authorize caller
→ begin transaction
→ enforce idempotency
→ lock purchase_order and open purchase_order_line
→ validate status and remaining quantity
→ insert receipt and receipt_line
→ update received quantity and purchase_order status
→ commit
```

The base may include a default static UI, but the backend contracts are the stable reuse boundary.

## 3. Required component and platform capability

### Application component

| Component family | Purpose |
|---|---|
| Generated data component | Compile-verified CRUD and named projection over an owned schema surface. |
| Receiving command component | Implements `receiving.record_receipt` and another atomic base command when needed. |
| CSV component or wiring | Bounded CSV decode or encode and `purchase_order` import or export mapping. |
| BFF component | Optional screen-shaped composition over base and client operation. |
| Client command or integration component | Client-owned business command, ERP adapter, or post-commit processing. |
| UI bundle | Platform default UI or client-owned SPA using generated TypeScript client packages. |

### Platform capability

```text
ordered platform and client migration stream
→ ownership-aware schema verification
→ isolated PostgreSQL migration testing
→ pg_catalog introspection
→ derived effective catalog IR
→ Rust, TypeScript, and JSON Schema generation
→ SQLx verification of the application SQL corpus
→ generated CRUD, route, wiring, and case
→ operation authorization with caller_identity
→ host-selected PostgreSQL identity and RLS
→ component build and OCI publication
→ effective release generation and pointer activation
→ CDC or event delivery for post-commit behavior
```

The exact package manifest and ownership-ledger representation are implementation details for the POC. The required invariant is structural ownership of every managed definition.

## 4. Client overlay model

A client overlay may add:

- a client-owned field on an explicitly extensible base data model;
- a client-owned data model or relation;
- client-generated CRUD;
- a client projection or named query;
- a client command or BFF operation;
- a client route or permission;
- a client wiring or integration;
- a client test;
- a client UI bundle.

Ownership rules:

```text
platform migration
  may modify platform-owned definition
  may not modify client-owned definition

client migration
  may add or modify client-owned definition
  may not alter or drop platform-owned definition
```

A client field on a base table remains client-owned even though it shares the physical table. More substantial client state should use a client-owned table.

Schema ownership and runtime mutation authority are separate. A client-generated operation over a shared platform relation may mutate client-owned fields only, unless the platform base explicitly publishes another mutation capability. By default, the client may not create or delete platform-owned rows or update platform-owned business fields.

The platform base API does not automatically expose a client-owned field. The client exposes it through its own generated CRUD, projection, or BFF operation.

## 5. Scenario A — add a client field

Acme adds an inspection flag to the platform `purchase_order` table:

```sql
ALTER TABLE receiving.purchase_order
    ADD COLUMN acme_inspection_required boolean
    NOT NULL DEFAULT false;

ALTER TABLE receiving.purchase_order
    ADD COLUMN acme_quality_status text
    NOT NULL DEFAULT 'not_required';

ALTER TABLE receiving.purchase_order
    ADD CONSTRAINT purchase_order_acme_quality_status_check
    CHECK (acme_quality_status IN (
        'not_required', 'pending', 'approved', 'rejected'
    ));
```

[Owner-ruling correction, 2026-09-01 — `acme_quality_status` is non-null text
with the `not_required` sentinel; the nullable TypeScript spelling was a
document defect.]

The migration is accepted only because `receiving.purchase_order` is declared extensible and the new column is recorded as client-owned.

The client may expose a narrow generated data operation:

```text
client_acme_receiving@3.0.0::purchase_order.get
client_acme_receiving@3.0.0::purchase_order.update
```

With HTTP bindings such as:

```text
GET   /acme/purchase_order/:id/quality
  → client_acme_receiving@3.0.0::purchase_order.get

PATCH /acme/purchase_order/:id/quality
  → client_acme_receiving@3.0.0::purchase_order.update
```

The generated client mutation is restricted to the client-owned field surface. For example:

```ts
export interface AcmePurchaseOrderUpdateInput {
  request_id: string;
  id: Uuid;
  expected_row_version: Int64;
  change: {
    acme_inspection_required?: boolean;
    acme_quality_status?: QualityStatus;
  };
}
```

It does not expose platform-owned fields such as `supplier_id`, `status`, or received quantity unless the platform explicitly grants that mutation.

Or it may create a combined projection:

```text
client_acme_receiving@3.0.0::quality.load_purchase_order_detail
  platform field:
    purchase_order_number
    supplier_id
    status

  client field:
    acme_inspection_required
    acme_quality_status
```

Generation updates the client-owned artifact set:

```text
Rust and TypeScript model
CRUD or projection SQL
JSON Schema and API client
route, permission, and test
```

The base Receiving API and base command remain unchanged.

## 6. Scenario B — client preprocessing through a BFF

Acme creates:

```text
client_acme_receiving@3.0.0::receiving.record_receipt
```

The BFF operation performs:

```text
authorize receiving.record_receipt
→ read client inspection state
→ apply client form and workflow rule
→ invoke base_receiving::receiving.record_receipt
→ enforce the callee permission for the same caller_identity
→ return a combined confirmation view
```

[Owner-ruling correction, 2026-09-01 — the client BFF local operation is
`receiving.record_receipt`; the prior alternate spelling was a document defect.]

`base_receiving` is a source-level dependency alias. The client overlay stores contract requirement metadata for `receiving.record_receipt`; that metadata is what the immutable client artifact was compiled and tested against. It is not another canonical operation version. The effective release resolves the requirement to one exact implementation:

```text
effective_release_r17
  requirement: base_receiving::receiving.record_receipt
  implementation: wamn_receiving@1.0.0::receiving.record_receipt
```

This is appropriate for UI shaping, advisory checks, defaults, and non-atomic client rules.

A client may instead register its own local `receiving.record_receipt`; package scope keeps it distinct:

```text
wamn_receiving@1.0.0::receiving.record_receipt
client_acme_receiving@3.0.0::receiving.record_receipt
```

The selected route binds one exact identity. The client implementation calls the declared base contract dependency; the effective release records the exact base package implementation.

This composition is **not** sufficient for a rule that must remain true at the exact moment the receipt commits. The inspection state may change between the client check and the base transaction.

## 7. Scenario C — client post-receiving work

After the base command commits:

```text
wamn_receiving@1.0.0::receiving.record_receipt
→ committed receipt state observed through the existing CDC or event plane
→ client wiring
→ client_acme_receiving@3.0.0::quality.create_inspection
```

`quality.create_inspection` is private to the installed application and is the
first genuine materializer consumer. Its inline registration names the source
package and entity; it is not route-bindable and carries no public permission
token. The originating caller identity remains preserved at invocation.

ERP synchronization is deferred because the POC has no ERP consumer. The same
post-commit model remains the direction for a later real integration demand.

[Owner-ruling correction, 2026-09-01 — private `quality.create_inspection` is
the genuine first materializer consumer; `integration.sync_receipt` is
deferred.]

The existing CDC or event plane remains the post-commit integration mechanism. The POC does not add a second generic outbox architecture.

## 8. Scenario D — client rule requiring atomicity

Requirement:

> A receipt must not commit unless the client inspection is approved while `purchase_order` is locked.

The BFF sequence in Scenario B cannot guarantee this.

Atomic client extension of a platform-owned command is unsupported in this POC. Supporting it later would require an explicit transactional extension contract published by the platform base. The POC must identify this limitation rather than simulate atomicity across separate component or router invocations.

## 9. Scenario E — platform upgrade

Current installation:

```text
base:   wamn_receiving@1.0.0
client: client_acme_receiving@3.0.0
active: effective_release_r17
```

The platform publishes:

```text
wamn_receiving@1.1.0
```

The upgrade candidate is evaluated against the combined installed application:

```text
wamn_receiving@1.1.0
+ client_acme_receiving@3.0.0
→ candidate_effective_release_r18
```

### Upgrade verification

```text
start from a representative effective_release_r17 database
→ verify platform and client migration history
→ apply only the platform 1.1.0 migration delta
→ refuse any direct or dependent effect that invalidates a client-owned definition
→ introspect the combined target schema
→ verify the client package's consumed base operation contracts
→ resolve compatible base operation dependencies to exact 1.1.0 implementations
→ generate only effective-release binding, security overlay, manifest, and TypeScript facade artifacts
→ reuse the exact immutable client overlay application package plus compatible component artifacts and the selected UI artifact
→ verify the complete application SQL corpus
→ run platform and client test
→ produce an inactive candidate release
```

If successful, WAMN retains the candidate inactive until the client selects it for deployment:

```text
client selects candidate activation
→ apply the exact tested platform migration
→ verify effective schema and policy contract
→ activate effective_release_r18 by pointer
```

The resulting installation is:

```text
wamn_receiving@1.1.0
+ client_acme_receiving@3.0.0
```

The client does not merge platform source or manually recreate its change. Neither `wamn_receiving@1.1.0` nor `client_acme_receiving@3.0.0` is rewritten while constructing the candidate.

If the platform migration conflicts with a client-owned definition or breaks a consumed contract:

```text
candidate refuses
→ effective_release_r17 remains active
→ client receives a compatibility report
```

The client may update and publish `client_acme_receiving@4.0.0`, after which WAMN evaluates:

```text
wamn_receiving@1.1.0
+ client_acme_receiving@4.0.0
```

For the POC, every version resolved into an effective release is exact. Automatic upgrade policy is optional; silent incompatible activation is prohibited.

## 10. Base update policy (direction only)

A platform base release creates an **upgrade candidate**; it does not immediately replace the client’s active effective release.

```text
current effective release:
  platform_package: wamn_receiving@1.0.0
  client_overlay_package: client_acme_receiving@3.0.0
  client_ui_artifact: sha256:<ui_digest_6>

platform publishes:
  wamn_receiving@1.1.0

candidate effective release:
  platform_package: wamn_receiving@1.1.0
  client_overlay_package: client_acme_receiving@3.0.0
  client_ui_artifact: sha256:<ui_digest_6>
```

Candidate preparation verifies the new base against the installed client overlay:

```text
apply the base migration to a copy of the current effective schema
→ preserve every client-owned definition
→ refuse a platform change whose direct or dependent effects invalidate client-owned state
→ derive the candidate effective catalog
→ verify base contracts and client-consumed contracts
→ resolve stable client dependencies to exact candidate base implementations
→ generate effective-release bindings, policy overlay, manifest, and TypeScript facade
→ reuse the immutable platform and client WAMN application packages plus compatible component and UI artifacts
→ run platform and client tests
→ report compatibility
```

The compatibility result is intentionally small:

```text
compatible
requires_revalidation
incompatible
```

If a client component artifact or UI artifact remains compatible, its exact digest is reused. If backend source or compilation must change, the client publishes a new client overlay package version. If only the frontend changes, the client publishes a new UI artifact. WAMN never changes the contents of an existing application package or artifact digest.

SemVer communicates the platform’s intended compatibility:

| Base release | Intended meaning |
|---|---|
| Patch | Internal correction; no intended public contract break. |
| Minor | Additive schema or operation capability; existing consumers expected to remain valid. |
| Major | Breaking contract or migration requiring explicit client work. |

SemVer is a signal, not proof. Verification against the effective schema and the client’s consumed operation contracts is authoritative. Early automatic candidate generation should focus on additive, predecessor-compatible patch and minor releases; major upgrades remain explicit client projects.

The client controls when a compatible candidate is activated. The platform may later define a supported-version window or a security deadline, but the POC requires only:

```text
no silent replacement
candidate testable before activation
incompatible candidate remains inactive
previous effective release remains the rollback target when schema compatibility permits
```

## 11. UI and generated TypeScript client

### Package identity in JavaScript and TypeScript

JavaScript has no `::` namespace operator. The equivalent boundary is:

```text
ES/npm import
  → generated client distribution for a WAMN application package

import alias
  → local JavaScript namespace

nested client object
  → data_model or domain

function
  → action
```

Canonical WAMN identity:

```text
wamn_receiving@1.1.0::purchase_order.get
```

Frontend use:

```ts
import { create_client as create_wamn_receiving } from "@wamn/receiving";

const wamn_receiving = create_wamn_receiving(transport);

const output = await wamn_receiving.purchase_order.get([
  {
    request_id: "load_purchase_order",
    id: "po_123",
  },
]);
```

The generated client embeds the exact WAMN application package and operation coordinate. The frontend lockfile pins the TypeScript/npm artifact used by the UI.

### Generated TypeScript/npm package shape

```text
@wamn/receiving
  type.ts
  error.ts
  purchase_order.ts
  purchase_order_line.ts
  receipt.ts
  receiving.ts
  client.ts
  index.ts
```

A generated client is organized by the local operation convention:

```ts
export interface WamnReceivingClient {
  readonly purchase_order: {
    get(
      input: readonly PurchaseOrderGetInput[],
    ): Promise<readonly OperationItem<PurchaseOrder>[]>;

    query(
      input: readonly PurchaseOrderQueryInput[],
    ): Promise<readonly OperationItem<Page<PurchaseOrder>>[]>;

    update(
      input: readonly PurchaseOrderUpdateInput[],
    ): Promise<readonly OperationItem<PurchaseOrder>[]>;
  };

  readonly receiving: {
    record_receipt(
      input: readonly ReceivingRecordReceiptInput[],
    ): Promise<readonly OperationItem<ReceivingRecordReceiptResult>[]>;
  };
}
```

**Owner-ruling correction, 2026-08-29:** generated base
`purchase_order` operations are exactly `get`, `query`, and `update`; no
base `create` or `delete` authority is registered.

The shared array result preserves correlation. Each outer item succeeds or fails independently; no transaction spans multiple outer items:

```ts
export type OperationItem<T> =
  | {
      request_id: string;
      value: T;
      error?: never;
    }
  | {
      request_id: string;
      value?: never;
      error: OperationError;
    };

export interface Page<T> {
  item: readonly T[];
  next_cursor: string | null;
}
```

### Generated data type

```ts
export type Int64 = string;
export type Uuid = string;
export type Timestamp = string;
export type Decimal = string;

export interface PurchaseOrder {
  id: Uuid;
  purchase_order_number: string;
  supplier_id: Uuid;
  status: "open" | "complete" | "cancelled";
  row_version: Int64;
  created_at: Timestamp;
  updated_at: Timestamp;
}
```

### Generated CRUD input

```ts
export interface PurchaseOrderGetInput {
  request_id: string;
  id: Uuid;
}

export interface PurchaseOrderQueryInput {
  request_id: string;
  filter?: {
    status?: readonly PurchaseOrder["status"][];
    supplier_id?: readonly Uuid[];
  };
  sort?: {
    field: "purchase_order_number" | "status" | "created_at";
    direction: "ascending" | "descending";
  };
  cursor?: string;
  limit?: number;
}

export interface PurchaseOrderUpdateInput {
  request_id: string;
  id: Uuid;
  expected_row_version: Int64;
  change: {
    supplier_id?: Uuid;
  };
}
```

**Owner-ruling correction, 2026-08-29:** a query accepts at most one declared
sort field and direction. Multi-column sorting is not assembled at runtime; a
future finite expansion requires a named manifest demand.

**Owner-ruling correction, 2026-08-29:** pagination is keyset-based, defaults
to `created_at` ascending, and uses `id` as a stable tie-breaker inheriting the
requested direction. An omitted limit means 100; valid limits are 1 through
100, while zero or a negative limit returns `invalid_input` before SQL. The
opaque v1 wire cursor is canonical compact JSON encoded as unpadded base64url;
an unknown or undecodable cursor returns `invalid_input` rather than resetting
pagination, and clients preserve the cursor verbatim.

**Owner-ruling correction, 2026-08-29:** `supplier_id` is the sole writable
base field. Omission means unchanged, explicit null is refused because the
column is `NOT NULL`, and command-owned `status` is absent from the generated
update input. A stale `expected_row_version` returns the typed
`concurrency_conflict` refusal.

A client package that extends the shared relation receives a narrower generated mutation type:

```ts
export interface AcmePurchaseOrderUpdateInput {
  request_id: string;
  id: Uuid;
  expected_row_version: Int64;
  change: {
    acme_inspection_required?: boolean;
    acme_quality_status?:
      | "not_required"
      | "pending"
      | "approved"
      | "rejected";
  };
}
```

The client type intentionally omits platform-owned mutable fields. The corresponding generated SQL can update only the fields present in this contract.

### Generated custom action input

```ts
export interface ReceivingRecordReceiptLine {
  purchase_order_line_id: Uuid;
  quantity: Decimal;
  location_id: Uuid;
}

export interface ReceivingRecordReceiptInput {
  request_id: string;
  value: {
    idempotency_key: string;
    purchase_order_id: Uuid;
    receipt_reference: string;
    occurred_at: Timestamp;
    line: readonly ReceivingRecordReceiptLine[];
  };
}

export interface ReceivingRecordReceiptResult {
  receipt_id: Uuid;
  purchase_order_id: Uuid;
  purchase_order_status: "open" | "complete";
  row_version: Int64;
}
```

Usage:

```ts
const output = await wamn_receiving.receiving.record_receipt([
  {
    request_id: "record_receipt",
    value: {
      idempotency_key: crypto.randomUUID(),
      purchase_order_id: "po_123",
      receipt_reference: "receipt_100",
      occurred_at: new Date().toISOString(),
      line: [
        {
          purchase_order_line_id: "po_line_1",
          quantity: "12.500",
          location_id: "dock_1",
        },
      ],
    },
  },
]);
```

### Same local operation from two packages

```ts
import {
  create_client as create_wamn_receiving,
} from "@wamn/receiving";

import {
  create_client as create_client_acme_receiving,
} from "@acme/receiving";

const wamn_receiving = create_wamn_receiving(transport);
const client_acme_receiving = create_client_acme_receiving(transport);
```

The following calls remain unambiguous even when both packages register `receiving.record_receipt`:

```ts
await wamn_receiving.receiving.record_receipt([base_command]);
await client_acme_receiving.receiving.record_receipt([client_command]);
```

The import alias is a local JavaScript name. The generated client sends the exact package identity, version, and local operation coordinate.

### Effective-release TypeScript facade

A client UI may install one generated TypeScript/npm package for the exact effective release:

```ts
import {
  wamn_receiving,
  client_acme_receiving,
} from "@acme/receiving_application";

await client_acme_receiving.quality.approve_inspection([command]);
```

Conceptually, the facade pins and re-exports the two WAMN application package clients:

```text
wamn_receiving@1.1.0
client_acme_receiving@3.0.0
```

`quality` and `integration` are domains inside `client_acme_receiving`, not additional client packages. `@acme/receiving_application` is an npm distribution artifact for the effective release; it does not merge application package ownership or operation identities.

### UI ownership

The client may use the platform Receiving UI unchanged, or publish a separate client SPA as a UI artifact:

```text
name: acme_receiving_ui
release_label: 6.0.0
digest: sha256:<ui_digest_6>
```

The release label is frontend distribution metadata; the digest is the immutable release identity. Neither makes the UI another WAMN application package.

A client UI may call both WAMN application package surfaces:

```text
wamn_receiving
  purchase_order.get
  purchase_order.query
  purchase_order.update
  receipt.get
  receipt.query
  receiving.record_receipt

client_acme_receiving
  purchase_order.get
  purchase_order.update
  receiving.record_receipt
  quality.load_purchase_order_detail
  quality.approve_inspection
```

**Owner-ruling correction, 2026-08-29:** the platform client surface includes
the real, narrowly writable `purchase_order.update`; it does not imply base
`create` or `delete` authority.

A platform backend upgrade does not silently replace a client UI. A new effective-release TypeScript facade may be generated, while the immutable platform package, client overlay package, component artifacts, and existing UI artifact remain unchanged. If a consumed contract requires backend source changes, the client publishes a new overlay package version. If only the frontend changes, the client publishes a new UI artifact.

The generated TypeScript/npm package is framework-independent. A Solid UI consumes it directly; framework-specific generated stores or hooks are outside the POC.

## 12. Simplest design and implementation process

### Phase 1 — build the platform Receiving base

```text
write platform migration
→ declare base CRUD, projection, permission, operation limits, and `per_input` semantics
→ implement receiving.record_receipt
→ generate and SQLx-check the base application SQL corpus
→ build component, route, case, TypeScript package, and optional default UI
→ publish wamn_receiving@1.0.0
```

### Phase 2 — build the client overlay

```text
declare required base operation contracts
→ test them against exact wamn_receiving@1.0.0
→ write client migration
→ declare client CRUD, projection, permission, operation limits, and `per_input` semantics
→ optionally implement client BFF, command, or integration
→ generate client TypeScript package
→ build client UI
→ generate, check, and test against the combined schema
→ publish client_acme_receiving@3.0.0
```

Normal client loop:

```bash
wamn diff
wamn check
wamn test
wamn publish
```

### Phase 3 — create the effective release

```text
exact base package version
+ exact client overlay package version
→ combined migration result
→ derived effective catalog
→ generated and verified artifact
→ exact component and UI digest
→ exact dependency binding set
→ exact TypeScript/npm client artifacts and effective-release facade
→ effective release
→ activation
```

This combination is deterministic and build-time. Runtime executes only the resolved release; it does not resolve ownership or override dynamically.

### Phase 4 — prove a platform upgrade

```text
publish wamn_receiving@1.1.0
→ evaluate against client_acme_receiving@3.0.0
→ derive new effective-release bindings and frontend facade
→ reuse immutable client artifacts when compatible
→ check and test the inactive candidate
→ activate only through an explicit client-controlled selection
```

## 13. POC acceptance

The scenario is complete when it proves:

1. The layered base plus overlay model supersedes client-owned copy, and one exact platform application package version plus one exact client overlay application package version produce one effective release.
2. Platform and client migration streams coexist in one database with ownership evaluated at the affected-definition level.
3. A client adds a field to an extensible base data model without gaining DDL authority over a platform-owned field.
4. Client-generated update contracts and SQL over a shared relation can mutate client-owned fields only unless the platform explicitly grants another mutation capability.
5. Client-generated CRUD and a client projection expose the new field while the base API remains stable.
6. Canonical operation identity includes exact package, version, and local operation.
7. Platform and client application packages may register the same local operation without collision or implicit override.
8. Every registered operation accepts an array and returns a correlated array; every outer item executes independently under fixed `per_input` semantics.
9. Generated TypeScript exposes `package_alias.data_model.action(...)` and `package_alias.domain.custom_action(...)` functions.
10. A client BFF calls a stable base operation contract that the effective release resolves to an exact base implementation, with preserved `caller_identity` and nested permission enforcement.
11. A committed receipt is observed through the existing CDC or event plane and causes a client-owned post-commit wiring to execute.
12. `receiving.record_receipt` remains one authoritative atomic platform transaction for each outer input item.
13. The system refuses to claim atomicity for a separate precheck and base command call.
14. A platform `1.0.0 → 1.1.0` migration is tested against the combined platform and client predecessor state.
15. Upgrade verification refuses a platform change whose direct or dependent effects invalidate a client-owned definition.
16. The platform upgrade preserves a client-owned field, table, component artifact, operation identity, and UI artifact identity.
17. A conflict blocks candidate activation and leaves the prior effective release active.
18. Publishing a base update produces an inactive candidate and never silently replaces the active effective release.
19. A compatible candidate reuses immutable platform and client application packages, component artifacts, and UI artifact, generates only new effective-release artifacts, and activates through an explicit client-controlled selection.
20. A client frontend can consume an effective-release TypeScript/npm facade without introducing another WAMN application package or losing ownership boundaries.

## 14. Explicitly deferred

```text
general_extension_resolver
arbitrary_base_definition_override
version_range_execution
transactional_base_command_extension
automatic_client_code_reconciliation
cross_input_atomicity
platform_ui_plugin_system
destructive_schema_migration
framework_specific_client_hook
```

The POC proves the smaller model:

```text
platform-owned base package
+ client-owned overlay package
→ one verified effective application
→ exact application_package, component_artifact, ui_artifact, and typescript_facade binding
→ independently upgradeable when ownership and consumed contracts remain compatible
```
