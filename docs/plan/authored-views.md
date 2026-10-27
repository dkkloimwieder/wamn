# Authored views

## Purpose and scope

A package can author PostgreSQL views in its own schema.
A view is a named, reviewed relation that holds one join and its column list in one place.
Views serve three uses:

- Report sources, which report operations read with their own filters.
- A narrow read surface, because the application role can read a view instead of its base tables.
- The relation that a query feed reads when its query joins, as [wamn offline and sync](wamn-sync.md#query-feed-host) states.

The operation stays the only unit of permission, through role to operation rows.
Views work under that rule.
They add no role, no grant surface, and no access-control list.

Materialized views stay refused.

## Terms

| Term | Meaning |
| --- | --- |
| base table | A table that a view reads. |
| owner rights | PostgreSQL checks a view's base table privileges against the view owner, not the caller. |
| invoker rights | PostgreSQL checks them against the caller, through the view option `security_invoker`. |
| linking column | A view column that holds a stored foreign key value, which names a row of another table. |
| root | The table whose id keys each row of a feeding view. |
| column origin | The base table and column that a plain view column reads. |

## Authoring

A package declares a view in its migration SQL with `CREATE VIEW`, in the package schema.
The platform applies it with the other migration statements of the package.

The manifest does not declare views, for these reasons:

- Migrations are already the one owner of package DDL, and apply runs them as the package owner role.
- A view that reads a changed table must be dropped and created around the `ALTER TABLE` in the same file. Only migration order can place it there.
- Qualification already compares a package's migrations with its predecessor's.
- A manifest declaration needs a second DDL path and a rule for its order against migrations.

[`migration_policy.rs`](../../crates/schema/introspection/src/migration_policy.rs), `validate_statement`, admits these statements:

- `CREATE VIEW` and `CREATE OR REPLACE VIEW` with a schema-qualified name in a configured schema.
- `DROP VIEW` with a schema-qualified name in a configured schema, without `CASCADE`.

It refuses `TEMPORARY`, `RECURSIVE`, `WITH CHECK OPTION`, and the `security_invoker` option.
A view is read-only for operations, so authored command SQL that writes to a view refuses.

The migration lexer does not parse the view's query.
After apply, the catalog reader makes sure that the view reads only relations that the package can read.
It reads the view's base tables from `pg_depend` through the view's `pg_rewrite` rule.
Each base table must be in the package's own schema, or a relation that the package's data access already reads.

| Rule | Invariant | Test |
| --- | --- | --- |
| Migration only | A view enters only through a package migration in a configured schema | `migration_policy` tests for each admitted and refused form |
| Own schema | A view lives only in its package's schema | Refusal test for a view in another schema |
| Read bound | A view reads only relations that its package can read | Catalog test with a view over another package's undeclared table |
| Read-only | No authored statement writes to a view | `sql_lex` refusal test for INSERT, UPDATE, and DELETE on a view |

## Ownership and grants

The package owner role owns the view, because apply runs migrations as that role.
[`synchronization.rs`](../../crates/control/lib/src/apply_package/synchronization.rs) calls `roles::set_package_owner_role` before it runs them.

wamn runs PostgreSQL 18.
[`environment.rs`](../../crates/control/lib/src/dev/environment.rs) refuses a version below 180000, and the CloudNativePG clusters use `ghcr.io/cloudnative-pg/postgresql:18`.
PostgreSQL 15 added `security_invoker`, so the option is available.

Views use owner rights, the PostgreSQL default.
The application role gets `SELECT` on the view columns that its operations read, and nothing on the base tables of that view.
With invoker rights, the application role needs `SELECT` on every base table too, so the view narrows nothing.

The data access derivation grants view columns the same way that it grants table columns today.
[`data_access.rs`](../../crates/schema/generator/src/data_access.rs), `derive_data_access_overlay_for_manifest`, writes those grants from the operations' derived reads.
So a grant still comes from an operation, and the role to operation rows stay the only permission surface.

Owner rights bypass row-level security on the base tables for the view owner.
No application table enables row-level security today.
The build refuses a view over a table that enables it.

| Rule | Invariant | Test |
| --- | --- | --- |
| Owner | The package owner role owns every view of the package | Apply test that reads `pg_class.relowner` |
| View grant | The application role reads a view without any grant on its base tables | Application-role SQL execution against a view |
| One surface | Every view grant comes from an operation's derived reads | Data access derivation test |
| Owner rights | A migration with `security_invoker` refuses | `migration_policy` refusal test |
| Row security | A view over a table with row-level security refuses | Catalog refusal test |

## Catalog and types

The catalog reader models views, with their columns, column types, and base tables.
[`postgres.rs`](../../crates/schema/introspection/src/postgres.rs) refuses relation `v` with `UnsupportedView` today.
Only authored views lift that refusal.
`UnsupportedMaterializedView` stays.
[`ir.rs`](../../crates/schema/introspection/src/ir.rs), `CatalogIr`, gains a view list beside `tables`.

The generated row type of a projection over a view comes from the view's columns.
Each field takes the column's type.
PostgreSQL reports every view column as nullable, so the catalog cannot give nullability.
The projection keeps its declared nullability, and the existing statement verifier makes sure that it matches the column types.

The catalog reader also records each view column's origin.
It prepares the view's own definition, from `pg_get_viewdef`, and reads the table OID and column number that PostgreSQL returns for each plain column.
A computed column has no origin.

Materialized views stay refused, because their rows change only through `REFRESH`.
`REFRESH` runs outside any operation, writes no history, and takes its own lock.
So the feed, the write log, and operations cannot see or order those changes.

| Rule | Invariant | Test |
| --- | --- | --- |
| View catalog | The catalog lists each view with its columns, types, base tables, and column origins | Introspection test over a migrated view |
| Row types | A projection over a view gets field types from the view's columns | Generator test that changes a view column type |
| Materialized | A materialized view refuses, naming it | Existing `UnsupportedMaterializedView` test |

## Upgrade

`CREATE OR REPLACE VIEW` can add columns at the end only.
It cannot drop, rename, reorder, or retype a column.
A migration that does any of those drops the view and creates it again in the same file.

PostgreSQL refuses `ALTER TABLE ... ALTER COLUMN ... TYPE` and `DROP COLUMN` on a column that a view reads.
The migration drops the view first, changes the table, and then creates the view again.
Apply runs one migration file in one transaction, so readers never see the view missing.

Qualification makes sure of three facts:

- Each view of the predecessor release still exists, with each of its columns and the same types. New columns are allowed. This is the rule for table columns, because statements of the predecessor release still run during the upgrade.
- Each view reads only relations that its package can read, as the authoring section states.
- Each authored statement of the new release prepares against the upgraded schema.

| Rule | Invariant | Test |
| --- | --- | --- |
| Compatible view | A new release keeps every predecessor view column with its type | Qualification refusal test that drops a view column |
| Dependency order | A base table change and its view rebuild apply in one transaction | Apply test with a column type change under a view |

## Effect profile

A statement that reads a view reports the view and each base table of the view.
[`profile.rs`](../../crates/schema/generator/src/profile.rs), `RelationProfile`, gains the view's base tables from `pg_depend`.
So role review still sees every table that an operation reaches.

Today [`sql_lex.rs`](../../crates/schema/generator/src/sql_lex.rs), `physical_select_relations`, keeps only names in the catalog's table map.
A view in authored SQL therefore yields no relation access at all.
This is the first implementation item, bead `wamn-p0q0g`.
`physical_select_relations` resolves a view to the tables that it depends on, from `pg_depend`.

| Rule | Invariant | Test |
| --- | --- | --- |
| Profile | A statement over a view reports the view and every base table | Profile test over a view with three base tables |

## History

Views have no history.
The history trigger runs on tables only, and a view holds no rows.
A feed over a view reads the history of the view's base tables.

| Rule | Invariant | Test |
| --- | --- | --- |
| No view history | Apply creates no history table for a view | Apply test that lists history tables |

## Feed on a view

A feeding view selects its root id.
For each base table, it either is the root, references the root, or is referenced by a column that the view selects.
That selected column is the linking column, as stored on the referencing row.

The build makes sure of this from the catalog:

1. It reads the base tables from `pg_depend`.
2. It reads the column origins and the foreign keys from `pg_constraint`.
3. For each base table other than the root, it looks for one of two links. One is a view column whose origin is a foreign key column to that table's key. The other is a foreign key on that table to the root's key.
4. If a base table has neither, it refuses the feed and names the table.

Affected root keys for one `changes` call:

- A root table entry gives its own key.
- A table that references the root gives the foreign key value from its row, and from its history `before` image for deletes and moves.
- A table that a linking column names gives its changed keys, and from its history `before` image for deletes. The view's root ids where that linking column is in those keys are affected.

The call then reads the view's rows for those root keys, as the query feed section states.

A view with aggregates, or with several rows for one root, feeds only in one case. Its root is the grouping key, and that key is unique per row.
The build reads the view definition as PostgreSQL stores and prints it, through `pg_get_viewdef`.
It accepts one of two shapes:

- No `GROUP BY`, `DISTINCT`, window, set operation, or set-returning function. Every join is `ON joined.<primary key> = <referencing>.<column>`, so each join adds at most one row per root.
- `GROUP BY` the root's primary key, from the root table in `FROM`. The group can also name columns of tables that the root row references, because each such table adds one row per root. Each joined table references the root, or is referenced by the root row.

It refuses every other shape, and names the view.
This reads PostgreSQL's printed form, which is narrower than authored SQL.

| Rule | Invariant | Test |
| --- | --- | --- |
| Linking columns | Every base table of a feeding view is the root, references it, or has a linking column | Build refusal test that names the table |
| Affected keys | A change to any base table returns the affected root keys, including deletes and moves | Host test that changes each base table of a view |
| One row per root | A feeding view returns one row per root key | Build refusal test for a one-to-many join without `GROUP BY` |

## Reporting

Views are report sources.
An authored report operation selects from a view with its own filters, sort, and paging.
It needs no feed.
The view holds the join, and the operation holds the question.

| Rule | Invariant | Test |
| --- | --- | --- |
| Report source | A report operation reads a view through its own authored statement and grant | Generated contract and application-role test |

## First uses

### Receipt screen

`receiving.receipt_screen_line` replaces the join in [`load_receipt_screen.sql`](../../apps/wamn_receiving/query/load_receipt_screen.sql).
Its root is the order line, with one row per line.
An order with no lines does not appear.

```sql
CREATE VIEW receiving.receipt_screen_line AS
SELECT
    line.id AS line_id,
    line.purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status AS purchase_order_status,
    purchase_order.supplier_id,
    purchase_order.row_version,
    line.line_number,
    line.item_id,
    item.item_number,
    line.ordered_quantity,
    line.received_quantity,
    line.ordered_quantity - line.received_quantity AS remaining_quantity
FROM receiving.purchase_order_line AS line
JOIN receiving.purchase_order AS purchase_order
    ON purchase_order.id = line.purchase_order_id
JOIN receiving.item AS item
    ON item.id = line.item_id;
```

`purchase_order_id` and `item_id` are its linking columns.
Both are `NOT NULL` foreign keys, so the inner joins keep every line.
`receiving.load_receipt_screen` then selects from the view where `purchase_order_id = $1`.

`purchase_order_line` has no revision column, and the feed needs none.

### Purchase order receiving summary

`receiving.purchase_order_receiving_summary` is the report view.
It has one row per purchase order, with its supplier and its line totals.

```sql
CREATE VIEW receiving.purchase_order_receiving_summary AS
SELECT
    purchase_order.id AS purchase_order_id,
    purchase_order.purchase_order_number,
    purchase_order.status,
    purchase_order.supplier_id,
    supplier.name AS supplier_name,
    count(line.id) AS line_count,
    coalesce(sum(line.ordered_quantity), 0) AS ordered_quantity,
    coalesce(sum(line.received_quantity), 0) AS received_quantity
FROM receiving.purchase_order AS purchase_order
JOIN receiving.supplier AS supplier
    ON supplier.id = purchase_order.supplier_id
LEFT JOIN receiving.purchase_order_line AS line
    ON line.purchase_order_id = purchase_order.id
GROUP BY purchase_order.id, supplier.name;
```

A report operation filters it by supplier and status.
It can also feed, because it has the second shape.
Lines reference the root, and `supplier_id` is the linking column for suppliers.
