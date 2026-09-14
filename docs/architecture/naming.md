# Naming

A package is the unit of ownership, versioning, and compatibility.
Its source identifier is singular `snake_case`, and its application home is `apps/<exact-package-id>/`.
Module and domain names organize implementation inside that package.
They do not create separate package versions or authority.

## Source and wire identifiers

WAMN-owned model, domain, route, event, JSON, SQL, and generated function identifiers use singular `snake_case`.
Generated language types use the language's naming convention.
Third-party protocol fields retain their required spelling.
Persisted identifiers and generated artifact names retain their declared contracts.
Native Rust prose and private names do not redefine those contracts.

Local operations use these forms:

```text
<data_model>.<crud_action>
<domain>.<custom_action>
```

The generated CRUD actions are `get`, `query`, `create`, `update`, and `delete`.
Custom actions use singular `verb_noun` names.
A model declares which generated actions it exposes.

## Operation identity

One token names an operation's export, dispatch selection, and authorization:

```text
<package-id-kebab>:<module-kebab>/<action-kebab>@<package-version>
```

Source underscores become single hyphens in this external spelling.
For `wamn_receiving`, `purchase_order.get` at `1.0.0` becomes `wamn-receiving:purchase-order/get@1.0.0`.
The package version is the only operation-version coordinate.

The token keys the component's operation declaration and names its exported handler instance.
The `registered-operation` field repeats that exact token for authorization.
Admission refuses a different repeated value.
There is no separate alias between the dispatch identity and the permission identity.

The [component contract](components.md) defines grouping and dependency declarations.
[Data access](data-access.md#canonical-values-and-sql-names) owns SQL names, canonical values, and pagination order.

## Reserved names

The platform reserves four column names: `created_at`, `created_by`, `updated_at`, and `updated_by`.
A model column with one of these names carries record history, and the model selects it in `audit_log`.
A column with another meaning takes another name.
[Data access](data-access.md#record-history) owns the declaration and the stamp rules.

The platform reserves the relation name suffix `_history`.
The history table of a logged relation is `<relation>_history`.
Generation refuses a model table, an internal relation, or a custom operation write target whose name ends with `_history`.
apply-package refuses a migration that creates such a table.
[Data access](data-access.md#history-tables-and-the-log-trigger) owns the history table rules.

The platform reserves the package id `wamn`, because its operation tokens start with `wamn:`.
The catalog and the generator refuse it.

The platform reserves the principal namespace `wamn:`.
A platform component writes under the `app_system.users` row named `wamn:<component>`.
The component is kebab-case and carries no action and no version.
The row names the component, never the invocation.
The closed component list is `provisioning`, `apply-package`, `materializer`, `executor`, and `audit-retention`.
A tenant or application cannot create a `wamn:` name.

The id of a platform row is `uuid5(WAMN_NAMESPACE, "wamn:<component>")`.
`WAMN_NAMESPACE` is `uuid5(NAMESPACE_DNS, "wamn.dev")`, which is `0df299cf-7085-537d-919b-1145b5ef00f8`.
Every tenant and deployment derives the same id, so no configuration carries it.
[`wamn-project-state`](../../crates/identity/project-state/src/lib.rs) defines the namespace, the component list, and the derivation.
