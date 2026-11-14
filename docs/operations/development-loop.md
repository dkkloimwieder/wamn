# Development loop

The local developer loop builds components and loads them from files.
It keeps the application database for most saved changes.
In `reset` mode, a changed SQL recipe recreates the recorded disposable database.
Local saves do not publish components or release manifests to a registry.
[Building](building.md) gives the required build commands.

## [WAMN-DEV-ENVIRONMENT] Developer session

Build `wamn`, `wamn-host`, `wamn-identity`, the flow-http component, and the materializer component before starting a session.
Build each application with `wamn build` before `dev up`.
For each application with a web client, run `wamn web build --package <path>` before `dev up`.
The local server reads `web-artifact/dist/` below the package build output.
The loop rebuilds that standalone web artifact when it builds a changed package.
Build the platform UI with `pnpm --filter @wamn/platform-ui build`.
`--platform-ui` defaults to `apps/platform_ui/dist` and accepts another built directory.
If you change the manifest vocabulary of `wamn.json` or the KCL schema module, build `wamn` again first.
The loop runs the binary you built, so an older one refuses the manifest with `unknown field`.
Use only disposable PostgreSQL 18, one NATS with TLS, and telemetry services.
The system administrator URL must name `wamn_system` without a query or fragment.
`wamn dev up` resets its control store, so shared or durable targets are unsuitable.
An environment template keeps the catalog functions of the standup that created it.
To take changed catalog SQL, run `wamn dev up` again and get a new template.

