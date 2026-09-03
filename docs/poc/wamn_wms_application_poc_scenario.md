# WAMN WMS — POC Scenario (app 1 of the portfolio)

Status: **RULED AND APPROVED TO BUILD, 2026-09-03.** All five §10 questions
answered; the answers are folded in below and §10 is kept as the record of what
was asked and decided. Structure
follows `wamn_receiving_layered_application_poc_scenario.md`. Scope is fixed by
`poc-application-portfolio.md` row 1 and its build-order exit gate; this
document does not widen it.

## 0. Why this app, and what it is for

WMS is the portfolio's first application and its build-order gate reads:

> a non-technical wiring composing `inventory.move` with a label-render palette
> node, gated, published, routed; contention test (two concurrent moves on one
> pallet) yields exactly one `concurrency_conflict`.

It is chosen first because it exercises, on real content, the five things
Receiving proved only structurally: multi-row transactional commands under
contention, optimistic concurrency at the row that actually contends, a
sort/filter vocabulary at real breadth, a PURE TRANSFORM palette node composed
with package operations in a wiring, and the blobstore seam carrying a rendered
artifact.

It is also the SECOND APPLICATION the toolkit-promotion rule waits on. Nothing
here should be generalized into shared machinery on the strength of this app
alone; where a shape looks reusable, this document says so and stops.

**Everything below is authored content over frozen machinery.** No platform
capability is added. If a section appears to require one, that is a finding for
the owner, not a licence to build it.

## 1. What already exists, and what this app must not rebuild

Landed and reused as-is:

| Prerequisite | Where | Used for |
|---|---|---|
| Multi-package release, effective-release weld | slice iii/iv | one release carrying WMS beside Receiving |
| ACL union from declared fields | generator | WMS's own read/write surface |
| Simulator framework + BOTH WMS profiles | `test-support/simulator` | `scan_event` and `seed_inventory` already authored, stream-proven only |
| `label-render` + `label-template` | `components/no-std` | the pure transform node in the wiring |
| `blob-put` | `components/execution` | putting the rendered label in the object store |
| `wasmcloud:blobstore@0.1.0` capability | `wamn_blobstore` plugin | the seam, now complete for released deployments |
| Client-contract IR, Rust emitter, `wamn-client`, TUI primitives | slice v | an operator surface, if one is wanted |

**RULED — no WMS operator TUI.** The gate names a wiring and a route, not a
screen. Slice v's primitives would make one cheap, and "cheap" is not a reason.

## 2. Simplest WMS base

```text
seed products and locations
→ scan a pallet at a location
→ move / adjust / merge / split its quantity
→ render and store a label for the affected pallet
→ read quantity aggregates by status, product and location
```

### Base data model

```text
product
location            (reused shape, NOT the Receiving table — see open question 2)
pallet
pallet_quantity
inventory_movement
```

Codes follow the shapes the landed simulator profiles already emit: `PAL-000000`,
`LOC-0000`, `SKU-00000`. Status is `available` | `held`, likewise from the
profiles.

`pallet` is the contended row: a move locks it and carries `row_version`.
`pallet_quantity` is the per-status quantity, so a move that changes status is
a two-row write inside one transaction — which is what makes the contention
test meaningful rather than a single-row update in disguise.

`inventory_movement` is the append-only history every command writes, and the
source the aggregates read.

**RULED — WMS owns `wms.location` and `wms.product`.** Sharing a table across
packages is a layering question app 3 (traceability) introduces DELIBERATELY,
not one this app backs into as a side effect. The portfolio's "shared across
apps as base package data" is corrected accordingly: **fixtures are SHAPES,
not tables.** Every app authors its own reference tables; what they share is
the code vocabulary the simulator emits.

### Base application operations

```text
pallet.get
pallet.query
inventory.move
inventory.adjust
inventory.merge
inventory.split
inventory.aggregate
```

`pallet.get` and `pallet.query` are generated CRUD. The four `inventory.*`
commands are custom operations of kind `command`. `inventory.aggregate` is a
`projection`.

Every operation uses the array-shaped invocation contract with `per_input`
semantics, exactly as Receiving does. Cross-input atomicity remains deferred.

### The four commands, and why they are four and not one

