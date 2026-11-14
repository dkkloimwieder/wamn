# Native control

`wamn-control` serves the org control routes over native HTTP.
It authenticates org sessions and checks current authority before each operation.
It has no application release, route guest, runtime operator registration, or host heartbeat.
The platform chart deploys the `control` image.

Start the service with `wamn-control --config /etc/wamn/control/config.json`.
The installer owns this JSON file. Database passwords stay in CNPG Secrets.
Both database fields use `wamn-postgres::ConnectionConfig`.
The service refuses a database credential outside its org namespace.
It also checks the org control login and the fixed apply login.

| Field | Meaning |
| --- | --- |
| `org` | Org served by this process |
| `http_addr`, `public_origin` | Bind address and public control origin |
| `database`, `apply_database` | Public connection objects for control reads and lifecycle writes |
| `session_issuer`, `session_jwks_ca` | Session authority and its public trust bundle |
| `administration_dir` | Project administration connection files, named by `control_administration_key` |
| `worker.image` | Pinned `control` image for operation Jobs |
| `worker.service_account` | Org worker account for existing platform updates and platform SQL |
| `worker.configuration` | ConfigMap that mounts `config.json` at `/etc/wamn/control` in each Job |
| `chart` | Installed immutable chart reference |
| `release_artifact_base`, `package_artifact_base`, `component_artifact_base`, `integration_artifact_base` | Installed artifact repositories |
| `registry_auth_file` | Mounted registry credential file |
| `registry_token_metadata` | Use the installed metadata credential source instead of the registry file |
| `insecure_registry`, `oci_ca_paths` | Installed registry transport and public trust bundles |
| `nats` | Broker `url`, mounted `ca`, `cert`, `key`, and optional `server_name` |
| `event_observer` | Optional broker connection for capture-gap recovery, using the observer credential |
| `events` | Optional event policy: `stream_replicas` and `dup_window_secs` |
| `identity` | Optional operator `endpoint`, `client_cert`, `client_key`, and `server_ca` |
| `oauth_encryption` | Optional envelope encryption configuration |

Application operations use the shared Kubernetes Job runner.
The service supplies the authenticated actor and reads typed environment configuration through `wamn-registry`.
The registry owns desired release selections. Helm owns deployment and rollback history.
Workers receive coordinates and release identities. A request cannot select an image, command, credential, namespace, or chart path.
The worker calls `environment::operation::execute` once. It runs SQL in the same Job.
`release.retain` calls the environment retention owner under its existing lock.
Job status is visible only through the authorized target and operation annotations.

The chart mounts the apply credential, `wamn-ctl-nats`, and the configured registry credential in operation Jobs.
SQL operations also mount the project platform-migration, package-migration, and capture credentials.
Reset uses capture authority for replication and package-migration authority for package object deletion.
Their names come from the registry coordinate, database identity, and the provisioning role definitions.
No operation Job mounts a superuser credential.
Jobs use the environment's existing `code` or `sql` ServiceAccount and a bounded lifetime.
The chart must grant those accounts access only to their owning role Secrets.
The worker writes its ServiceAccount kubeconfig under `/work`, which needs a writable volume.

The private `prune-record-history` and `prune-run-history` worker commands serve chart retention tasks.
They select the record or run retention login from the recorded project connection.
The chart mounts that role's Secret and the public administration connection files.
Library tests retain the assertions about retained rows, login refusals, and grants.

`api_service.create` returns its PAT through the authenticated response.
`wamn create-api-service` saves that response to the selected private file and prints only its principal id.
The token never enters a Job log or a Secret handoff.
Workflow administration also uses server-selected connections and the authenticated principal.
Capture-gap recovery selects the recorded capture role and the configured event observer. It accepts no database URL from the CLI.
Integration publication pushes local build output first. The service verifies the published artifact before it records its version.

`wamn` is the user CLI. `wamn-ctl` runs internal bootstrap steps.
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
`pat.revoke` checks the authenticated org's authority before it calls the PAT owner.
