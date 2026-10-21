# Development loop

The local developer loop builds components and loads them from files.
It keeps the application database for most saved changes.
When the package structure changes, it recreates that database.
Local saves do not publish components or release manifests to a registry.
[Building](building.md) gives the required build commands.

## [WAMN-DEV-ENVIRONMENT] Developer session

Build `wamn`, `wamn-host`, `wamn-identity`, the flow-http component, and the materializer component before starting a session.
Build each application with `wamn build` before `dev up`, so its web bundle exists.
Build the platform UI with `pnpm --filter @wamn/platform-ui build`.
`--platform-ui` defaults to `apps/platform_ui/dist` and accepts another built directory.
If you change the manifest vocabulary of `wamn.json` or the KCL schema module, build `wamn` again first.
The loop runs the binary you built, so an older one refuses the manifest with `unknown field`.
Use only disposable PostgreSQL 18, scheduler NATS, event NATS, and telemetry services.
The system administrator URL must name `wamn_system` without a query or fragment.
`wamn dev up` resets its control store, so shared or durable targets are unsuitable.
An environment template keeps the catalog functions of the standup that created it.
To take changed catalog SQL, run `wamn dev up` again and get a new template.

Read the current required arguments from the existing command:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev up --help
```

Supply an explicit environment directory, package roots, built binaries, and service endpoints.

Render the local collector before starting the session:

```bash
tools/dev-collector "$WAMN_DEV_ENV_DIR"
docker compose -f "$WAMN_DEV_ENV_DIR/local-artifacts/otel/compose.json" up --pull never --detach
```

The renderer reads `deploy/platform/otel-collector.yaml` and its GCP values at run time.
It preserves the receivers, processors, and image digest.
The host uses the deployed sampler: sampled parents and 10% of new traces. The collector does not sample traces.
It replaces the GCS exporters with OTLP JSON file exporters under `<dev-root>/local-artifacts/otel/`.
The collector keeps the deployed batch timeout, so small batches take up to 60 seconds to arrive.
An individual request can have no sampled trace.
The observation reader follows complete records in `<local_artifacts.directory>/otel/traces.jsonl` once per second.
It groups spans by trace ID and filters them by tenant, project, and environment.
It keeps bounded summaries and accepts an empty sample without blocking activation.
Supply the published collector address through `--otel-exporter-otlp-endpoint`.
The benchmark and pilot render and start this collector themselves.

Supply `--flow-http-component` with the absolute path to the built `http_route.wasm`.
Supply `--materializer-component` with the absolute path to a `materializer.wasm` built with the guest RUSTFLAGS of [building](building.md#native-programs-and-app-components).
The emitted `local_artifacts` configuration names the local output directory and these components.
For declared connections, pass `--local-bindings` with an absolute path to the selection file.
The emitted configuration stores that path in `local_artifacts.bindings`.
If the host needs credentials, pass `--credentials-file` with a file of the form `{project: {handle: secret}}`.
The command copies the file into the environment directory with mode 0600, and `credentials_file` names the copy.
The loop passes that path to the host as `WAMN_CREDENTIALS_FILE`, as the cluster passes its mounted Secret.

Supply `--platform-domain` with the domain of the platform principal emails, for example `example.invalid`.
The emitted configuration stores it in `platform_domain`.
Supply event runtime credentials separately from provisioning credentials.
Declare `--stream-replicas` and `--dup-window-secs` explicitly.
Use `--event-provisioning-username` and `--event-provisioning-password-file` for stream creation.
Use `--event-nats-username` and `--event-nats-password-file` for runtime access.
Supply `--cdc-reader-binary` with the built `wamn-cdc-reader`.
Use `--event-publisher-username` and `--event-publisher-password-file` for the credential that the reader publishes with.

The command runs `enable-cdc-project-env`, which is the verb that the cluster runs.
The verb provisions the event streams with the materializer consumers of the packages, and it records the reader registration.
The command applies the replication role, prepares a registry-reader login, and writes `cdc_reader` into `dev.json`.
The PostgreSQL server must run with `wal_level=logical`.
At each activation, the loop applies the CDC SQL of the verb to the target and starts `wamn-cdc-reader`.
The reader logs to `cdc-reader.log` in the environment directory, and the loop waits until it streams.
The loop stops the reader before it replaces the target, and the target lease drops the replication slot before the drop.

Use `--event-materializer-username` and `--event-materializer-password-file` for the credential that the materializer consumes with.
The command writes `materializer-nats.json` into the environment directory with mode 0600, in the shape of the cluster's binding Secret.
The file names the server, the credential, its private inbox prefix, and the environment's source stream and subjects.
`materializer_nats_binding_file` in `dev.json` names the file, and the loop passes it to the host as `--materializer-nats-binding-file`.

The loop runs the platform materializer as a second workload beside the release, as the cluster does.
At each activation, the loop renders `deploy/platform/materializer.example.yaml` with `render_materializer`, the renderer of the cluster files.
The rendering uses the deployed fetch and sweep intervals of `wamn-control-provision`.
The loop writes the result with the local component bytes to `materializer.json` in the local output directory.
The materializer does not enter the local admission facts, the release manifest, or the preload.
The host starts it after the flow-http workload runs, and the loop waits until it runs.
The materializer restarts with the host on each loop change. Its JetStream consumers are durable, so a restart resumes them.

The command starts identity, the org control host, and a local web server, then writes private `dev.json` and exits.
The control host uses `wamn-host host --control` and the same org credential operation as `provision-org --prepare-control-generation`.
It loads the local flow-http bytes with the interfaces from `deploy/gcp/control-service.yaml`.
The administration directory holds the patch from the normal administration credential operation.
No application release runs in the control host.

The command prints the platform UI URL and a URL for each built application client.
`web.json` in the environment directory records those URLs.
Each URL uses one origin for static files, `/password` requests, and `/api` requests.
The proxy trusts the local identity certificate authority and preserves session cookies.
The platform UI receives an org-only `config.json`, as it does after `wamn web upload --platform-ui`.
Application clients receive the org and project.
The development loop writes `application-endpoint.json` after activation and removes it before shutdown.
The proxy reads that file for each application request, so rebuilds can change the host port.
Before application activation, application API requests return 503.
Control sign-in works as soon as `dev up` returns.
Read `control-web.log` in the environment directory if control startup fails.
The identity process keeps running across application builds.
Startup creates its signing key once. Application rebuilds preserve that key and the identity database.
The `session_identity` field supplies the issuer address, certificate authority file, and environment instance to each application host.

`dev up` selects the local mail sender through `--local-mail` in `crates/control/lib/src/dev/pat_issuer.rs`.
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

Use its emitted configuration:

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

This loop migrates, generates, builds, and releases only the Receiving package.

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
Application and control hosts read this list, including those started by `tools/bench/local.sh`.
`WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT` is `65536`. A release probe measured the 1179648-byte initial Receiving memory as slower, because the drop clears the kept range with memset.
`WASMTIME_POOLING_TABLE_KEEP_RESIDENT` is `4096`, one host page covering the largest Receiving table of 3,360 bytes.
`WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT` is `8192`, the two resident stack pages measured during Receiving instantiation.

Native async stack zeroing remains disabled, so the stack value currently has no effect.
These values stay below their slot capacities and do not change allocator limits.

The probe binds to a free loopback port.
Activation reads pool sizes, the request limit, and Wasmtime resource limits from the embedded `deploy/gcp/host-group-receiving.yaml`.
Warm reuse trusts only the digest of the HTTP route component staged by this developer session.
Rebuild `wamn` after changing these YAML files.

The URL printed by `wamn dev` reaches the host directly.
The [local benchmark](benchmark.md#local-run) adds the deployed nginx edge configuration with a plain HTTP listener.
Its `--no-edge` flag uses the direct host URL for comparison.

The activation unit test `local_host_matches_the_deployed_host_contract` requires Helm.
It renders the bundled release chart with the host-group and release-value functions used by `env apply`.
It compares all host arguments and environment variables with local activation after normalizing equivalent flags and environment names.
It also compares the HTTP workload's store pool, concurrency, and retirement settings.
The test compares the host telemetry variables with deployment. Only the collector endpoint differs locally.
A setting on only one side or an unapproved value difference fails the test.
The test also rejects duplicate environment names and unused exceptions. It contacts no cluster or registry.

## Saved changes and target state

The running loop holds a lease, an exclusive claim on its application database.
A second loop or reset command cannot take that target while the lease remains active.
The first run creates the target from the environment template.
After each successful introspection, the loop records the build inputs and the introspected catalogs beside its local artifacts.
The build inputs are the `inputs.digest` of each package's `build.json` ([building](building.md#wamn-build)).
A restarted developer process keeps the target, unless a change below recreates it.
It also reuses the catalogs when the record matches the current build inputs.

A change to the build inputs of a package runs Migrate, Introspect and Generate again on the kept target.
Cargo controls which selected components need compilation.
Generate runs [`wamn build`](building.md#wamn-build) for each package, the base packages first, without the component build.
Each build describes its statements against its own fresh database on the server of the target database, and it writes `build.json` into the build output.
The Build stage then runs the component build of `wamn build` for each package, and the Virtualize stage selects the components that each `build.json` records.
SQLx metadata is prepared again only when [its inputs](running-tests.md#application-generation-and-sqlx) changed.
The build inputs are the files of the package without `target`, `node_modules`, `generated` and `web/dist`, the lock file of the apps workspace, and the generator identity.
The watcher skips each directory that the Git ignore rules cover.
The watcher watches the package directories, `apps/platform`, the shared platform WIT sources, and the local configuration files of `dev.json`.
Nothing in the loop builds natively, so the root Cargo files and `crates/client` are not inputs.
In a package that authors `wamn.k`, the watcher takes `wamn.k` as the manifest input and `wamn.json` in the build output, `apps/target/wamn/<package>`, as generated output.
A missing `build.json`, or one that records other inputs, prevents reuse.
Receiving and Acme use the shared SQLx CLI 0.9.0 commands and their existing verifier targets.

When the build inputs change, the loop stops the host before it applies them.

Every change keeps the target database and the rows it holds, except the changes below.
The loop applies a kept change, for example a migration file appended after the last applied migration, to the kept target and reads its catalogs again.

These changes recreate the target database:

- an applied migration that the directory edits, removes, or reorders
- a retention change that removes a history table that holds rows

Before it recreates the target, the loop prints the reason.
The new database receives a new target instance, the identity of one database creation.

Control store rows keyed by a non-empty target instance accumulate only within one `wamn dev up` standup.
Nothing deletes them one at a time, and the immutability trigger stays unconditional.
The control store reset of `wamn dev up` is the retention step, outside the recreate path.
A disposable projection made by hand with `wamn-ctl provision-project-env --disposable` against a durable control store has no such bound.

For code and compatible SQL changes, the previous application remains available during candidate preparation.
Build, Gate, and binding failures keep that application running.
A failed stage prints its top context first and every cause under it.
The printed line names the refusal that stopped the stage.
The loop replaces the host only after the candidate passes preparation.
The local Gate uses the same wiring rules as release authoring and authenticates the configured publisher.
A wiring can name a palette component, which is a platform component with a `declaration.json.in` under `apps/platform/*/*/`.
The `wamn build` of a package builds each palette component that a wiring of the package names.
The loop admits it into the scope of that package, with the platform packages that its declaration states.
A `__STORE_ALIAS__` connection takes the `store-alias` of the component's selection in `--local-bindings`.
That is the name that the operator passes to `bind-connection --store-alias` in the cluster.
The runtime still enforces operation grants, connection bindings, and credentials for each database role.

Local preparation limits each metadata or version command to 60 seconds.
On interruption or expiry, the loop gives each command and its child processes five seconds to stop.
After a forced stop, it waits up to five seconds for the command to exit.
It allows another five seconds to close and collect output.
A cleanup timeout reports failure and retains the last diagnostic output.

The local selection file contains one entry for each declared connection:

```json
[
  {
    "package-id": "example_app",
    "component": "example",
    "store-alias": "documents",
    "instance-id": "local_documents",
    "instance": {
      "requirement-type": "blobstore",
      "definition": "/absolute/path/documents.json",
      "credential-handle": "local-documents"
    }
  }
]
```

Replace the example package, component, alias, and instance with the actual declaration and target configuration.
The optional `instance` object creates missing database records through the existing connection provisioning functions.
It creates no external service and supplies no credential material.
The definition file uses the existing blobstore `endpoint`, `container`, and `prefix` fields.
The host resolves the credential handle through its normal credential source.

If the instance already exists, its definition and credential handle must match exactly.
For changed coordinates or a changed handle, select a different instance ID.
Without the `instance` object, the selection requires an existing enabled instance and active connection generation.
The loop watches both the selection file and its absolute definition paths.

To discard the application data manually, stop the loop and reset its target:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev reset --config "$WAMN_DEV_ENV_DIR/dev.json"
```

