# WAMN — Inventory Operations MVP

**Status:** DRAFT v0.10 · 2026-11-27 (revision of v0.9)  
**Basis:** *WAMN Operational Base Schema Catalog* v0.95, inventory subset and amendments in §2 and §9.2; selective seed alignment with *Operational Base Seed Data Catalog* proposed v0.5. No full schema rebase.  
**Platform baseline:** wamn `main` at `03996f014`, read 2026-11-27. No builds or tests were run for this revision.  
**Schedule:** starts as soon as Epic 6 (public addresses) is done.  
**Intent:** The smallest usable inventory application that proves multiline inventory posting, identity creation and redistribution, reconcilable transaction history, bidirectional inventory tracing, and permission-scoped reads. It becomes the repository's example application. Follow-ups extend it with an overlay, a declared connection, and a separately built UI.

## 1. Scope

One WAMN application package, six transactional commands and four authored reads over two authored PostgreSQL views. It operates directly on individual inventory without originating business documents.

**Cut 1:** Schema, package, reference seed data, commands, read operations, generated create and update on the master models, and tests. No UI.

**In:** Opening, multiline movement, absolute quantity adjustment, status change, split, merge, closure on depletion, transaction reasons, transaction history and bounded forward/backward/between trace reads. Item, item revision, location, facility, unit and classification lookups. Create and update of facility, location, item and item revision through generated operations (routes only).

**Out:** Container/composite inventory, containment and reparenting; PO, SO, WO, TO, receipts, fulfillments, builds, business-line attribution and bridges, warehouse/production orders and executions, flows, lots, serial tracking, counting workflows, costing, valuation, interfacility transfers, import tools, and master-data administration UI.

Use the inventory-only subset of the base catalog, not the full operational catalog and not a separate incompatible schema. The schema is not reduced further: it is designed to stay aligned with the larger application schema. No generalized inventory workflow engine, reason-policy DSL, status-transition engine or configurable UI framework.

## 2. Data and invariants

### 2.1 Inventory-only schema (19 application tables)

Three operational tables, six master-data tables and ten lookups (including the new reason lookup). All retained catalog columns and constraints follow v0.95 unless explicitly amended here. Platform-owned relations remain platform-owned and are not counted.

| Model | Purpose and principal fields |
|---|---|
| `inventory` | Current identity: `id`, `composite`, `parent_inventory_id`, `item_id`, `item_revision_id`, `quantity`, `uom_id`, `location_id`, `type_id`, `status_id`, `row_version` |
| `inv_trx` | Transaction header: `id`, `number`, `occurred_at`, `reason_id`, `memo`, common audit fields |
| `inv_trx_line` | Effect: `id`, `inv_trx_id`, `line_number`, `ref`, `inventory_id`, `type_id`, source/destination quantity, location, parent, status, inventory type and item revision |
| `item` | `id`, `number`, `name`, description/reference/weight fields, `uom_type_id`, stock/purchase/sales/consumption UOM defaults, `type_id`, `status_id`, `row_version` |
| `item_revision` | `id`, `item_id`, `revision`, `status_id`, description and optional effective/expiration times, `row_version` |
| `facility` | `id`, `number`, `name`, `status_id`, `row_version` |
| `location` | `id`, `facility_id`, `number`, `name`, `type_id`, `status_id`, `row_version` |
| `unit_of_measure_type` | UUID `id`, unique `code`, `name` |
| `unit_of_measure` | UUID `id`, `uom_type_id`, unique `code`, `name`, `symbol`, `base`, `conversion_rate`, `active` |

**Common audit (stamp columns).** Business and master records carry `created_at`, `created_by`, `updated_at` and `updated_by` as the catalog states; pure lookup records carry none.

- The application's migration declares the four columns as `NOT NULL`, with no default.
- `created_by` and `updated_by` each carry a foreign key to `platform."user" (id)`, named `<table>_<column>_fkey`. This is the catalog's requirement. It depends on the platform change in §9.1a.
- Each model declares `audit_log` in `wamn.k`: the stamped models select the four columns, the lookups select `columns = []`.
- The platform fills the stamps with a generated trigger. Command SQL and generated SQL write no stamp value.
- The audit tables (`<relation>_history`) hold no keys.

**Identifiers.** Master data, transaction headers and lines use UUIDv7 ids.

- Every uuid key is `id uuid PRIMARY KEY DEFAULT uuidv7()` (PostgreSQL 18). This depends on the platform change in §9.1b: on main the build admits only `gen_random_uuid()` as a uuid default.
- Command SQL and seed rows leave `id` out and take the default.
- A generated create takes the `id` from the caller (`crates/codegen/src/sql.rs::create` binds it), so the caller supplies a UUIDv7.

