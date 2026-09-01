# WAMN Base Application POC Architecture

**Status:** proposed POC direction  
**Scope:** platform base applications, client overlays, SQL-authored data models, generated CRUD, compile-verified data access, transactional commands, BFF operations, generated clients, migration verification, authorization, and release activation  
**Relationship:** this document is the general architecture for `wamn_receiving_layered_application_poc_scenario.md`. The layered base plus overlay model **supersedes** the earlier client owned copy assumption.

## 1. POC decisions

1. A deployed application is an exact layered installation:

   ```text
   immutable platform base package
   + immutable client overlay package
   → exact effective release
   ```

   The client does not copy or edit the platform package. The platform does not edit client-owned definitions.
2. The POC includes a narrow `effective_release_builder` that combines one exact platform application package version and one exact client overlay application package version, validates ownership and consumed contracts, detects conflicts, and emits exact effective bindings. A general-purpose extension resolver remains deferred.
3. Each package version owns an ordered, cumulative PostgreSQL migration stream. Every non-root version declares its exact `predecessor_version`; package versions form one linear, one-leaf lineage. The physical data model is authored as migration SQL; WAMN does not introduce a YAML or JSON schema DSL.
4. Schema authority is one way:

   ```text
   migration SQL
   → PostgreSQL
   → pg_catalog introspection
   → normalized WAMN catalog IR
   → generated artifacts
   ```

   The catalog IR is a derived runtime and release artifact, not an independent schema-editing surface.
5. A future schema designer may generate migration SQL, but it must not mutate the derived catalog independently.
6. The **application SQL corpus**—generated CRUD, lock, and mutation SQL plus authored named query and projection SQL—is compile-verified against the exact proposed PostgreSQL schema. Arbitrary workflow or developer SQL remains runtime-checked.
7. Generated CRUD operations use implicit transactions. Complex atomic behavior is compiled into registered command operations and executes within one explicit database transaction.
8. A transaction resource never crosses a WAMN wiring edge. Wirings compose coarse operations; database transaction sequencing remains inside one component invocation.
9. Authorization has two separate enforcement layers:

   ```text
   trusted CallerIdentity
   → registered-operation permission check
   → CRUD, projection, BFF, or command execution

   host-selected database identity
   → PostgreSQL privileges and RLS
   ```
10. Every managed definition has a package owner. Schema ownership and runtime mutation authority are separate.
11. Registered operations use exact package-scoped identity and a common array envelope with fixed `per_input` semantics.
12. A platform base update creates an inactive candidate effective release. The client controls activation; an active release is never silently overwritten.
13. An application is an immutable release pack, not necessarily one monolithic Wasm component.

## 2. Package, operation, and naming model

### 2.1 Package versus module

A **package** is the WAMN application ownership, compatibility, and version boundary. A **module** or **domain** is only source-code and operation organization inside a package and is not part of the public package identity.

Use **package**, unqualified, only for that WAMN application boundary. Other packaging concepts are named explicitly:

```text
Rust module or application domain
component artifact      # immutable Wasm bytes identified by digest
UI artifact             # immutable static frontend bundle identified by digest
TypeScript/npm package  # generated frontend distribution artifact
```

A client overlay may contain several modules/domains and several component artifacts without becoming several client overlay packages. A separately built UI or generated npm distribution likewise does not create another WAMN application package.

Canonical operation identity:

```text
<package_id>@<package_version>::<local_operation>
```

Examples:

```text
wamn_receiving@1.1.0::purchase_order.get
wamn_receiving@1.1.0::receiving.record_receipt
client_acme_receiving@3.0.0::receiving.record_receipt
```

The platform and client may register the same local operation because the package coordinates are distinct. A route, wiring, generated client, or registered caller binds an exact package and operation identity.

Source code may use aliases such as:

```text
base::receiving.record_receipt
acme::receiving.record_receipt
```

The alias is local ergonomics only. The effective release records exact package coordinates.

A client overlay consumes a base operation through a dependency alias plus **contract requirement metadata**. The requirement records the input, output, error, authorization, and relevant behavior the client artifact was compiled and tested against; it is not a second operation-version coordinate. The effective release resolves that requirement to one exact platform package version and component artifact. Nothing executes against a version range.

### 2.2 Local operation convention

