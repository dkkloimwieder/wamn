# Naming

A package is the unit of ownership, versioning, and compatibility.
Its source identifier determines its application home, `apps/<exact-package-id>/`.
Package and model names have no singular naming rule.
Module and domain names organize implementation inside that package.
They do not create separate package versions or authority.

## Source and wire identifiers

WAMN-owned source identifiers use `snake_case` where their language or wire contract requires it.
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
Custom actions use the names that the package declares.
A model declares which generated actions it exposes.

## Operation identity

One token names an operation's export, dispatch selection, and authorization:

```text
<package-id-kebab>:<module-kebab>/<action-kebab>@<package-version>
```

Source underscores become single hyphens in this external spelling.
For `wamn_receiving`, `purchase_order.get` at `2.0.0` becomes `wamn-receiving:purchase-order/get@2.0.0`.
The package version is the only operation-version coordinate.
It is authored once, in `wamn.k`, and an authored publication file names the token without it, as [operation references](data-access.md#operation-references) states.

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
A platform component writes under the `platform."user"` row named `wamn:<component>`.
The component is kebab-case and carries no action and no version.
The row names the component, never the invocation.
The closed component list is `provisioning`, `apply-package`, `executor`, and `audit-retention`.
A project or application cannot create a `wamn:` name.

Every environment also holds one `service` user named `workflow`, which runs workflows.
Its name is not in the `wamn:` namespace, and the platform grants it no role.
Admins grant it roles like any user.

The id of a platform row is `uuid5(WAMN_NAMESPACE, "wamn:<component>")`.
`WAMN_NAMESPACE` is `uuid5(NAMESPACE_DNS, "wamn.dev")`, which is `0df299cf-7085-537d-919b-1145b5ef00f8`.
Every project and deployment derives the same id, so no configuration carries it.
The id of the `workflow` user is `uuid5(WAMN_NAMESPACE, "workflow")`.
[`wamn-identity-protocol`](../../crates/identity/protocol/src/platform.rs) defines the namespace, the component list, and the derivation.

## Environment names

`wamn_registry::name` owns resource names, database names, NATS users, event subjects, and host status labels.
Every derived project key orders its coordinates as `<org>--<env>--<project>`.
Event subjects use `evt.<org>.<env>.<project>.<entity>.<operation>`.
The database name adds the prefix `wamn-db-` and the stored instance suffix.
Credential Secrets use the same coordinate order.

Project deployment resources use `e-<slug>-<hash>`.
The hash covers `org/env/project`, so sibling projects keep separate resources.
An application release uses `r-<slug>-<hash>` and hashes `org/env/project/application`.
Both use at most 24 environment characters and 20 hex characters from SHA-256.
Names fit Helm's 53-character limit.
Application objects carry `org/env/project` in `wamn.environment`. Shared host pods carry `org/env`.


The environment host name is `h-<slug>-<hash>`. The hash comes from `org/env`, without a project. The environment chart owns its ServiceAccount and project projection.

Application workloads select the shared environment host group. Each workload carries its own release digest and project deployment key in annotations. The `release` chart owns application workloads and route records. The `environment` chart owns the host Deployment. Application releases do not set release labels or release arguments on the host.
