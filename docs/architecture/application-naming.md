# Application naming law

Status: RATIFIED for the Receiving base and client-overlay POC.

This document is the naming contract for generated application artifacts. The
POC design remains in `docs/poc/`; generators cite this contract rather than
copying its rules.

## Technical identifiers

WAMN-owned wire and schema identifiers are singular `snake_case`. This applies
to package-local identifiers, data models, operations, domains, route segments,
events, JSON properties, SQL relations and fields, and generated function
names. Generated language types may use that language's naming convention.
Third-party protocol fields retain externally required spelling.

## Package and operation identity

A package is the ownership, version, and compatibility boundary. A module or
domain organizes implementation inside a package and has no independent public
package identity.

Canonical operation identity is:

```text
<package_id>@<package_version>::<local_operation>
```

The package version is the only operation-version coordinate. A local
operation has one of two forms:

```text
<data_model>.<crud_action>
<domain>.<custom_action>
```

The closed generated CRUD action set is `get`, `query`, `create`, `update`, and
`delete`. Custom actions use singular `verb_noun` names.

## Pagination order

Keyset pagination uses `id` as the total-order tie-breaker, and `id` inherits
the declared primary sort direction; descending reverses the compound order.
Cursor keys preserve canonical PostgreSQL lexical values: `timestamptz` is UTC
RFC3339 with exactly six fractional digits, always, and `numeric` preserves scale.
Canonical means one spelling of what PostgreSQL holds, never a transformation
of it. Durable command, cursor, and weld JSON use
`wamn_execution_contract::canonical_json_bytes` as their single byte authority.

## Command envelopes

`receiving.record_receipt` accepts `1..=100` outer items and `1..=100` lines
per item. Raising either bound requires a ruled demand naming its consumer.
Receipt lines are canonicalized by `purchase_order_line_id`; duplicates refuse.
Raw bodies above 1 MiB refuse at ingress with HTTP 413 before parsing, while
parsed count breaches refuse inside the operation as `invalid_input` with the
bound and observed count. Transport and operation refusals remain distinct.

## PostgreSQL schema selection

Migrations author schema selection and require qualified DDL; SQL corpora inherit
the host-selected `search_path` and refuse qualified references.

## PostgreSQL constraint and index names

Constraint and ordinary-index names normally use:

```text
<table>_<column_1>[_<column_n>]_<kind>
```

Every referenced column appears in table-definition order. The kind suffixes
are:

| Object | Kind |
|---|---|
| Primary key | `pkey` |
| Unique constraint | `key` |
| Foreign key | `fkey` |
| Check constraint | `check` |
| Ordinary index | `idx` |

Names are explicitly authored and must contain fewer than 64 bytes. PostgreSQL
auto-generated names and silent identifier truncation are refused. When the
full conventional name cannot fit, the author writes an explicit shorter name
in the migration; no tool silently abbreviates it.