**Excluded columns (not nullable dangling references):**

| Model | Omit until the referenced domain is introduced |
|---|---|
| `facility` | `default_address_id` |
| `inventory` | `bom_revision_id`, `work_order_operation_id` |
| `inv_trx` | `production_execution_id`, `warehouse_execution_id` |
| `inv_trx_line` | `src_bom_revision_id`, `dst_bom_revision_id`, `src_work_order_operation_id`, `dst_work_order_operation_id`, `production_execution_line_id`, `warehouse_execution_line_id`, `receipt_line_id`, `fulfillment_line_id`, `build_line_id` |

Do not install the three inventory/business-line bridges or their dependencies. The deferred relations and columns can be introduced later by additive migrations. Item revisions stay in the MVP, but their use is optional. Containment-related columns remain in the schema for catalog compatibility; the MVP does not populate them or create composite identities. The table definitions must not require a container sequence or any other operational domain.

### 2.2 Lookups and initial data

All ten lookup models use `id integer GENERATED BY DEFAULT AS IDENTITY` as the PK, `code text UNIQUE NOT NULL` and `name text NOT NULL`. They carry no stamp columns. The UOM family is a separate UUID master-data model, not an integer classification lookup.

| Lookup | Additional fields | Initial codes |
|---|---|---|
| `item_type` | `inventory boolean NOT NULL` | `inventory` (Inventory; true), `assembly` (Assembly; true) |
| `item_status` | — | `active`, `inactive` |
| `item_revision_status` | — | `active`, `inactive` |
| `facility_status` | — | `active`, `inactive` |
| `location_type` | — | `storage` |
| `location_status` | — | `active`, `inactive` |
| `inventory_type` | `composite boolean NOT NULL` | `stock` (composite = false) |
| `inventory_status` | `active boolean NOT NULL DEFAULT true` | `active`, `defective`, `closed` |
| `inv_trx_line_type` | — | `create`, `adjust`, `move`, `status`, `split`, `merge` |
| `inv_trx_reason` | `active boolean NOT NULL DEFAULT true` | See §2.3 |

**Item types:** Both `inventory` and `assembly` are inventory-tracked item classifications (`inventory = true` on both) and require a compatible stock UOM. No BOM, assembly posting, or type-dependent command behavior is introduced in Cut 1. All six commands apply equally to both item types.

**Inventory statuses:**

| Code | `active` (financial stock membership) | Meaning |
|---|---|---|
| `active` | true | Positive-balance accountable stock eligible for ordinary stock selection |
| `defective` | true | Positive-balance accountable stock restricted from ordinary stock selection |
| `closed` | false | Fully depleted identity (quantity = 0) |

`inventory_status.active` denotes financial stock membership, not action eligibility (base seed catalog v0.5). The column keeps its name, as the larger schema has it. Ordinary stock selection filters on the status code `active`, not on the column.

Closure is an automatic consequence of depletion, not a manually selectable status. `closed` is terminal in Cut 1: no command may mutate a closed identity. Use `inventory.open` with an appropriate reason for newly discovered stock. Closed rows retain their IDs, item, UOM, location and all history. The quantity/status rule is enforced by the posting commands in the same transaction.

### 2.3 Transaction reasons

`inv_trx_reason` replaces the catalog's free-text reason with a lookup. `inv_trx.reason_id` is a nullable integer FK to `inv_trx_reason.id`. The reason explains **why** a posting occurred; `inv_trx_line.type_id` describes **what** occurred. One reason per transaction header, shared by all lines. `memo` remains available for additional explanation.

| Typical use | Example reason codes |
|---|---|
| Opening | `opening_balance`, `found_inventory` |
| Adjustment | `count_discrepancy`, `damage`, `loss`, `correction` |
| Move | `relocation`, `replenishment`, `consolidation` |
| Status change | `damage`, `inspection`, `correction` |
| Split | `partial_quantity`, `repack` |
| Merge | `consolidation`, `repack` |

Reasons are required for open, adjust and status change, optional for move, split and merge. A supplied reason must exist and be active. Seed each distinct code once. Codes are shared across operations. Deactivate obsolete codes rather than deleting historical references. Reason administration is not part of Cut 1.

### 2.4 Identity, quantities and history