Local operations use one of two forms:

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

Custom actions use singular `verb_noun` names:

```text
receiving.record_receipt
receiving.reverse_receipt
quality.approve_inspection
quality.load_purchase_order_detail
```

Operation names are transport-independent. HTTP may map:

```text
POST   /purchase_order      → purchase_order.create
PATCH  /purchase_order/:id  → purchase_order.update
DELETE /purchase_order/:id  → purchase_order.delete
```

### 2.3 Technical naming

WAMN-owned wire and schema identifiers use singular `snake_case`:

```text
purchase_order
purchase_order_line
receiving.record_receipt
/acme/purchase_order/:id/quality
```

This applies to data model, operation, package-local identifier, route segment, event identifier, JSON property, SQL relation/field, and generated function name. Generated language types may follow language convention:

```text
purchase_order  → PurchaseOrder
record_receipt  → ReceivingRecordReceiptInput
```

Hyphenated WAMN technical identifiers are not used. Third-party protocol fields may retain their required external spelling.

### 2.4 Array-shaped invocation

Every registered operation accepts an array and returns a correlated array. A single-record call uses a one-element array.

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

Each input carries a stable `request_id`. Each outer input item executes independently using fixed `per_input` semantics:

```text
receiving.record_receipt([command_1, command_2])

means:
  transaction_1 → command_1 → result_1
  transaction_2 → command_2 → result_2
```

One command item may contain several records and process them atomically inside its transaction. Cross-item atomicity is outside the POC.

Operation metadata—not its name—records:

```text
kind
owner_package
visibility
permission
input_item_semantics
transaction_behavior
input_contract
output_contract
error_contract
effect
connection_requirement
```

## 3. Authored project surface

The platform application package and client overlay application package use the same authoring model.

Illustrative platform package:

```text
wamn_receiving/
  migrations/
    0001_initial.sql

  query/
    open_purchase_order.sql
    receipt_screen.sql

  command/
    record_receipt.rs
    reverse_receipt.rs

  bff/
    receiving.rs

  test/
  wamn.json
```

Illustrative client overlay:

```text
client_acme_receiving/
  migrations/
    0001_add_inspection_required.sql
    0002_quality_inspection.sql

  query/
    quality_purchase_order_detail.sql

  command/
    approve_inspection.rs

  bff/
    receiving.rs

  test/
  wamn.json
```

The migration files own physical PostgreSQL structure. Application metadata declares only behavior PostgreSQL cannot infer:

```text
package identity and base contract dependencies
predecessor_version and cumulative migration lineage
definition ownership
CRUD and route exposure
operation permissions
create- and update-writable fields
server-owned and audit fields
optimistic-concurrency revision field
permitted filters, sorts, and projections
registered commands and BFF operations
connection requirements
component artifact grouping
screen exposure
```

Non-CRUD declarations use one closed `custom_operations` map with kinds
`projection`, `command`, and `event_handler`. Every declaration is explicitly
`public` or `private`: a public operation carries its exact local permission,
while a private operation carries no permission token. An event handler alone
carries inline `source_package` and `entity` registration metadata.

[Owner-ruling correction, 2026-09-01 — one custom-operation grammar replaces
separate command/projection/handler grammars; private handlers are not public
permission-bearing operations.]

These declarations reference introspected relations and fields; they do not redefine column types, nullability, defaults, constraints, indexes, or relationships.

`wamn.json` may be split into source fragments such as `exposure/`, `permission/`, or `screen/`, but those fragments resolve into one canonical application model. Generated files are disposable. Developers and agents edit only migrations, named SQL, metadata, command/BFF source, UI source, and tests.

## 4. Managed PostgreSQL and definition ownership

“PostgreSQL is authoritative” does not mean WAMN models arbitrary PostgreSQL. WAMN introspects only configured application schemas and a closed supported object set.

### 4.1 Supported initially

```text
ordinary table
column using the supported type mapping
nullability
approved default
identity/generated-column property where supported
primary key
unique constraint
foreign key
check constraint
ordinary index
```

### 4.2 Normalized rather than duplicated

```text
identity sequence
  → property of the identity column

index backing primary key or unique constraint
  → represented by the constraint

PostgreSQL OID, generated name, and storage setting
  → excluded from semantic identity
```