The reset command refuses an active target lease.
Restart the loop with the same configuration after reset.

## Pushing from a working tree

The loop does not push packages.
To push a package that you changed, run `wamn build` for the package first, and then run `wamn-ctl push-package`.
If the tree changed after the last build, `push-package` refuses and tells you to run `wamn build` ([building](building.md#pushing-a-package)).
If the tree did not change, it pushes the existing build output.
To see what a pushed package holds, run `wamn inspect`.
To compare two pushed versions, run `wamn diff` ([building](building.md#reading-a-pushed-package)).

## Issuer connection

`wamn dev` uses `--pat-issuer`, `--pat-client-cert`, and `--pat-client-key` for the identity issuer.
`--pat-server-ca` supplies an explicit issuer CA.
Their environment names are `WAMN_PAT_ISSUER`, `WAMN_PAT_CLIENT_CERT`, `WAMN_PAT_CLIENT_KEY`, and `WAMN_PAT_SERVER_CA`.
The identity process uses `--operator-ca` or `WAMN_IDENTITY_OPERATOR_CA` for client certificates.
Keep certificate keys and issued tokens out of committed files and published logs.
[Execution](../architecture/execution.md) explains session authority.

## [AGENT-PILOT] Authoring experiment

The pilot measures package authoring from declared task inputs.
Read the [protocol](../../tests/integration/fixtures/agent-pilot/protocol.md) before running or grading it.
`tests/integration/src/agent_pilot` owns preparation, grading, interpretation, and cleanup.

```bash
cargo build --locked --offline -p wamn-gates --bin wamn-gates
cargo test --locked --offline -p wamn-integration-tests --lib agent_pilot:: -- --nocapture
tools/agent-pilot-run all --run 001 --agent claude \
  --task tests/integration/fixtures/agent-pilot/tasks/dock-appointments
```

Choose an unused run identifier. Run only one pilot at a time, separately from cluster tests.
The runner refuses occupied ports `54332`, `4224`, and `4319`.
`CARGO_TARGET_DIR` selects the built native entrypoint, with `target/debug/wamn-gates` as the fallback.
The separate actions are `up`, `launch`, `grade`, and `down`.
`--agent stub` selects the local driver, without completing or grading a Dock implementation.

The runner builds its prepared source into a separate target and records source comparisons and binary hashes.
Keep that source unchanged until `up` succeeds.
Preparation removes grading inputs and this marked section from the measured worktree.
The measured agent cannot push or use `bd` through its supplied environment.

Run data initially lives under `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-pilot/runs`.
Report the command, source, outcome, and whether the agent or only the stub ran.
Stop the owned environment when the run ends:

```bash
tools/agent-pilot-run down --run 001
```

Repeated `down` is safe.
The existing cleanup guard preserves unexported run data and targets still used by another run.
It does not require publishing ordinary test logs.

Use `tools/agent-pilot-report --run 001` only for an explicitly requested grading export.
That exporter requires completed human inputs and refuses credentials that survive its scrubber.
It writes output under `evidence/experiments/agent-authoring/`.

For a recorded run, `tools/agent-pilot-grade --replay RUN_DIRECTORY` preserves its original grading files.
`--placement` and `--contract` also take an explicit recorded run directory.
Missing request logs cannot establish replayed execution.

## Platform UI build

The platform UI lives in `apps/platform_ui` and reads an org-only `config.json`.
Run these commands from the repository root:

```bash
pnpm --filter @wamn/platform-ui check
pnpm --filter @wamn/platform-ui test
pnpm --filter @wamn/platform-ui build
```
The build writes `apps/platform_ui/dist` without an application package or `wamn.k`.
[Building](building.md#platform-ui) owns the platform build record.
[Deployment](deployment.md#platform-ui-files) owns upload and control-host routing.