- Every MVP record has a single PK named `id`. `inventory.id` is the sole permanent inventory number, `varchar(64) COLLATE "C"`, allocated through the platform `inventory` sequence. All inventory FKs use the same `varchar(64)` type. Put the collation on the column, never on an index: the catalog reader refuses a key or index whose collation differs from its column's. No redundant inventory `number` column and no generated create on `inventory`. UI label: **Inventory Number**.
- Only two formatted platform sequences: `inventory` with default `INV{seq:n,width=6}`, and `inv_trx` with default `IT{seq:n,width=6}`. The administrator may change patterns for future numbers. No resets, per-type counters, or container sequence. Number allocation is explicit in the posting commands (`sequences::<name>::next`); neither column has generated `number` field metadata. PK/UQ and length checks refuse collisions or invalid rendered values atomically. Gaps are acceptable; numbers do not determine transaction order.
- `inventory.row_version int4 NOT NULL DEFAULT 1` starts at 1 and increments once for every changed existing identity. It is used solely for optimistic concurrency on current inventory. No no-op effects or revision bumps.
- `facility`, `location`, `item` and `item_revision` each carry `row_version int4 NOT NULL DEFAULT 1`. A generated update requires a revision field (`crates/codegen/src/generate/validation.rs::require_mutation_shape`).
- Only `composite = false`, `parent_inventory_id IS NULL` and nonnegative, non-null quantities in the MVP. Keep the catalog's composite FK to `inventory_type(id, composite)`. Item, revision, UOM and inventory type are fixed for the identity; the revision, if supplied, must belong to its item. An inventory item must be tracked and its UOM must belong to its quantity family. Use the item's stock UOM and a single seeded UOM (EA).
- `inventory.quantity`, `inv_trx_line.src_quantity` and `inv_trx_line.dst_quantity` are PostgreSQL `numeric NOT NULL` with no fixed precision or scale, carried as the WIT `numeric` string alias.
  - PostgreSQL `numeric` accepts `NaN` and `Infinity`, and `quantity >= 0` passes both. The check is `quantity >= 0 AND quantity < 'Infinity'`.
  - All quantity arithmetic and comparison is done in SQL. The guest carries no decimal library; it only canonicalizes the input string (`wamn_scalar::canonical_numeric`), which refuses exponents.
  - Fractional quantities are valid even with the seeded EA unit. No implicit rounding.
- All moves stay within the current facility. `inventory.quantity` is the materialized current balance. For each affected identity, `src_quantity` and `dst_quantity` are its before/after balance; their difference is the signed quantity delta. Record before/after status and location and other retained state fields, including automatic closure; state-only effects have equal quantities. For an opening, source quantity is 0 and absent source state fields are NULL.
- Ledger history is append-only through normal application behavior. No update or delete operation on `inv_trx` or `inv_trx_line`; no command modifies existing postings. Both ledger models use `audit_log.retention = "none"`; there is no change feed on them. Do not claim database-role immutability: the application login holds `SELECT, INSERT, UPDATE, DELETE` on every table of the schema by default (`crates/env/src/project_migration.rs`).
- **Business chronology:** `occurred_at` is required caller-supplied business time (`timestamptz`). Backdated and future-dated business times are allowed; posting changes current inventory immediately. `created_at` is system-filled recording time. History and trace presentation default to `occurred_at`, then `created_at`, then transaction ID and line number for deterministic ties. Post against actual current locked state, not a reconstructed past balance. Do not replay stock or change existing effects when a backdated transaction is entered. Trace connectivity follows explicit split/merge effects, not business-time adjacency (§4). Reconcile quantities using the recorded signed deltas; do not promise exact reconstruction of successive historical states.

## 3. Write operations

All operations on inventory share one namespace, `inventory.*`. Each operation's permission is the same text as its name.

| Commands | Reads |
|---|---|
| `inventory.open` | `inventory.get`, `inventory.query` (generated) |
| `inventory.adjust` | `inventory.current` |
| `inventory.move` | `inventory.history` |
| `inventory.status_change` | `inventory.transaction` |
| `inventory.split` | `inventory.trace` |
| `inventory.merge` | |

No command is named `inventory.create`: `<model>.create` is the name and permission of a generated create on every model.

All commands use the claim-idempotent path with `transaction = "explicit_per_input"`. One outer envelope item contains the entire business posting and its lines; configure `envelope = {minimum = 1, maximum = 1}` so one request produces one transaction header. A successful posting commits one `inv_trx`, all its effects and all inventory changes in one PostgreSQL transaction. No partial success.