### 4.3 Refused in managed application schemas

```text
CREATE EXTENSION
role, grant, or SET ROLE
function or procedure
trigger, rule, or event trigger
foreign table
client-authored view or materialized view
client-authored RLS policy
untrusted language
unsupported domain or custom type
nontransactional schema operation
cross-schema mutation outside approved application schemas
```

Read models and projections are authored as named static queries under `query/`; WAMN does not introspect client-authored PostgreSQL views as data models or operations in the POC.

### 4.4 Definition ownership

Every managed definition records an `owner_package`.

```text
platform package
  may change platform-owned definition
  may not change client-owned definition

client package
  may add or change client-owned definition
  may not alter or drop platform-owned definition
```

A platform data model may explicitly permit additive client-owned fields. A client field stored in a platform-owned relation remains client-owned. More independent client state uses a client-owned relation.

Ownership is evaluated at the affected-definition level. Upgrade verification refuses a platform change whose direct or dependent effects alter, remove, conflict with, or invalidate a client-owned definition.

### 4.5 DML authority on shared relations

Schema ownership does not grant unrestricted runtime mutation authority.

For the POC:

```text
client package over client-owned relation
  → declared CRUD over client-owned fields

client package over shared platform relation
  → read explicitly exposed platform fields
  → read and update client-owned fields
  → no platform-owned row create/delete by default
  → no platform-owned business-field update by default
```

Mutation of a platform-owned field requires an explicitly published platform operation or mutation capability.

Generated input contracts and SQL enforce this. A client operation such as:

```text
client_acme_receiving@3.0.0::purchase_order.update
```

may expose:

```text
acme_inspection_required
acme_quality_status
```

but not platform-owned fields such as `status`, `supplier_id`, or `received_quantity` unless explicitly granted.

### 4.6 Platform security overlay

WAMN-owned grants and RLS remain a separate platform security overlay. Client migrations cannot author, replace, or weaken them.

Generation derives the required overlay from the effective introspected schema and application authority metadata. Deployment applies the platform-owned overlay after application migrations and before policy verification and activation.

## 5. Generation and SQLx verification

The effective generation pipeline is:

```text
exact platform migration stream
+ exact client migration stream
    ↓
exact PostgreSQL effective schema
    ↓ pg_catalog introspection
normalized effective WAMN catalog IR
    ↓
model · SQL · CRUD · projection · API schema · client · case
```

The generator assembles one exact **application SQL corpus**:

```text
application SQL corpus
  = generated CRUD, lock, and mutation SQL
  + authored named query and projection SQL
```

Two sibling build targets consume the exact same SQL files:

```text
application SQL file
    ├── native verifier
    │     sqlx::Postgres
    │     query_file!/query_file_as!
    │     cargo sqlx prepare --check
    │
    └── Wasm runtime artifact
          WamnPostgres custom sqlx Database
          executes the same SQL through wamn:postgres
```

Compile-time verification proves:

```text
SQL parses against the exact effective schema
bind count and PostgreSQL bind types match
result columns, types, and nullability match generated Rust models
generated routes reference real registered operations
registered operation contracts and clients compile
```

Integration tests still prove:

```text
current_user RLS and production privileges
operation authorization before mutation
custom WamnPostgres type parity
constraints and data-dependent behavior
lock and optimistic-concurrency behavior
transaction rollback and idempotency
business invariants
```

Generation refuses PostgreSQL types not representable by the production `wamn:postgres` value contract.

Each immutable application package records:

```text
verified_schema_state_id
required_schema_contract
required_platform_policy_contract
application SQL corpus identity
source and toolchain provenance
```

A later additive base schema may satisfy an unchanged client overlay package’s required contracts without changing that overlay package’s bytes.

## 6. Generated data access and CRUD

### 6.1 Generated data access

The generator emits package-owned typed accessors and registered operations. It does not expose one universal runtime controller with unrestricted `call(operation_id, json)` authority over the effective catalog.

Generated output includes:

```text
Rust model and codec
static SQL file
typed CRUD/query/lock accessor
transaction runner
operation contract and metadata
source map
TypeScript type and client
```

The same generated data-access kernel may be compiled into several narrow artifacts:

```text
CRUD/data component
transaction-command component
projection/BFF component
CSV/integration component
```

