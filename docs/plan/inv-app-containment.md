# WAMN — Inventory containment overlay

**Status:** DRAFT v0.1 · 2026-11-27  
**Base:** package `wamn_inventory`, spec [inventory-app](inventory-app.md) v0.11.  
**Platform baseline:** wamn `main` at `03996f014`, read 2026-11-27. No builds or tests were run.  
**Schedule:** not scheduled. It needs the inventory app and the platform change in §4 first.  
**Intent:** The overlay follow-up of the example application. The overlay and its base make a new application, and this document is its spec. The base application and its spec do not change for the overlay.

Package and application names are not decided.

## 1. What the overlay does

- A location type can be a containment type.
- Stock that lands in a containment location takes the status `containment`.
- The base commands refuse stock in `containment`: no move, adjust, status change, split or merge.
- Quality control (QC) decides what happens to that stock. `qc.release` makes it `active`. `qc.fail` makes it `defective`.

What it exercises on the platform:

- Overlay columns on base tables.
- Overlay rows in base lookups.
- New commands.
- Overlay commands that call a base command in the same transaction.
- Overlay routes that replace base routes.

## 2. Schema

The overlay's migration, in the base schema `inv`:

```sql
ALTER TABLE inv.location_type    ADD COLUMN containment boolean NOT NULL DEFAULT false;
ALTER TABLE inv.inventory_status ADD COLUMN qc          boolean NOT NULL DEFAULT false;

INSERT INTO inv.location_type (code, name, containment)
VALUES ('containment', 'Containment', true);

INSERT INTO inv.inventory_status
    (code, name, active, usable, move, adjust, status_change, split, merge, qc)
VALUES ('containment', 'Containment', true, false, false, false, false, false, false, true);

INSERT INTO inv.inv_trx_reason (code, name)
VALUES ('containment', 'Containment'), ('qc_release', 'QC release'), ('qc_fail', 'QC fail');
```

`location_type.containment` and `inventory_status.qc` are overlay columns on base tables. The overlay declares them in `field_owners`.

The `containment` status row:

| `active` | `usable` | `move` | `adjust` | `status_change` | `split` | `merge` | `qc` |
|---|---|---|---|---|---|---|---|
| true | false | false | false | false | false | false | true |

- `active` is true: stock in containment stays in financial stock.
- It is not `usable`: no user can choose it. Only the overlay's `inventory.open` and `inventory.move` set it.
- It has none of the five base command attributes, so the base commands refuse the stock.
- `qc` allows the QC commands. Only `containment` has it, so only stock in containment can get a QC decision.

## 3. Operations

| Operation | Input | Effect and rules |
|---|---|---|
| `inventory.open` | As the base operation | Replaces the base operation of the same name. It calls the base `inventory.open`, then applies the containment rule to each line whose location has a containment type. |
| `inventory.move` | As the base operation | Replaces the base operation of the same name. It calls the base `inventory.move`, then applies the containment rule to each line whose destination location has a containment type. |
| `qc.release` | Per line: inventory ID, expected revision | The current status must have `qc`. Sets the status `active`. Reason `qc_release`. |
| `qc.fail` | Per line: inventory ID, expected revision | The current status must have `qc`. Sets the status `defective`. Reason `qc_fail`. `defective.active` is true, so the stock stays in financial stock. |

**Containment rule**

- A line that lands in a containment location must have the status `active`. It becomes `containment`.
- Any other status refuses the whole posting. This refuses a move of defective stock into a containment location. It also refuses an open with the status `defective` in a containment location.
- For the lines it changes, the overlay writes its own transaction header with reason `containment` and one `status` effect per line. The header number comes from the base's `inv_trx` sequence.
- The base posting and the overlay posting commit in one database transaction, or neither commits. Such a request records two headers.
- A request with no line in a containment location records the base posting only.

**QC commands**

- Each writes one transaction header and one `status` effect per line, and follows the posting contract of the base spec (§3).
- Released stock is `active`. All base commands apply to it again. A move into another containment location makes it `containment` again.
- Failed stock is `defective`. It can move out of the containment location. It cannot move into a containment location.

**Why the overlay replaces two base operations.** In the base, only `inventory.open` and `inventory.move` put stock in a location. A split keeps the source's location, and stock in `containment` cannot be split. If the base's two routes stayed, a user could put stock in a containment location without the status change.

**Permissions.** Each operation's permission is the same text as its name. A caller of a replaced operation needs the overlay operation's permission only. The base operation runs under the overlay operation's authority.

## 4. Platform prerequisite: an overlay route replaces a base route

On main, a release merges the routes of every package, base and overlay (`crates/release/src/publish_release.rs::deployment_routes`). Two packages with the same route path refuse (`crates/release/src/publish_release/attachments/plan.rs::resolve_owned_route_methods`).

The owner ruled on 2026-11-27 that an overlay's route replaces the same path of a base that the overlay pins, and that the release drops the base's route.

- The change is in `resolve_owned_route_methods`. Its two callers pass the map of pinned bases.
- It needs tests, a fixture overlay that replaces a route, one journey and the docs.
- The control and CLI images rebuild. No host image rebuild is expected (read from the manifests, not built).
- Not decided: the replaced base operation stays registered in the release, so a later workflow node could call it.

## 5. Facts on main that this spec relies on

| Concern | Fact |
|---|---|
| Call in one transaction | An overlay command can call a base command in the same host transaction. The overlay pins the base by version and digest in `base_dependencies` and lists the base operations it calls. The build composes both into one component (`crates/build/src/composer.rs`, `docs/architecture/data-access.md`). |
| Same operation name | An overlay operation that bears a base operation's name imports that base operation to call it (`crates/codegen/src/generate/component.rs::emit_package_world`). No application on main does this yet. |
| Writing a base column | The build checks that the columns in a command's SQL exist. It does not check which package owns them (`crates/codegen/src/generate/validation.rs`). The overlay's SQL can set `inventory.status_id`. |
| Sequence | Two packages of one application that declare one sequence name with one pattern share one counter (`crates/env/src/env/stage/plan.rs::sequence_defaults`). The overlay declares `inv_trx` with the base's pattern. |
| Rows in base lookups | Migrations are plain SQL with no statement policy. Base and overlay migrations run as one login: the migration of `apps/client_acme_receiving` alters a base table, which only the table owner can do. Inference: the overlay's inserts into base lookups work. No migration on main inserts a row yet. |
| Column named `move` | Generated Rust spells a keyword with a trailing underscore: `move_` (`crates/package/src/manifest.rs::rust_identifier`). |