| Command | Input (business fields) | Effect and rules |
|---|---|---|
| `inventory.open` | Per line: item ID, optional item revision ID, location ID, type ID, initial status ID, positive initial quantity; header reason required | Allocate one new inventory number per line, derive stock UOM, insert revision 1, write `0 → quantity` effect. Only active or defective initial status. Opening balances are one use case. |
| `inventory.adjust` | Per line: inventory ID, expected revision, absolute target quantity; header reason required | Reject negative targets; write current → target. Target zero closes the identity. Positive quantity preserves status; closed identities cannot be adjusted. |
| `inventory.move` | Per line: inventory ID, expected revision, destination location ID | Move the entire positive-balance identity; conserve quantity. Destination must belong to the same facility. |
| `inventory.status_change` | Per line: inventory ID, expected revision, destination status ID; header reason required | Active ↔ defective for positive-balance stock. No manual closure or reopening. Quantity unchanged. |
| `inventory.split` | One source ID, expected revision, positive split quantity | Require `0 < split_quantity < source.quantity`. Reduce the source and create one new identity inheriting item, revision, UOM, location, status and type. Conserve total quantity. |
| `inventory.merge` | `dst_inventory_id` and `line[]` containing two or more distinct inventory IDs, each with its expected revision, including the destination | All participants have positive balances and the same item, revision, UOM, location, facility, status and type. The destination receives all source balances; sources become zero and closed. Conserve total quantity. |

All commands require `occurred_at` and accept optional `memo`. Reason rules are in §2.3. `closed` is terminal; any command targeting an existing closed identity refuses. Defective stock remains visible and may be explicitly selected for authorized operations on positive balances.

**Master-data eligibility:** Open requires an active item of a tracked type, an active item revision if supplied, an active facility and location, and an active stock UOM. Move requires an active destination location within the existing facility (the source may be inactive). Adjust and status change are not blocked by inactive master data; split inherits the existing classifications, and merge checks existing compatibility. Revision ownership, UOM compatibility and all other structural invariants still apply.

**Merge input:** The destination is a selector into the same participant array, not a second independently guarded input.

```json
{
  "dst_inventory_id": "INV001",
  "line": [
    {"inventory_id": "INV001", "expected_row_version": 3},
    {"inventory_id": "INV003", "expected_row_version": 2}
  ]
}
```

The destination must occur exactly once in `line[]`. Lock and revision-check it alongside every source.

**Examples** (EA; simplified numbers):

| Command | Before → after | Ledger effects |
|---|---|---|
| Open | New `INV001` = 10; new `INV002` = 5 | Two effects, `0→10` and `0→5`, one header, e.g. `opening_balance` |
| Multiline move | `INV001` A01→B01 (10); `INV002` A02→B01 (5) | Two location changes, `10→10` and `5→5` |
| Multiline adjust | `INV001` 10→8; `INV002` 5→0 | Two effects, second also `active→closed`, e.g. `count_discrepancy` |
| Status change | `INV001` active→defective (8) | One effect, `8→8`, e.g. `damage` |
| Split | `INV001` 8→6; new `INV003` 0→2 | Two effects with shared `ref`, net zero |
| Merge | `INV001` 6→8; `INV003` 2→0 | Two effects with shared `ref`, net zero; `INV003` becomes closed |

**Posting contract:** Validate permission, input and reason; refuse duplicate participant IDs within the posting. For existing identities, lock in ascending `id` order (the column is `COLLATE "C"`), re-read authoritative state, compare all supplied expected revisions (including unchanged lines), and validate eligibility and conservation before omitting no-ops. Refuse an ineligible line even when unchanged. Omit eligible no-op move/adjust/status lines; refuse an all-no-op posting (`invalid_input`). Allocate the header and any new inventory numbers only for a nonempty posting. No empty header is written. On a stale revision nothing commits. Return the transaction ID/number and only changed or newly created inventory identities with their updated state and revisions. Same claim key and same intent return the original result; changed intent under the same key is refused. Use the platform's claim path, not an application idempotency table. No network call while the database transaction is open.

**Authoring:** Author input, result and typed business errors in WIT. `wamn.k` holds command metadata, labels, references, limits, revision guards, the statement list and idempotence.

```kcl
input: {
    envelope = {minimum = 1, maximum = 1}
    line = {minimum = 1, maximum = 100}
    revision_guards = {"line[].expected_row_version" = "line[].inventory_id"}
    labels = {"line[].inventory_id" = {references = {model = "inventory"}}}
}
```

- `line` bounds the line count; `inventory.merge` uses `minimum = 2`. `raw_body_maximum` bounds the body.
- A list of records in WIT flattens to `line[].<field>` paths, and a guard is matched by path (`crates/codegen/src/authored_wit.rs::read_input`), so the array-element guard compiles. No package on main uses one yet; the first multiline command is its first user.
- The stale-revision error is an authored WIT error case. An error payload may hold string fields plus `expected-row-version` and `observed-row-version` (`authored_wit.rs::read_errors`), so the case carries the inventory `id` of the stale line.
- Declare the merge destination selector as a text-key inventory reference.