Package boundaries and generated contracts preserve platform/client ownership.

### 6.2 Generated CRUD semantics

The canonical generated actions are:

```text
get
query
create
update
delete
```

Not every data model exposes every action. Operational records may expose only `get` and `query`, while mutation occurs through a domain command.

#### `create`

- Omitted field: PostgreSQL/default generation applies.
- Explicit `null`: allowed only for nullable writable fields.
- Identity, generated, revision, audit, and other server-owned fields: refused if supplied.
- Generated values are returned in the result.

#### `update`

The WAMN operation is `update`; an HTTP route may use `PATCH`.

```text
field absent             → unchanged
field present with null  → set NULL if nullable
field present with value → set value
```

Generated static SQL preserves all three states.

#### Optimistic concurrency

A mutable generated data model may designate a revision field such as:

```sql
row_version bigint NOT NULL DEFAULT 1
```

`update` and `delete` require the expected revision. A successful update increments it.

```text
row absent                    → not_found
row exists, revision differs  → concurrency_conflict
success                       → new row and revision
```

No silent last-writer-wins mutation is generated.

### 6.3 Query and projection

`query` returns a bounded page per outer input item. Named static projections support screen-shaped reads and reports:

```text
receiving.load_receipt_screen
quality.load_purchase_order_detail
```

Each declares one result class:

```text
one
optional_one
page
bounded_list
```

List and projection results have hard row and byte limits. Large import/export and binary paths use a later payload or streaming boundary rather than unbounded JSON materialization.

## 7. Transaction commands and BFF operations

### 7.1 Registered transaction command

A registered command is a coarse operation such as:

```text
wamn_receiving@1.1.0::receiving.record_receipt
client_acme_receiving@3.0.0::quality.approve_inspection
```

The registered adapter accepts an array and executes each input item independently. One `record_receipt` item owns one database transaction.

Its internal implementation may use a private helper:

```rust
pub async fn record_receipt_item(
    db: &Database,
    command: RecordReceipt,
) -> Result<ReceiptResult, ReceiptError> {
    db.with_transaction(TransactionProfile::ReceiptPosting, |tx| {
        Box::pin(record_receipt_in(tx, command))
    })
    .await
}

pub(crate) async fn record_receipt_in(
    tx: &mut ReceivingUnitOfWork,
    command: RecordReceipt,
) -> Result<ReceiptResult, ReceiptError> {
    // generated typed read, lock, and write access
}
```

`record_receipt_in` is internal Rust reuse, not a registered application operation or compatibility contract.

POC command rules:

```text
no raw SQL in command source
no direct import of wamn:postgres or unrestricted database capability
generated typed accessor and named-query accessor only
command dependencies inventoryable from approved package/module imports
```

The POC does not require Rust call-graph analysis or a generated per-command unit-of-work trait.

The transaction runner owns begin, commit, and rollback on error, drop, trap, or deadline. Serialization and deadlock failures surface as typed errors to the caller; the runner performs no automatic retry. Retry arrives only with a named policy demand and is owned by the caller.

[Owner-ruling correction: `wamn-0h0g.22.2a`, 2026-08-28 — no automatic retry.]

No HTTP, messaging, or other network effect occurs while database locks are held. Post-commit behavior uses the existing CDC/event and wiring path.

### 7.2 BFF operation

A BFF shapes data for a UI or application API:

```text
receiving.load_receipt_screen
receiving.record_receipt
quality.load_purchase_order_detail
```

[Owner-ruling correction, 2026-09-01 — the client BFF local operation is
`receiving.record_receipt`; the prior alternate spelling was a document defect.]

It should prefer a compiled projection over many chatty primitive reads. A write BFF calls one already-atomic command, then loads a post-commit projection.

A client BFF may combine:

```text
client-owned read or validation
→ registered platform command
→ client-owned post-commit read or response shaping
```

This composition is not one shared transaction. Atomic client extension of a platform-owned command is unsupported in the POC; a future explicit transactional extension contract would be required.

### 7.3 Base operation dependencies

A client overlay that calls a base registered operation depends on it through an alias such as `base_receiving` and stores a contract requirement in dependency metadata. The requirement is what the immutable client artifact was compiled and tested against; the resolved implementation is the exact platform package version and component digest selected by the effective release.