| Operation | Rows touched | Why it is its own operation |
|---|---|---|
| `inventory.move` | one pallet, one or two `pallet_quantity` | changes LOCATION; the contended one |
| `inventory.adjust` | one pallet, one `pallet_quantity` | changes QUANTITY with a reason code; an audit event a move is not |
| `inventory.merge` | two pallets, one survives | consumes a pallet; the only operation that ENDS a pallet's life |
| `inventory.split` | one pallet in, two out | creates a pallet; the only operation that BEGINS one outside receipt |

A single `inventory.change` taking a discriminated payload would collapse four
different authority questions into one grant. Merge and split in particular
create and destroy pallet identity, and a caller permitted to move stock is not
thereby permitted to make a pallet disappear.

## 3. Optimistic concurrency, and where it actually bites

Every command takes `expected_row_version` for the pallet it acts on, and the
transaction is:

```text
authorize caller
→ begin transaction
→ enforce idempotency on the caller-supplied key
→ lock the pallet row FOR UPDATE
→ compare expected_row_version to observed
→ validate the movement against the current quantity and status
→ write inventory_movement
→ update pallet_quantity
→ bump pallet.row_version
→ commit
```

The refusal carries BOTH revisions, per the client-contract detail matrix
already proven in slice v: `expected_row_version` and `observed_row_version`.

**The contention gate is the reason the lock is on `pallet` and not on
`pallet_quantity`.** Two concurrent moves of the same pallet to different
locations must not both succeed by touching different quantity rows. Locking
the pallet makes the pallet the serialization point, which is what the gate
measures.

## 4. Sort and filter vocabulary at real breadth

Receiving's `purchase_order.query` filters on two fields and sorts on three.
`pallet.query` is where the vocabulary is exercised properly:

- filters: `status`, `product_id`, `location_id`, `pallet_code`
- sort: `pallet_code`, `location_id`, `updated_at`, `quantity`
- keyset pagination on `created_at` ascending with an `id` tie-breaker

**Open question 3 — sorting on `quantity` crosses a table.** Quantity lives in
`pallet_quantity`, not `pallet`. A keyset sort over a joined column needs the
sort key in the tie-broken ordering, and the generator's authored-SQL variant
mechanism has only ever sorted own-table columns. **RULED — dropped.** A
join-crossing sort is a generator expansion in its own right, not a WMS side
quest.

Note also the constraint measured in `wamn-10yt.5.6`: a model `query` must be
`ResultClass::Page`, and `Page` mandates a keyset cursor on `created_at ASC`
with an `id` tie-breaker. `pallet` therefore needs `created_at`, which is
natural for it — unlike `location`, where that requirement forced a bounded
list.

## 5. The wiring — the low-code proof

This is the gate's centre and the reason the app exists:

```text
inventory.move  →  label-render  →  blob-put
```

- `inventory.move` is a package operation with an effect posture (postgres).
- `label-render` is the PURE TRANSFORM: no imports beyond `wamn:node`, so its
  gate cases are effect-free and its output is golden-tested per template.
- `blob-put` carries the blobstore effect and writes the rendered ZPL under a
  caller-supplied deterministic key.

The wiring is authored, not coded. `template_id` rides the wiring's parameter
schema as a typed enum (ruling of 2026-09-02), so the wiring declares honestly
which label it renders and a gate case can pin golden output per wiring.

**The at-least-once rule holds end to end:** `blob-put` never generates keys.
The key is derived from the movement identity, so a redelivered wiring
execution overwrites the same object rather than creating a second label.

**Open question 4 — what is the label's deterministic key?** The obvious
choice is the `inventory_movement.id`, which is server-generated inside the
command's transaction — so the wiring's later node needs a value the first node
produced. That is a normal wiring data flow, but it means the key is only
knowable after the command commits, and a retry of the WHOLE wiring re-runs the
command under the same idempotency key and must observe the same movement id.
**RULED — already law, and the schema is how it is kept.** Idempotent replay
returns the IMMUTABLE ORIGINAL RESULT (the `record_receipt` ruling), so
`inventory_movement.id` is stable under retry BY CONTRACT rather than by
convention. Receiving implements it in the schema and WMS copies the shape
exactly: `inventory_move_command` is keyed by `idempotency_key` and holds
`movement_id uuid NOT NULL DEFAULT gen_random_uuid()` as a UNIQUE column, so a
replay finds the claim row and returns the id it already generated. A command
returning a FRESH id on replay is a defect in that command, never a reason to
choose a different key.