**Effect representation:** One effect per affected identity, not per changed field. `inv_trx_line.type_id` classifies the operation (the opening effect keeps the line type code `create`). `line_number` is unique within the transaction, not temporal order. `ref` is the fixed transaction-local text value `"1"` on every split/merge effect and NULL on other effects. Positive and negative deltas determine cross-identity direction in the split/merge group. Retain zero-balance identities and their history.

### 3.1 Master-data operations

`facility`, `location`, `item` and `item_revision` each have generated `get`, `query`, `create` and `update`. No delete. `unit_of_measure_type`, `unit_of_measure` and the ten lookups have generated `get` and `query` only.

- Create takes the caller's UUIDv7 `id` and the model's business fields.
- Update uses `revision_field = "row_version"`. Its writable fields are names, descriptions and status. Not writable, because existing stock depends on them: `item` type and unit fields, `location.facility_id`, `item_revision.item_id`.
- Their permissions are `facility.create`, `item.update` and so on. Grant them only to the role that maintains master data.

## 4. Read model: two authored views

Use ordinary read-only PostgreSQL views in the application migration; do not create materialized copies of the ledger. Views are not permissions or isolation boundaries: the route authorizes the operation, and the read login can select every table of the schema.

| View | Grain / content |
|---|---|
| `inventory_current` | One row per inventory identity, item number/name/revision, facility/location, quantity/UOM, type, status, available flag, revision and underlying IDs |
| `inventory_ledger` | One row per transaction effect, joined to the header and lookup labels: IDs, transaction number/time/reason/memo, action, line number, `ref`, source/destination quantities and state, business and recording timestamps |

| Authored read | Purpose |
|---|---|
| `inventory.current` | Current joined inventory list or exact inventory-number lookup; filter by item/facility/location/status, page deterministically |
| `inventory.history` | One identity's paginated ledger, default business-time order |
| `inventory.transaction` | All effects for one transaction ID, ordered by `line_number`, with header metadata |
| `inventory.trace` | Bounded forward, backward or between-identity traversal over the ledger |

Expose generated `inventory.get` and `inventory.query` as deliberate primitive reads (also the live text-key consumer). Keep the ledger models without generated operations so raw history reads do not bypass the history, transaction and trace permissions. No optional balance-summary view in Cut 1.

**Result shapes** (`authored_wit.rs::read_result`):

- A list read has a `row` type, and its result record holds exactly one field: `rows`. Paging is a `cursor` field on each row and an `after-cursor` input, as `receiving.load_purchase_order_history` does. Stable keyset paging, not offset.
- `inventory.trace` returns one record with no `row` type: its identities, effects and edges as nested lists, and the `truncated` flag. Three nested lists in one result are not used on main yet; verify in A6.
- Transaction detail bounds follow the write command's line limit.

Current and history reads include closed and defective records when requested.

**Trace:** A normalized result contains inventory identities, their recorded transaction effects and directed lineage edges. Only effects classified as split or merge and sharing a non-NULL `(inv_trx_id, ref)` establish cross-identity edges, directed from negative-delta sources to the positive-delta destination. Identify each edge by its contributing effects and transaction. Never infer an identity-to-identity edge because lines share an `inv_trx_id` or appear adjacent in a business-time sort. Include open, adjust, move, status change, split and merge effects, including depletion and zero-balance identities. The trace reports identity-level lineage and recorded changes, not an exactly ordered chain of historical states.

A bounded authored handler may use SQL or bounded single-step queries over `inventory_ledger`. Do not add a graph database, persistent lineage table or general-purpose traversal framework. Return results in table-renderable form, sorted by business time, with both timestamps. Bound depth, work and result size; set the default and maximum bounds in A6. Deduplicate identities for traversal and effects by effect ID. Return `truncated`: false when the requested traversal completed, true when a bound prevented completion. An empty between-query result with `truncated = false` establishes no connecting path; with `truncated = true` it does not.

## 5. UI (deferred)

Cut 1 has no UI. When built, use the independently built web artifact path (`crates/release/src/web_artifact.rs`, `apps/<app>/web`), generated typed clients, and existing table/form components. A single workbench provides a searchable/scannable inventory table, item/facility/location/status filters, quantities, UOM, status and multi-selection. Actions: **Open · Adjust · Move · Status · Split · Merge**.

- Open: select item, optional revision, location, type, initial status and quantity per line; select reason and post once.
- Multiline actions: common destination location/status with per-line override, absolute target quantities, reason selection as required, before/after preview and expected revisions. Display automatic closure on zero balance.
- Split: one identity, split quantity, resulting identities. Merge: compatible selected identities, explicit destination inventory, preview closed sources.
- Detail: authoritative current state, history, transaction details and Trace tab. Closed records remain searchable. The trace first renders a chronological table; a later graph uses the same result.