```text
effective_release_r17
  requirement: base_receiving::receiving.record_receipt
  implementation: wamn_receiving@1.0.0::receiving.record_receipt

effective_release_r18
  requirement: base_receiving::receiving.record_receipt
  implementation: wamn_receiving@1.1.0::receiving.record_receipt
```

No independent `@0.1` operation-version axis is introduced in the POC. If the candidate base no longer satisfies the stored contract requirement, the client overlay is incompatible and must publish a new version.

## 8. Authorization and identity

Every externally callable CRUD, projection, BFF, or command operation declares an operation permission.

```text
authenticated request
→ trusted CallerIdentity
→ registered-operation permission check
→ operation execution
```

Calling another registered operation preserves the same trusted `CallerIdentity` and enforces the callee operation’s permission before database work. Calling an explicitly private implementation function such as `record_receipt_in(...)` does not create another registered authorization boundary.

Refusal occurs before database mutation. Data-dependent business authorization may additionally execute inside the command transaction.

The host independently selects the PostgreSQL execution identity for project, tenant, and authority class. PostgreSQL privileges and RLS enforce the database boundary through `current_user`.

The authenticated human/service actor and PostgreSQL login are distinct concepts. Request data cannot select either identity, and a request-body `user_id` is never authoritative.

The POC does not define a role hierarchy, field-level policy language, per-operation database role, or general policy-expression DSL.

## 9. Layered schema and client customization

An installed application records exact application package versions:

```text
wamn_receiving@1.0.0
+ client_acme_receiving@3.0.0
→ effective_release_r17
```

The platform base may declare `purchase_order` field-extensible. The client can then add a client-owned field through its migration stream:

```sql
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
with the `not_required` sentinel; nullable and sentinel spellings of “none” are
not both admitted.]

The client separately selects where the field is exposed:

```text
client purchase_order.get/query output
client purchase_order.update input
client projection
client UI
```

The platform package’s operation contracts do not silently acquire the field.

The generated client mutation surface over the shared relation includes only client-owned fields. Platform-owned receipt behavior remains behind the platform command:

```text
wamn_receiving@1.0.0::receiving.record_receipt
```

Client pre-processing may call that registered operation through a BFF. Client post-processing runs after commit through the existing event plane. Neither path rewrites the platform command.

## 10. Migration history and safety

### 10.1 Separate immutable migration streams

Platform and client package IDs retain independent, ordered migration lineages. Each package version carries the complete cumulative migration stream from its lineage root. A root version declares `predecessor_version: null`; every later version names the exact prior version:

```text
wamn_receiving@1.0.0
  predecessor_version: null
  migrations: [0001_initial.sql]

wamn_receiving@1.1.0
  predecessor_version: "1.0.0"
  migrations: [0001_initial.sql, 0002_add_receipt_reference.sql]
```

The predecessor's ordered paths and checksums must be a byte-identical prefix of the candidate. The only admitted successor names the current leaf as its predecessor, so a package ID has one linear lineage and exactly one leaf; forks, merges, skipped predecessors, removal, reordering, and mutation of inherited migrations refuse.

```text
existing migration id + same checksum
  → already applied

existing migration id + different checksum
  → refuse

new migration presented at a package version already held by an effective release
  → package-version-sealed; refuse
  → remedy: publish a new package version that names the current leaf as predecessor_version

new package version + exact current-leaf predecessor + byte-identical inherited prefix
  → eligible; apply only the new cumulative suffix
```

Before a package version's first effective-release membership, its candidate stream may be corrected. The first membership locks the package coordinate and seals that exact cumulative stream atomically. After sealing, neither a new ledger row nor changed bytes may be added at that coordinate; every correction or addition requires a new cumulative package version.

The effective release records exact package coordinates. Their immutable package records own the cumulative migration histories; the release does not carry a second ledger snapshot.

[Owner-ruling correction, 2026-08-30 — package-coordinate seal; publish a new cumulative version rather than appending after effective-release membership.]

### 10.2 Verification environment

Verification runs in a disposable PostgreSQL environment with:

```text
non-superuser migration role
ownership only over approved application schemas
no role, database, or extension creation
no SET ROLE
no production credential or data
no network egress
bounded statement, lock, and transaction timeout
bounded storage and process resource
```

A coarse statement scanner may reject obvious dangerous classes early, but the restricted role and isolated environment are the primary containment boundary.

Production applies only exact migration bytes already tested, using an equivalent restricted migration role.

### 10.3 Fresh and upgrade proofs

```text
fresh install:
  empty database
  → exact base migration stream
  → exact client migration stream
  → target schema_state_id

