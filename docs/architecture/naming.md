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
The closed component list is `provisioning`, `apply-package`, `materializer`, `executor`, and `audit-retention`.
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

The Helm release lives in the release namespace `hosts` (R6). The host group file owns the scheduler and data NATS URLs, because the subchart default assumes NATS in the release namespace. Helm release names are bounded to 53 characters, so the name is `r-<slug>-<hash>`, where slug is the env name cut to 24 characters and hash is the first 20 hex of sha256(org, project, env), 80 bits, collision-resistant at any scale wamn will see; the full coordinate is in the annotation `wamn.environment` on every object. Every object identity in the namespace derives from that name and nothing is authored: the wasmCloud host group is the release name, so the subchart renders `hostgroup-<name>`; the WorkloadDeployments are `<name>-http` and `<name>-materializer`; the Services likewise. Two environments therefore cannot share an object name, which matters because the chart names objects after the host group and not after the release (`templates/runtime/deployment.yaml (name: hostgroup-{{ .name }})`). The same rule binds every Secret and ConfigMap the environment owns: its credential Secrets are named from the coordinate, as `workload_secret_name` already does (`provision/src/name.rs::workload_secret_name`, `<family prefix><org>--<project>--<env>`); a fixed name belongs only to a platform-owned shared object, today the TLS Secrets the operator release renders. The CNPG `Database` CR for the environment is applied by `apply` with `kubectl apply` (§12.1) and carries the same annotation; it is not a chart object because a chart around one CR is ceremony, and CNPG reconciles the CR itself (§4.4). Credential Secrets are not chart objects either: `apply` writes them after the database is ready and is their one owner; the chart references them by those derived names.

The pod label is the same value for the live set and `kubectl`; the two label keys for the group differ (`wasmcloud.com/hostgroup` on the pod, `hostgroup` in the heartbeat) and are both the chart's. A Kubernetes label value is at most 63 characters of `[a-z0-9A-Z._-]`, so the label carries the full 32-byte digest as lowercase unpadded base32 (52 characters), which is exact; the hex form is in the `wamn.release-digest` annotation and in the host's arguments. The host group is the environment's stable pool, one per environment. Its name is the Helm release name (§9.1). The release chart (`deploy/platform/release`) derives `runtime-operator.runtime.hostGroups[].name` and passes the release in its arguments. The release is the label, not the group.