Use permission-aware action visibility, but revalidate all rules in the backend. Keyboard-wedge scanning into the inventory-number search is sufficient.

## 6. Permissions and seed data

Each operation's permission is exactly its name: the six commands, the four authored reads, `inventory.get`, `inventory.query`, and the generated operations of the master models and lookups. No separate permission alias, per-view role or generic write permission.

**Seeded by the migration** (in every environment): the ten lookups, `unit_of_measure_type` and `unit_of_measure`.

- There is no package seed mechanism on main. Seed rows are `INSERT` statements in the application migration.
- Lookup rows are plain `(code, name)` plus their own extra fields. Omit the integer `id`; everything resolves a lookup by its stable `code`. Unit rows omit their uuid `id` and take the `uuidv7()` default.
- The stamp trigger is installed by the generated history migration, which is numbered after the application's own. A seed row in a stamped table is therefore inserted before the trigger exists and must write its own stamp values:
  ```sql
  created_at = transaction_timestamp(), created_by = wamn_authority.current_principal_id(),
  updated_at = transaction_timestamp(), updated_by = wamn_authority.current_principal_id()
  ```
  In a migration that principal is `wamn:apply-package`, which has a `platform."user"` row. Verify in A2.

| Seeded table | Records |
|---|---|
| `unit_of_measure_type` | `quantity` |
| `unit_of_measure` | EA — Each; base = true, conversion_rate = 1 |

**Not seeded:** facility, location, item and item revision. They are created through the generated create routes, by the tests and by a post-install script for user acceptance.

| Example data for tests and the script | Records |
|---|---|
| `facility` | WH01 — Main Warehouse |
| `location` | A01, A02, B01 in WH01 |
| `item` | ITEM001 — inventory item (`item_type.inventory`); ITEM002 — assembly item (`item_type.assembly`); both tracked, active, stock UOM EA |
| `item_revision` | Revision A for ITEM001 |

The post-install script calls the create routes over HTTP with an access token, then posts opening stock with `inventory.open` and reason `opening_balance`. No direct inventory balance seed and no transaction-header seed. Tests resolve generated inventory numbers and lookup IDs from actual results.

## 7. Acceptance tests

**How the command rules are tested is deferred by the owner.** The fact to decide on: `tests/receiving-tests` runs the application's SQL files against PostgreSQL and does not run the component; a rule written in guest code runs only in a deployed host, and the host runs only on Kubernetes. The list below states what must hold, not the test level.

1. Multiline open records distinct generated identities, revision 1, one header and all effects; invalidity on any line rolls back the posting. Exercise both `inventory` and `assembly` item types.
2. Multiline adjust, move and status change post exactly one header per outer item, correct before/after states and effects, and reject invalid lines atomically. A mixed changed/no-op request returns only changed identities. All-no-op requests refuse without a header. An ineligible or stale no-op line refuses the entire request. Reject requests with more than one outer item.
3. Adjustment to zero and merge depletion set `closed` in the same effect. Defective positive stock is not closed. Closed stock stays queryable, and every command targeting it refuses.
4. Split and merge conserve balances exactly, including fractional quantities, and preserve group membership and direction. Refuse invalid, negative, `NaN` and `Infinity` quantities and incompatible participants. The merge destination is in the array exactly once and stays open; all sources close. `ref = "1"` on split/merge effects and NULL otherwise.
5. Concurrent overlapping writes cannot lose updates; a stale revision on any line refuses the entire posting and names the stale identity. Same-key replay returns the original result with no duplicate header or identity; changed intent refuses.
6. Reasons are required for open, adjust and status change; missing, unknown and inactive supplied reasons refuse. Optional reasons on other commands work.
7. Current quantities reconcile with the sum of recorded signed deltas per identity. Each change records its actual before/after quantity, status and location and updates the current row and its revision atomically. Do not assert that the application login lacks update/delete grants.
8. The four authored reads resolve labels and IDs, filter and page stably, and authorize callers independently. Generated text-ID get and query work. Direct denied write calls make no changes.
9. Trace includes every posting type, supports forward/backward/between paths, never links unrelated multiline effects, and handles repeated split/merge cycles and closed identities. An incomplete search sets `truncated = true`; a complete disconnected between-query returns an empty result with `truncated = false`.
10. A backdated effect posted after a later business-time posting displays by `occurred_at` with deterministic ties, keeps `created_at`, and applies to the actual current state. Missing `occurred_at` refuses; a future-dated posting changes current stock immediately.
11. Both sequences render from their current patterns and produce unique identifiers; a collision or invalid inventory number refuses the posting.
12. A fresh migration creates the exact inventory subset with valid FKs, including the stamp keys to `platform."user"`, and no dependency on excluded domains.
13. Inactive master-data rows prove the open and destination-move eligibility rules; adjust, status change, split and merge are not refused solely because reference data is inactive.
14. Master-data create and update work through their routes; update refuses a stale `row_version` and a non-writable field.