upgrade:
  current effective release database
  + candidate whose predecessor_version is the current package version
  + representative predecessor fixture when needed
  → verify the predecessor as the candidate's byte-identical cumulative prefix
  → apply only the candidate package migration suffix
  → target schema_state_id
```

Data-changing or constraint-strengthening migrations exercise relevant predecessor boundary cases such as null values, duplicate candidates, foreign-key references, check boundaries, and backfilled rows.

### 10.4 Ownership-aware change policy

A candidate platform migration is refused when its direct or dependent effects invalidate a client-owned definition. This includes:

```text
drop or rename of a shared relation
retype of a client-owned field
constraint that invalidates client-owned values
physical-name collision
break of a consumed client projection or operation contract
```

The POC supports only active-predecessor-compatible additive migration. Drain-required rollout is deferred.

## 11. Build, base update, and activation

### 11.1 Initial effective release build

```text
1. Verify exact cumulative platform and client migration streams and their linear predecessor lineages.
2. Apply exact platform and client streams to a fresh database.
3. Introspect the effective managed schema.
4. Generate package-owned data access, operation contracts, clients, and the platform security overlay.
5. Verify the application SQL corpus with native SQLx.
6. Compile package components and frontend clients.
7. Run authorization, RLS, transaction, concurrency, and application tests.
8. Build one inactive exact effective release.
```

### 11.2 Base update candidate

A new platform base version creates an inactive candidate; it does not replace the active release.

```text
current:
  wamn_receiving@1.0.0
  client_acme_receiving@3.0.0
  effective_release_r17

platform publishes:
  wamn_receiving@1.1.0

candidate:
  wamn_receiving@1.1.0
  client_acme_receiving@3.0.0
  candidate_effective_release_r18
```

Candidate preparation:

```text
start from a representative effective_release_r17 database
→ require platform 1.1.0 predecessor_version = 1.0.0
→ verify the installed 1.0.0 ledger as the byte-identical cumulative prefix
→ apply only the tested platform migration suffix
→ preserve every client-owned definition
→ introspect the candidate effective schema
→ verify client-consumed base operation contracts
→ resolve stable base dependency aliases to exact 1.1.0 implementations
→ reuse the exact client overlay application package plus compatible component artifacts and the selected UI artifact
→ generate only effective-release bindings, security overlay, manifest,
  compatibility report, and TypeScript facade
→ run platform and client tests
```

Compatibility result:

```text
compatible
requires_revalidation
incompatible
```

If client backend source or component compilation must change, the client publishes a new client overlay package version. A UI-only change publishes a new UI artifact digest and may create a new effective release without changing the client overlay package. WAMN never changes the contents of an existing application package or artifact digest.

SemVer communicates intended compatibility:

| Base release | Intended meaning |
|---|---|
| Patch | Internal correction; no intended public contract break. |
| Minor | Additive schema or operation capability; existing consumers expected to remain valid. |
| Major | Breaking contract or migration requiring explicit client work. |

SemVer is a signal, not proof. Verification against the effective schema and consumed operation contracts is authoritative.

The client controls activation of a compatible candidate. The platform may later define a supported-version window or security deadline; no timing policy is fixed by this POC.

### 11.3 Deployment and activation

Assume one deployment writer:

```text
1. Verify the expected current package version is the candidate's explicit predecessor and current lineage leaf.
2. Verify the inherited cumulative prefix, then apply the exact tested migration suffix and immutable candidate-version records.
3. Introspect and require the candidate schema_state_id.
4. Derive and apply the platform-owned RLS/grant overlay.
5. Verify required schema and platform-policy contracts.
6. Verify exact operation, route, wiring, permission, and connection bindings.
7. Run the candidate compatibility gate.
8. Activate the candidate effective release by pointer.
```

If migration or verification fails, the candidate is not activated. An incompatible candidate remains inactive. The previous effective release remains the rollback target when the additive schema still satisfies its required contracts.

### 11.4 Runtime and drift

Runtime components do not introspect `pg_catalog` during readiness. Deployment and activation prove compatibility; runtime trusts the activated release state.

Schema drift is checked during generation, before and after migration, at activation, and through an operator-invoked or periodic control-plane check. Manual changes outside recorded migration history are unsupported.

## 12. Generated TypeScript client

A generated TypeScript/npm client provides the JavaScript equivalent of canonical WAMN application package scoping.

Canonical operation:

```text
wamn_receiving@1.1.0::purchase_order.get
```

Frontend use:

```ts
import { create_client as create_wamn_receiving } from "@wamn/receiving";

