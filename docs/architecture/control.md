# Native control

`wamn-control` serves the org control routes over native HTTP.
It authenticates org sessions and checks current authority before each operation.
It has no application release, route guest, runtime operator registration, or host heartbeat.
The platform chart deploys the `control` image.

Start the service with `wamn-control --config /etc/wamn/control/config.json platform`.
The installer owns this JSON file. The control Deployment mounts the system database login files and the database CA certificate.
Both database fields use `wamn-postgres::ConnectionConfig`.
The service refuses a database credential outside its org namespace.
It also checks the org control login and the fixed apply login.

| Field | Meaning |
| --- | --- |
| `org` | Org served by this process |
| `http_addr`, `public_origin` | Bind address and public control origin |
| `database`, `apply_database` | Public connection objects for control reads and lifecycle writes |
| `session_issuer`, `session_jwks_url`, `session_audience`, `session_algorithms`, `session_jwks_ca` | Bearer issuer, key-set address, org audience, EdDSA allowlist, and the CA bundle path |
| `administration_dir` | Public project connection files named by `control_administration_key` and credential subdirectories named by login Secret |
| `worker.image` | Pinned `control` image for operation Jobs |
| `worker.service_account` | Org worker account for existing platform updates and platform SQL |
| `worker.configuration` | ConfigMap that mounts `config.json` at `/etc/wamn/control` in each Job |
| `chart` | Installed immutable chart reference |
| `package_artifact_base`, `component_artifact_base`, `integration_artifact_base` | Installed artifact repositories |
| `registry_auth_file` | Mounted registry credential file |
| `registry_token_metadata` | Use the installed metadata credential source instead of the registry file |
| `insecure_registry`, `oci_ca_paths` | Installed registry transport and public trust bundles |
| `events` | Optional event policy with `stream_replicas` and `dup_window_secs` |
| `identity` | Optional operator `endpoint`, `client_cert`, `client_key`, and `server_ca` |
| `oauth_encryption` | Optional envelope encryption configuration |

Control authorizes the target and submits one Kubernetes Job.
It puts the request and authenticated actor in an immutable ConfigMap owned by that Job.
The worker mounts this input and the applied environment document from the project chart.
The request cannot select an image, command, credential, namespace, or chart path.

Deploy resolves the package closure and bindings, checks enabled workflows, applies SQL, and upgrades the application under the lifecycle lock.
Control reads the current package digest and content revision from the WorkloadDeployment.
Only the rollback worker reads Helm history for the target package and application configuration.
Job status is visible only through the authorized target and operation annotations.

The project chart creates the `<coord>-operation` account.
Environment configuration and operations that change the shared host use the org worker account.
Workers select managed database logins from the project placement and write their ServiceAccount kubeconfig under `/work`.
They fetch the named Secrets in the org and write only `username`, `password` and `ca.crt` to a memory-backed volume at `/etc/wamn/db/<login-secret>/`.
The recovery Job mounts its project recovery certificate. It can read only that project's source stream and receive replies on its own inbox.
The private `prune-record-history` and `prune-run-history` worker commands serve chart retention tasks.
They select the record or run retention login from the recorded project connection.
The chart mounts that role's Secret and the public administration connection files.
Library tests retain the assertions about retained rows, login refusals, and grants.

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
`wamn-environment` owns capture-gap recovery and reader lifecycle. Recovery selects the recorded capture role and its project recovery certificate. It accepts no database URL from the CLI.
Integration publication pushes local build output first. The service verifies the published artifact before it records its version.

`wamn` is the user CLI and runs internal bootstrap steps.
Deploy, rollback, status, schema, workflow, and integration administration use authenticated routes.
They require control credentials and have no direct database fallback.
Build, dev and eject stay local. Bootstrap stays local before control exists.
After bootstrap, `wamn platform deploy --only <owner>` updates an existing Helm owner through a worker Job.
Platform schema inspection and apply also use the worker. The service selects their chart and SQL image.
Cloud node pool administration belongs to the platform infrastructure tooling.

Environment creation and configuration use `environment.configure`. Deletion uses `environment.delete`.
Their Jobs call installer-owned role and database interfaces and then the environment lifecycle owner.
The installer runs a separate initial SQL bootstrap only when the baseline is absent and no configuration is recorded.
Its private worker repeats the baseline refusal before DDL. It cannot update an existing baseline.
The environment chart removes the initial bootstrap account's access after successful initialization.
Normal lifecycle and platform updates use configured restricted connections.