The contention gate asserts it: a replayed command returns the same movement
id and the label object count stays one.

## 6. Aggregates

`inventory.aggregate` is a projection returning quantity totals grouped by
status, product and location. It is a single screen-shaped read, `bounded_list`,
with `get`-style refusals.

It is deliberately NOT a paged query: an aggregate over a warehouse is small
relative to its movement history, and paging it would invite a caller to
reconstruct a total from pages that moved underneath them.

## 7. The simulator profiles — already written, waiting for this app

`wamn-simulator` ALREADY ships both profiles this app needs. Its own module doc
says why they are only half-proven:

> Both describe WMS traffic, and the WMS package does not exist yet
> (`packages/` holds only `receiving` and `client_acme_receiving`), so today
> they are provable as *streams* — the determinism gate — rather than end to
> end.

**WMS is the app that finishes them.** Nothing new is written here; the
profiles stop being stream-only and start driving real routes.

Their shapes are therefore a CONSTRAINT on this app's model, not a free
choice — the vocabulary is already authored:

| Profile | Fields | What it pins |
|---|---|---|
| `scan_event` | `pallet_id` (`PAL-000000`), `location_id` (`LOC-0000`), `scanner_id`, `scan_sequence` | pallet and location code shapes |
| `seed_inventory` | `product_id` (`SKU-00000`), `location_id`, `quantity`, `status` | product code shape; status vocabulary `available` \| `held` |

The status vocabulary above is the one the aggregates group by. A WMS model
that invented a different one would leave a landed fixture unable to drive it.

**The simulator drives the real routes.** It never seeds the database. Seeding
happens through `inventory.adjust` against a real route, so the fixture is
built by the same authority a caller would use.

## 8. Exit gates

Exactly the portfolio's, restated as measurements:

1. **The composed wiring.** `inventory.move → label-render → blob-put`
   authored, gated (label-render's cases effect-free, blob-put carrying effect
   posture), published, routed. One move through the route leaves one label in
   the object store, and the span shows both nodes with the originating caller.
2. **The contention test.** Two concurrent moves on one pallet yield EXACTLY
   ONE `concurrency_conflict`. Not "at least one": two conflicts would mean
   neither move succeeded, and zero would mean the lock is not the pallet.
   The refusal carries both revisions.
3. **Idempotent redelivery.** The scan profile at a measured duplicate rate
   produces one movement per logical scan, and one label object per movement.
4. **The vocabulary.** `pallet.query` filters and sorts across the declared
   breadth, cursor-paged, with an opaque cursor round-tripping unchanged.

## 9. Explicitly out of scope

```text
cross_package_shared_reference_data     (app 3 introduces it deliberately)
wms_operator_tui                        (open question 1 — recommend no)
sorting_on_a_joined_column              (open question 3 — recommend drop)
label_template_authoring                (three templates, demand-gated)
cycle_count / putaway_strategy / wave_picking
```

## 10. What was asked, and what was ruled

Kept as the record. All five were decided 2026-09-03.

| # | Question | Ruling |
|---|---|---|
| 1 | Operator TUI in this app? | **No.** The gate names a wiring and a route. |
| 2 | Own `location`/`product`, or share Receiving's? | **Own.** Cross-package sharing is app 3's deliberate introduction. |
| 3 | Sort on `quantity` across the join? | **Dropped.** A join-crossing sort is a generator expansion, not a WMS side quest. |
| 4 | Does idempotent replay return the ORIGINAL movement id? | **Yes, by contract** — the immutable-original-result law, kept by the command table's UNIQUE pre-generated id. Asserted in the contention gate. |
| 5 | Aggregates: projection or event-sourced rollups? | **Projection.** Rollups are app 2's problem; building them here is app-2 machinery inside app 1. |

Three of the five were derivable from existing law rather than novel forks —
Q4 from the idempotency ruling, and Q1/Q3/Q5 from R-C. Recorded so the next
scenario escalates less.