## 8. Deliberately deferred

- Containment, container numbering, business document/execution attribution and other excluded domains.
- Final inventory storage UOM choice, conversion workflows, optional indexing beyond named access paths, bulk imports and master-data administration UI.
- Strong database immutability, reopening closed inventory, generalized correction, status-transition and reason-to-operation policies, revision effective-date enforcement and deactivation cascades.
- Exact mutation/commit ordering, historical state reconstruction and allocation of individual units across mixed ancestry.
- UI and the interactive trace graph.
- The test level for the command rules (§7).

## 9. Implementation plan

### 9.1 Platform prerequisite: text primary keys

`crates/codegen/src/key.rs::key_type` admits uuid, int4 and int8 keys only, and `apps/crates/guest/src/data_access/cursor/id.rs` has no `String` id. Every generated operation calls `key_type`, so generated `inventory.get` and `inventory.query` fail to build today. The same agent does this change first, then the application.

| Work | Requirement |
|---|---|
| P1 — Key mapping | Admit a `text`/`varchar` single-column `id` as a text key. A generated create on a text-key model refuses. The numbered-key refusal stays. |
| P2 — Generated data access | A text key round-trips through get, query, update and delete, SQL bind and not-found paths with no UUID parse or numeric conversion. Keyset comparison is done in PostgreSQL with the column's own collation. The uuid branches are in `key.rs` and `generate/{data,adapter,component}.rs`. |
| P3 — Contracts and clients | Text maps to string in WIT, Rust, TypeScript and the API schema. Text-key references and selectors are recognized. Revision guards do not assume UUID ids. |
| P4 — Cursor | A string key encodes and decodes without loss; keyset pages over text IDs with punctuation, digits and tied sort values return every row once, in both directions. |
| P5 — Tests and docs | Generator tests for text-key get, query, update, delete and the create refusal. Cursor round trip. Generated get and query on this package are the live consumer. Document key support and that a text-key model's rows are created by commands, never by a client-generated id. |

Cost: a generator change rebuilds the control and CLI images, which link it. The host does not link `wamn-codegen`. The guest crate change rebuilds every application component. No host image rebuild is expected (read from the manifests, not built).

### 9.1a Platform prerequisite: stamp keys

The catalog requires `created_by` and `updated_by` to reference `platform."user" (id)`. On main no stamp column has a key, and `crates/codegen/src/catalog.rs::platform_reference_is_permitted` admits application keys only to `platform.external_system` and `platform.file`. The owner ruled on 2026-11-27 that the keys are added (project database to `platform."user"`, system database to `identity.principal`, `deleted_by` included, audit tables excluded). That work is handed over separately and must be on main before slice A1.

### 9.1b Platform prerequisite: `uuidv7()` defaults

The build admits two default functions by name, `gen_random_uuid()` and `CURRENT_TIMESTAMP` (`crates/schema/src/ir.rs::POSTGRES_SERVER_EXPRESSIONS`); a column with `DEFAULT uuidv7()` refuses. The owner ruled on 2026-11-27 that `uuidv7()` is admitted and that the platform's own tables and the existing applications switch from `gen_random_uuid()` to `uuidv7()`. Only `wamn-codegen` links the crate that holds the list, so the control and CLI images rebuild and the host does not. The work must be on main before slice A1.

### 9.2 Catalog amendments and subset

- Apply the 19-table subset and omission list in §2.1.
- All integer type/status lookup PK columns become `id` (referencing FK field names unchanged). Preserve `UQ (id, composite)` and the matching composite FK on inventory.
- Add `inventory.row_version int4 NOT NULL DEFAULT 1`. Add `row_version int4 NOT NULL DEFAULT 1` to `facility`, `location`, `item` and `item_revision` for their generated update. Keep `UQ (inv_trx_id, line_number)` and the access path `inv_trx_line(inventory_id)`.
- Keep `inventory.id varchar(64) COLLATE "C"` and all corresponding FKs as `varchar(64)`. Quantities are `numeric NOT NULL` with the check in §2.4.
- Add `inv_trx_reason` and replace `inv_trx.reason_code` with `reason_id`. Add terminal `closed` status and the depletion behavior. Use the two item types (`inventory`, `assembly`). `inventory_status.active` means financial stock membership.

### 9.3 Application package and sequence of work