Read the current required arguments from the existing command:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev up --help
```

Supply these flags: `--system-database-url`, `--root`, `--nats-url`, `--nats-tls-ca`, `--nats-tls-cert`, `--nats-tls-key`, `--route-host`, `--host-binary`, and one `--package` for each package root.
Supply the other flags below as the session needs them.

Render the local collector before starting the session:

```bash
cargo xtask dev-collector "$WAMN_DEV_ENV_DIR"
docker compose -f "$WAMN_DEV_ENV_DIR/local-artifacts/otel/compose.json" up --pull never --detach
```

The renderer reads `deploy/platform/otel-collector.yaml` and its GCP values at run time.
It preserves the receivers, processors, and image digest.
The host uses the deployed sampler: sampled parents and 10% of new traces. The collector does not sample traces.
It replaces the GCS exporters with OTLP JSON file exporters under `<dev-root>/local-artifacts/otel/`.
The collector keeps the deployed batch timeout, so small batches take up to 60 seconds to arrive.
An individual request can have no sampled trace.
The observation reader follows complete records in `<local_artifacts.directory>/otel/traces.jsonl` once per second.
It groups spans by trace ID and filters them by `project_env_key`, project, and environment.
It keeps bounded summaries and accepts an empty sample without blocking activation.
Supply the published collector address through `--otel-exporter-otlp-endpoint`.
The benchmark renders and starts this collector.

Supply `--flow-http-component` with the absolute path to the built `http_route.wasm`.
Supply `--materializer-component` with the absolute path to a `materializer.wasm` built with the guest RUSTFLAGS of [building](building.md#native-programs-and-app-components).
The emitted `local_artifacts` configuration names the local output directory and these components.
The environment document supplies the integration name and version.
The host reads credential references from Postgres and resolves values through the environment store.
Set `WAMN_CONTROL_URL` to the control host URL and `WAMN_TOKEN` to the caller session token.
Before activation, write connections with `wamn-ctl connection write`.
Supply the first credential through `connection write --credential-file <private-file>`.
Use `connection set-credential` for later credential changes.
The `dev up` command neither copies nor imports a credential file.

Supply `--platform-domain` with the domain of the platform principal emails, for example `example.invalid`.
The emitted configuration stores it in `platform_domain`.
Supply `--nats-url` with the one NATS, and `--nats-tls-ca`, `--nats-tls-cert` and `--nats-tls-key` with one client certificate.
The certificate maps to a user with every permission: the host, the reader, stream provisioning and the materializer use it.
Declare `--stream-replicas` and `--dup-window-secs` explicitly.
Supply `--cdc-reader-binary` with the built `wamn-cdc-reader`.

The command runs `enable-cdc-project-env`, which is the verb that the cluster runs.
The verb provisions the event streams with the materializer consumers of the packages, and it records the reader registration.
The command applies the replication role, prepares a registry-reader login, and writes `cdc_reader` into `dev.json`.
The PostgreSQL server must run with `wal_level=logical`.
At each activation, the loop applies the CDC SQL of the verb to the target and starts `wamn-cdc-reader`.
The reader logs to `cdc-reader.log` in the environment directory, and the loop waits until it streams.
The loop stops the reader before it replaces the target, and the target lease drops the replication slot before the drop.

The command writes `host-config.yaml` into the environment directory with mode 0600.
It holds the `plugins` entry that the release chart declares: the `wasmcloud-nats` binding `events` with the environment's stream, subjects and inbox.
The binding names no server, so it uses the host's data connection and its certificate.
`host_config_file` in `dev.json` names the file, and the loop passes it to the host as `--user-config`.

The loop runs the platform materializer as a second workload beside the release, as the cluster does.
At each activation, the loop renders `deploy/platform/materializer.example.yaml` with `render_materializer`, the renderer of the cluster files.
The rendering uses the deployed fetch and sweep intervals of `wamn-control-provision`.
The loop writes the result with the local component bytes to `materializer.json` in the local output directory.
The materializer does not enter the local admission facts, the release manifest, or the preload.
The host starts it after the flow-http workload runs, and the loop waits until it runs.
The materializer restarts with the host on each loop change. Its JetStream consumers are durable, so a restart resumes them.

Local application and control hosts use `WAMN_OAUTH_ENCRYPTION='{"provider":"local"}'`.
They share `<dev-root>/oauth.key` through `WAMN_OAUTH_KEY_FILE`.
Startup creates the 32-byte key with mode `0600` only when the file is absent.
Keep that file across host restarts so that stored OAuth tokens stay readable.

The command starts identity, the org control host, and a local web server, then writes private `dev.json` and exits.
The control host uses `wamn-host host --control`. Its component configuration references the org control login and its native basic-auth Secret.
The host serves control HTTP endpoints natively. `deploy/platform/control-service.yaml` exposes that host through a Service.
The administration directory holds the patch from the normal administration credential operation.
No application release runs in the control host.

The command prints the platform UI URL and a URL for each built application client.
`web.json` in the environment directory records those URLs.
Each URL uses one origin for static files, `/password` requests, and `/api` requests.
The proxy trusts the local identity certificate authority and preserves session cookies.
The platform UI receives an org-only `config.json` from the web front.
Application clients receive the org and project.
The development loop writes `application-endpoint.json` after activation and removes it before shutdown.
The proxy reads that file for each application request, so rebuilds can change the host port.
Before application activation, application API requests return 503.
Control sign-in works as soon as `dev up` returns.
Read `control-web.log` in the environment directory if control startup fails.
The identity process keeps running across application builds.
Startup creates its signing key once. Application rebuilds preserve that key and the identity database.
The `session_identity` field supplies the issuer address, certificate authority file, and environment instance to each application host.

`dev up` starts identity with `--local-mail`, which selects the local mail sender.
The sender records mail in `mail.jsonl` inside the private directory named by `identity-process.json`.
Each line contains `to`, `subject`, and `text`. Invitation text includes `Invitation code: <principal>:<secret>`.

Open the platform UI at `/invite#<principal>:<secret>` to set the invited user's password.
The file has mode `0600`, and `dev down` removes it with the private identity directory.
Local identity sends no network mail, even when `.env` contains Resend credentials.
Deployed identity keeps the existing [password configuration](deployment.md#identity-password-configuration).

At environment teardown, stop the owned processes:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev down --root "$WAMN_DEV_ENV_DIR"
```

This command stops the web server and control host, then removes identity, its database login authority, and its private certificate directory.
It leaves externally supplied PostgreSQL, NATS, and telemetry services running.
Stop the environment before running `dev up` again against the same directory.
Local identity certificates last 30 days. A new disposable environment creates new certificates.

Write `environment.k` in the directory selected by `dev up --root` before you provision the environment.
Use the same KCL format as deployed environments.
Declare every candidate package in exactly one application.
For Acme Receiving, the file can contain:

```kcl
import environment

environment.Environment {
    org = "acme"
    project = "receiving"
    env = "dev"
    target = "cloud"
    policy = "dev"
    revocation_seconds = 60
    applications = {
        receiving = {
            current = "none"
            hostname = "receiving.localhost"
            revocation_seconds = 5
            packages = ["wamn_receiving", "client_acme_receiving"]
        }
    }
}
```

Edit the coordinate and route host to match your local environment.
For Receiving alone, remove `client_acme_receiving` from the package list.
The loop reads the application map from this file and passes it to the host through `--applications`.
The candidate build supplies the local release.

If the file is missing, `wamn dev` refuses and names its expected path.
The loop never derives an application name from a package or writes this file for you.
Changes to this file trigger a new run in watch mode.

Use the emitted configuration:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/client_acme_receiving" --watch
```

Use this Acme overlay command for an Acme change.
For a Receiving change, give `wamn dev up` only `--package "$PWD/apps/wamn_receiving"` as its package root.
Then run the loop with Receiving as the overlay:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/wamn_receiving" --watch
```

This loop initializes missing SQL, generates, builds, and releases only the Receiving package closure.

Add `--hold` to keep the activation alive until interruption.

The loop runs from a Git worktree.
After changing generator code, rebuild `wamn` and restart the developer process.

The host reads platform defaults from `deploy/platform/release/values.yaml` embedded at build time.
These defaults supply the drain delay, the NATS connection timeout, and probe enablement.
A Wasmtime store holds a component's execution state.
The HTTP workload reads its Wasmtime store pool size and concurrency from the same defaults.
Activation sets HOME to `/tmp`, the same value that the runtime-operator chart template sets.
The parity test compares HOME directly, so a change on either side fails the test.

The chart's `runtime.env` list also supplies allocator residency, the bytes that each slot retains after use.
Application and control hosts read this list, including those started by `cargo xtask bench-local`.
`WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT` is `65536`. A release probe measured the 1179648-byte initial Receiving memory as slower, because the drop clears the kept range with memset.
`WASMTIME_POOLING_TABLE_KEEP_RESIDENT` is `4096`, one host page covering the largest Receiving table of 3,360 bytes.
`WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT` is `8192`, the two resident stack pages measured during Receiving instantiation.

Native async stack zeroing remains disabled, so the stack value currently has no effect.
These values stay below their slot capacities and do not change allocator limits.

The probe binds to a free loopback port.
Activation reads pool sizes, the request limit, and Wasmtime resource limits from the embedded `deploy/platform/release/values.yaml`, the one host default.
Warm reuse trusts only the digest of the HTTP route component staged by this developer session.
Rebuild `wamn` after changing that file.

The URL printed by `wamn dev` reaches the host directly.
The [local benchmark](benchmark.md#local-run) uses the direct host URL.

## Saved changes and target state

The running loop holds a lease, an exclusive hold on its application database.
A second loop or reset command cannot take that target while the lease remains active.
The loop starts from the target that `dev up` stamps.
After successful introspection, the loop caches the physical fingerprints and catalogs beside its local artifacts.
The loop compares the physical SQL fingerprint with the installed package records.
The fingerprint identifies the complete SQL recipe, including generated definitions and their execution owners.
Code, query, web, and package-version changes preserve rows when the SQL recipe stays unchanged.
A missing local catalog cache causes another database read, without authorizing a reset.
The loop reads `schema.mode` from the existing `environment.k` beside `dev.json`.
An omitted mode selects `reset` only when the registry records the environment as disposable.

The loop builds the selected packages before it compares installed SQL.
Each build describes statements against a private database on the target server.
The shared xtask library compiles components and writes one completed `build.json`.
Cargo decides which guests need compilation.
The Build stage compiles each web client. The Admit stage selects and admits the recorded components.
The loop prepares no SQLx verifier or metadata.
The watcher watches package directories, platform guests, shared WIT and local configuration files.
It skips paths covered by Git ignore rules.
The build record retains package identity, dependency pins, compiler identity and output digests.
It has no source fingerprints or freshness checks.
Artifact readers use the captured KCL, SQL and migrations beside the generated files.
Migrate reuses installed SQL decisions when the physical fingerprint matches.
Introspect reuses existing catalogs. ACL reuses preparation for the same compiled manifest.

In `reset` mode, a changed SQL recipe resets the owned disposable database through its existing template.
The loop uses `environment::sql::schema::plan` and `Diff::requires_reset`, as deployment does.
It prepares current generated output before that decision.
Both paths read package bytes through `apply_package::read_package_root` and execute SQL through the same package owner.
The local path opens `target_database_url` directly. The deployed path runs that owner in a Kubernetes SQL Job.
Local source access uses the completed build output and the same SQL executor.
It refuses a reset when the registry does not authorize data loss or a required schema source is missing.
The loop builds the candidate before it stops the previous application for a reset.
The reset discards all application rows and gives the database a new creation identity.

In `migrate` mode, the watcher reports pending SQL and keeps the installed SQL records unchanged.
It initializes missing packages but never appends migrations to an installed package.
Generated schema changes also require an explicit schema operation.
A mode edit alone preserves data.

For local SQL, use a built source directory and the local environment file:

```bash
wamn schema diff apps/wamn_receiving --env "$WAMN_DEV_ENV_DIR/environment.k"
wamn schema apply apps/wamn_receiving --env "$WAMN_DEV_ENV_DIR/environment.k"
```

The adjacent `dev.json` identifies the local database and credentials.
Before you apply local SQL, stop the application loop.
The command requires the exclusive target lease, which prevents concurrent replacement or SQL application.
The operation preserves application release selections.
In `reset` mode, it reconstructs retained packages from their recorded local sources before discarding data.
A changed or missing retained source stops preparation.
Deployed SQL operations use published artifacts and Kubernetes Jobs.
An application name selects its configured current release. `--release` selects another published release without selecting its code.
For local source operations, do not pass `--release`.
`wamn schema adopt --env "$WAMN_DEV_ENV_DIR/environment.k"` asserts the configured local sources without executing SQL.
Resolve any uncertain nontransactional file before adoption, as [deployment](deployment.md#nontransactional-sql-repair) describes.

Control store rows keyed by a non-empty target instance accumulate only within one `wamn dev up` standup.
Nothing deletes them one at a time, and the immutability trigger stays unconditional.
The control store reset of `wamn dev up` is the retention step, outside the recreate path.
A disposable projection made by hand with `wamn-ctl provision-project-env --disposable` against a durable control store has no such bound.

The previous application remains available during candidate preparation until an authorized schema reset requires an interruption.
A failure during preparation keeps that application running.
A failed stage prints its top context first and every cause under it.
The printed line names the refusal that stopped the stage.
The loop replaces the host only after the candidate passes preparation.
Platform builds publish palette components independently of application packages.
The environment document owns connection bindings.
The runtime enforces operation grants, connection bindings and credentials for each database role.

To discard the application data manually, stop the loop and reset its target:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev reset --config "$WAMN_DEV_ENV_DIR/dev.json"
```

The reset command refuses an active target lease.
Restart the loop with the same configuration after reset.

## Pushing from a working tree

The loop does not push packages.
To push a package that you changed, run `wamn build` for the package first, and then run `wamn-ctl push-package`.
`push-package` publishes the existing completed output, including the source snapshot captured by that build ([building](building.md#pushing-a-package)).
To see what a pushed package holds, run `wamn inspect`.
To compare two pushed versions, run `wamn diff` ([building](building.md#reading-a-pushed-package)).

## Issuer connection

`wamn dev` and `wamn dev up` take no PAT flags.
`dev up` starts its own identity process with `--operator-ca` set to a local operator CA.
It gives the control host the variables `WAMN_PAT_ISSUER`, `WAMN_PAT_CLIENT_CERT`, `WAMN_PAT_CLIENT_KEY`, and `WAMN_PAT_SERVER_CA`.
The `wamn-ctl` verbs that mint PATs take the flags `--pat-issuer`, `--pat-client-cert`, `--pat-client-key`, and `--pat-server-ca` with the same variable names.
A deployed identity service uses `--operator-ca` or `WAMN_IDENTITY_OPERATOR_CA` for client certificates.
Keep certificate keys and issued tokens out of committed files and published logs.
[Execution](../architecture/execution.md) explains session authority.

## Platform UI build

The platform UI lives in `apps/platform_ui` and reads an org-only `config.json`.
Run these commands from the repository root:

```bash
pnpm --filter @wamn/platform-ui lint
pnpm --filter @wamn/platform-ui test
pnpm --filter @wamn/platform-ui build
```
The build writes `apps/platform_ui/dist` without an application package or `wamn.k`.
[Building](building.md#platform-ui) owns the platform build record.
[Deployment](deployment.md#platform-ui-files) owns upload and control-host routing.