const wamn_receiving = create_wamn_receiving(transport);

const result = await wamn_receiving.purchase_order.get([
  {
    request_id: "load_purchase_order",
    id: "po_123",
  },
]);
```

A generated TypeScript/npm package is framework-independent and organized by the same operation convention:

```ts
export interface WamnReceivingClient {
  readonly purchase_order: {
    get(
      input: readonly PurchaseOrderGetInput[],
    ): Promise<readonly OperationItem<PurchaseOrder>[]>;

    query(
      input: readonly PurchaseOrderQueryInput[],
    ): Promise<readonly OperationItem<Page<PurchaseOrder>>[]>;

    create(
      input: readonly PurchaseOrderCreateInput[],
    ): Promise<readonly OperationItem<PurchaseOrder>[]>;

    update(
      input: readonly PurchaseOrderUpdateInput[],
    ): Promise<readonly OperationItem<PurchaseOrder>[]>;

    delete(
      input: readonly PurchaseOrderDeleteInput[],
    ): Promise<readonly OperationItem<DeleteResult>[]>;
  };

  readonly receiving: {
    record_receipt(
      input: readonly ReceivingRecordReceiptInput[],
    ): Promise<readonly OperationItem<ReceivingRecordReceiptResult>[]>;
  };
}
```

Each result item preserves correlation:

```ts
export type OperationItem<T> =
  | { request_id: string; value: T; error?: never }
  | { request_id: string; value?: never; error: OperationError };

export interface Page<T> {
  item: readonly T[];
  next_cursor: string | null;
}
```

A client package that extends a shared platform relation receives a narrower mutation type containing only client-owned fields.

Platform and client packages with the same local operation remain unambiguous through ES import aliases:

```ts
import { create_client as create_wamn_receiving } from "@wamn/receiving";
import { create_client as create_client_acme_receiving } from "@acme/receiving";

const wamn_receiving = create_wamn_receiving(transport);
const client_acme_receiving = create_client_acme_receiving(transport);

await wamn_receiving.receiving.record_receipt([base_command]);
await client_acme_receiving.receiving.record_receipt([client_command]);
```

An effective-release TypeScript/npm facade may pin and re-export the two exact WAMN application package clients:

```ts
import {
  wamn_receiving,
  client_acme_receiving,
} from "@acme/receiving_application";