| Slice | Deliverable |
|---|---|
| A1 — Schema and build | `apps/wamn_inventory`, package `wamn_inventory`, schema `inv` (`inventory` is the fixture's schema); `wamn.k`, migration, authored WIT, operation code, SQL. Verify fresh migration, catalog read and generation. |
| A2 — Seed and reads | Migration seed rows, two sequence declarations, lookup and unit reads, master-data get/query/create/update, generated inventory get/query and `inventory.current`. |
| A3 — Open | First end-to-end posting: numbered text inventory keys, explicit transaction, reasons, stamps, claim replay and ledger reconciliation. |
| A4 — Adjust, move, status change | Revision guards on multiline inputs, atomic refusal, terminal closure, eligibility-before-no-op, changed-only results and overlapping writes. |
| A5 — Split and merge | Exact decimal balances, identity and history preserved, depleted sources closed; destination selection, grouping and fractional quantities. |
| A6 — History, transaction, trace | Shared ledger view, separately permissioned reads, split/merge lineage, backdated display and bounded traversal with the truncation contract. |
| A7 — Acceptance | §7 against the application, at the test level the owner rules; the post-install script. |

Keep each slice working end to end before the next. Do not add infrastructure for later features.

### 9.4 Facts on main that this spec relies on

| Concern | Fact |
|---|---|
| Authoring | Custom input, result and errors are written in WIT; `wamn.k` holds metadata (`crates/codegen/kcl/manifest/manifest.k`, `apps/wamn_receiving/wamn.k`). |
| Operation names | A generated operation is `<model>.<action>` for `get`, `query`, `create`, `update`, `delete`. A custom operation may share a model's namespace (`widget.archive` in `apps/platform_fixture`). A repeated name refuses (`crates/package/src/manifest.rs::validate_operation_vocabulary`). |
| Numbering | A numbered field is filled only by a generated insert; a command calls the sequence itself (`crates/codegen/src/generate/numbering.rs`). |
| Record history | The application declares the stamp columns; the platform generates the stamp and log triggers into `apps/<app>/history/`, numbered after the application's migrations (`crates/codegen/src/history.rs::migrations`). |
| Grants | The application login has full DML on every table of the schema by default (`crates/env/src/project_migration.rs`). Omitting an operation does not revoke database access. |
| Column defaults | Admitted on main: `gen_random_uuid()`, `CURRENT_TIMESTAMP`, or a literal of the column's type (`crates/schema/src/ir.rs::postgres_default`). `uuidv7()` is added by §9.1b. |
| Seed | No package seed mechanism. Migrations are plain SQL with no statement policy. |
| Tests | `tests/receiving-tests` runs application SQL against PostgreSQL. Routes, claims and permissions are exercised only by the journeys on kind (`tests/journeys`). |
| Deployment | The CLI and `wamn dev` take the application source (`services/cli/src/app_verbs.rs`, `services/cli/src/dev/command.rs`). No application is selected by name. |
| Build cost | The Dockerfile copies `apps/` into the native build stage, so any change under `apps/` re-runs that stage. |

## 10. Follow-ups after Cut 1

| Follow-up | Content |
|---|---|
| Overlay | One client overlay pinning the base by version and digest. Choose an additive field and prove a real client rule through the supported composition boundary. |
| Connection | Declare one typed connection requirement and bind it in the environment, e.g. a label print after a successful posting or a scale read before posting. No network call inside the database transaction. |
| UI | Independently built application UI consuming generated contracts; graph visualization only when suitable components exist. |
| Deployment | Install this application through the current deployment commands. |
| Additional domains | Add the omitted catalog models and columns only as an actual application needs them. |
| Retire old examples | Owner decision; do not remove other applications in this cut. |

**Review notes:** v0.10 applies the owner rulings of 2026-11-27 and corrects v0.8 against main `03996f014`. Rulings: no schema cuts; uuid keys default to `uuidv7()`, on the platform's tables and existing applications as well; `row_version` on the four master models; one operation namespace with `inventory.open` and `inventory.status_change`; stamp columns keyed to `platform."user"`, audit tables not keyed; lookups and units seeded by the migration, facility, location, item and item revision created by tests or a post-install script; generated create and update on those four models; the `inventory_status.active` column keeps its name; the application starts after Epic 6; the test level is deferred. Corrections: no package seed mechanism exists; no manifest declaration of platform relations exists; `uuidv7()` is not an admitted column default on main; a generated update needs a revision field; `numeric` admits `NaN` and `Infinity`; list reads page by a cursor on each row; `tests/receiving-tests` does not run the component. Seed changes suggested elsewhere and not adopted (revision lifecycle vocabulary, additional location types, `on_hold`, action-specific flags, effect-code renaming, KG) remain pending review.
