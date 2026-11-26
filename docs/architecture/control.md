# Native control

`wamn-control` serves the org control routes over native HTTP.
It authenticates org sessions and checks current authority before each operation.
It has no application release, route guest, runtime operator registration, or host heartbeat.
The org chart deploys the `control` image.

Start the service with `wamn-control --settings /etc/wamn/control/settings.json platform`.
The org chart renders this JSON file and mounts it for control and the org worker.
The control Deployment mounts the org's control and apply logins and the system database CA certificate.
The worker uses the org's apply login for lifecycle operations.
Both database fields use `wamn-postgres::ConnectionConfig`.
The service refuses a database credential outside its org namespace.
It checks that the apply login name matches the org and database.

| Field | Meaning |
| --- | --- |
| `org` | Org served by this process |
| `public_origin` | Public control origin |
| `database`, `apply_database` | Public connection objects for control reads and lifecycle writes |
| `identity_issuer` | Shared identity endpoint for token verification and user administration |
| `nats_url` | Shared in-cluster NATS endpoint |
| `worker_image` | Pinned control image for operation Jobs |
| `chart` | Installed immutable chart reference |
| `registry` | Org origin for package artifacts and application components |
| `platform_registry` | Platform origin for integration components |
| `registry_token_metadata` | Use the metadata credential source for registry reads |
| `oci_ca_paths` | Mounted registry trust bundles |
| `events` | Optional event policy with `stream_replicas` and `dup_window_secs` |
| `oauth_encryption` | Optional envelope encryption configuration |

Control listens on port 80. It derives the JWKS URL and org audience and verifies EdDSA signatures.
The worker ConfigMap and service account names follow from the org.

Control authorizes the target and submits one Kubernetes Job.
It puts the request in an immutable ConfigMap owned by that Job. For a journaled command, the ConfigMap also carries the command ID and authenticated actor.
The worker mounts the request and reads applied project configuration from the registry.
The request cannot select an image, command, credential, namespace, or chart path.

Deploy resolves the package closure and bindings under the lifecycle lock.
It reads the manifest of each application and integration image and of the ingress and workflow images pinned in the chart.
After those reads and the workflow checks pass, deploy applies SQL and upgrades the application.
Control reads the current package digest and content revision from the WorkloadDeployment.
Only the rollback worker reads Helm history for the target package and application configuration.
Job status is visible only through the authorized target and operation annotations.
Control writes command starts and dispatch refusals or failures to `provisioning.platform_event`. Workers use the apply login to write the result of a command they run.

Deploy, rollback, stop, schema apply, schema reset, capture-gap recovery and close, and environment lifecycle Jobs use the org worker account for Helm.
The project chart creates the `<coordinate>-operation` account for schema diff. It stages files from named database Secrets and reads declared application workloads.
Workers select managed database logins from the project placement and write their ServiceAccount kubeconfig under `/work`.
They fetch the named Secrets in the org and write the login files and the server's `ca.crt` to `/etc/wamn/db/<login-secret>/` in memory.
Project logins use `tls.crt` and `tls.key` in that directory.
The recovery Job mounts its project recovery certificate. It can read only that project's source stream and receive replies on its own inbox.
The [record history pruning code](../../crates/env/src/prune_record_history.rs) uses a project database login and binds `wamn:audit-retention` for each transaction.
The [PostgreSQL test](../../crates/env/tests/history_retention_live.rs) checks that creation of a history table waits for pruning.

`api_service.create` returns its PAT through the authenticated response.
`wamn create-api-service` saves that response to the selected private file and prints only its principal id.
The response carries the raw token once. The database stores only its hash.
`service_principal.create` and `service_principal.disable` manage local service rows.
`pat.issue` and `pat.list` use the given service principal ID.
`pat.revoke` uses the token prefix and requires an org admin.
Each route takes `project` and `env` and uses the matching public connection file in the administration directory. The database client reads the three credential files from the named login subdirectory.
The system database supplies the platform domain for service email addresses.
Service creation refuses with `platform-domain-unset` if the domain is missing.
Workflow administration also uses server-selected connections and the authenticated principal.
`wamn-env` owns capture-gap recovery and reader lifecycle. Recovery selects the recorded capture role and its project recovery certificate. It accepts no database URL from the CLI.
The platform system schema seeds integration types. Only `wamn platform deploy` can change them.

`wamn` is the user CLI and runs internal bootstrap steps.
Deploy, rollback, status, schema, workflow, and integration administration use authenticated routes.
They require control credentials and have no direct database fallback.
Build, dev and eject stay local.
`wamn platform deploy --only` returns in Epic 10 with the `platform` chart.
Platform schema inspection and apply use `platform.schema` to start a SQL Job and `platform.operation_status` to follow it.
The service selects the worker image.
Cloud node pool administration belongs to the platform infrastructure tooling.

Environment creation and configuration use `env.configure`. Deletion uses `env.delete`.
Their Jobs call installer-owned role and database interfaces and then the environment lifecycle owner.
The installer runs a separate initial SQL bootstrap only when the baseline is absent and no configuration is recorded.
Its private worker repeats the baseline refusal before DDL. It cannot update an existing baseline.
The environment chart removes the initial bootstrap account's access after successful initialization.
Lifecycle operations and platform SQL use configured restricted connections.