await client_acme_receiving.quality.approve_inspection([command]);
```

`quality` is an application domain inside `client_acme_receiving`, not another client overlay package. `@acme/receiving_application` is a generated npm distribution artifact for the effective release, not a WAMN application package. Regenerating it does not mutate the underlying immutable WAMN application package clients, component artifacts, or UI artifact.

## 13. Tooling and diagnostics

The normal developer and agent loop is:

```text
wamn diff
wamn check
wamn test
wamn publish
```

`wamn check` performs:

```text
migration and ownership verification
→ PostgreSQL introspection
→ generation
→ SQLx verification
→ component and client compilation
→ operation-contract and reference validation
```

`wamn generate` remains available for explicitly refreshing or inspecting generated output.

### `wamn inspect`

Exposes the effective model in human- and machine-readable form:

```text
exact platform and client application package versions
definition ownership
managed data model and field
canonical PostgreSQL and wire type
default/generated/server-owned field
constraint and relation
CRUD exposure and mutation authority
query and projection
registered operation and consumed contract
route, permission, effect, and connection requirement
schema_state_id and required contracts
migration identity/checksum
source location
```

### `wamn diff`

Explains physical, ownership, and contract impact:

```text
+ client_acme_receiving owns receiving.purchase_order.acme_quality_status
+ CHECK purchase_order_acme_quality_status_check
~ client purchase_order.update adds acme_quality_status
= platform purchase_order.update unchanged
= receiving.load_receipt_screen unaffected
! candidate base migration must preserve the client-owned field
✓ active predecessor compatibility proved
```

Every generated artifact retains a source map. Diagnostics point first to authored migration, named SQL, command/BFF source, UI source, or application metadata—not disposable generated code.

## 14. Effective release contents

An immutable effective release closes over:

```text
exact platform base package and version
exact client overlay package and version
effective schema_state_id
package required schema and platform-policy contracts
derived effective catalog IR
immutable component artifact digests and selected UI artifact digest
exact operation-contract-requirement-to-implementation bindings
routes, wirings, permissions, effects, and connection bindings
platform security overlay
generated effective TypeScript facade
compatibility report
test evidence and build/toolchain provenance
```

The two input application packages and every referenced component or UI artifact remain immutable. A new package combination may generate new effective bindings, security overlay, manifest, compatibility report, and TypeScript/npm facade without changing either package or artifact.

Components may be split by authority, release cadence, and revocation fate. A BFF-only change should not necessarily change the digest containing a critical transaction command.

## 15. POC acceptance

The POC is complete when:

1. Exact `wamn_receiving` and `client_acme_receiving` application package versions produce one exact effective release.
2. The single client overlay may contain several domains and component artifacts without introducing another WAMN application package.
3. The effective release preserves definition-level platform/client ownership.
4. A client adds a client-owned field to an explicitly extensible platform data model without editing platform migration history.
5. Client-generated `purchase_order.update` can mutate only client-owned fields unless the platform explicitly grants more authority.
6. Canonical operation identity includes exact package, version, and local operation.
7. Platform and client application packages may register the same local operation without collision or implicit override.
8. Every registered operation accepts and returns correlated arrays with fixed `per_input` semantics.
9. The complete application SQL corpus is SQLx-verified against the exact effective schema.
10. Operation authorization refuses before CRUD, projection, BFF, or command mutation, including nested registered-operation calls.
11. `receiving.record_receipt` proves one transaction per command item, pessimistic row locking, rollback, business invariants, and idempotency.
12. Generated `purchase_order.update` proves expected revision, atomic revision increment, and `concurrency_conflict`.
13. The platform security overlay is derived, applied, and verified under the runtime role.
14. A committed receipt is observed through the existing CDC/event plane and causes a client-owned post-commit wiring to execute.
15. A generated TypeScript client exposes `package_alias.data_model.action([...])` and `package_alias.domain.custom_action([...])`.
16. A platform base update creates an inactive candidate against the unchanged exact client overlay application package.
17. Upgrade verification refuses a platform change whose direct or dependent effects invalidate a client-owned definition or consumed contract.
18. A compatible base update reuses the immutable client overlay package, component artifacts, and UI artifact and generates only new effective-release artifacts.
19. The client explicitly selects candidate activation; no active effective release is silently replaced.
20. An incompatible candidate remains inactive and identifies the required client overlay application package update.
21. The previous effective release remains a rollback target when its required contracts remain satisfied.

## 16. Explicitly deferred

```text
general_extension_resolver
arbitrary_base_definition_override
runtime_package_resolution
version_range_execution
automatic_client_source_adaptation
extension_dependency_graph_beyond_base_plus_client
transactional_base_command_extension
transaction_hook_or_borrowed_transaction_contract
cross_input_atomicity
schema_yaml_or_json_dsl
stable_logical_field_identity_independent_of_postgresql_name
arbitrary_postgresql_object_model
runtime_pg_catalog_reconciliation
permission_hierarchy_or_field_level_policy_dsl
per_command_database_role
mandatory_per_command_generated_unit_of_work_trait
rust_call_graph_analysis
wac_transaction_resource_composition
transaction_id_crossing_wiring
requires_drain_migration_mode
generalized_deployment_saga_or_concurrent_deployment_control
online_or_resumable_backfill
bulk_or_blob_streaming_contract
platform_ui_plugin_system
destructive_schema_migration
framework_specific_client_hook
monolithic_application_wasm_requirement
```
