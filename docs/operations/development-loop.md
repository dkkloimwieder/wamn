# Development loop

The local developer loop builds components and loads them from files.
It keeps the application database for most saved changes.
When the package structure changes, it recreates that database.
Local saves do not publish components or release manifests to a registry.
[Building](building.md) gives the required build commands.

## [WAMN-DEV-ENVIRONMENT] Developer session

Build `wamn`, `wamn-host`, `wamn-identity`, the flow-http component, and the materializer component before starting a session.
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
Supply `--flow-http-component` with the absolute path to the built `http_route.wasm`.
Supply `--materializer-component` with the absolute path to the `materializer.wasm` that `tools/build-components` builds.
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
The materializer does not enter the local admission digest, the release manifest, or the preload.
The host starts it after the flow-http workload runs, and the loop waits until it runs.
The materializer restarts with the host on each loop change. Its JetStream consumers are durable, so a restart resumes them.

The command starts `wamn-identity`, writes private `dev.json`, prints the next developer command, and exits.
The identity process keeps running across application builds.
Startup creates its signing key once. Application rebuilds preserve that key and the identity database.
The `session_identity` field supplies the issuer address, certificate authority file, and environment instance to each application host.
The identity process uses the existing [password configuration](deployment.md#identity-password-configuration), including `.env` in the working directory.

At environment teardown, stop the owned identity process:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev down --root "$WAMN_DEV_ENV_DIR"
```

This command removes the owned process, its database login authority, and its private certificate directory.
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

Add `--tui` to open the developer console. Without a terminal, `--hold` keeps the activation alive until interruption.

The loop runs from a Git worktree.
After changing generator code, rebuild `wamn` and restart the developer process.

## Saved changes and target state

The running loop holds a lease, an exclusive claim on its application database.
A second loop or reset command cannot take that target while the lease remains active.
The first run creates the target from the environment template.
After each successful introspection, the loop records the schema inputs and the introspected catalogs beside its local artifacts.
A restarted developer process keeps the target when that record names the same package structure.
It also reuses the catalogs when the record matches the current schema inputs.
Otherwise the first run of the new process recreates the target.

Rust code saves reuse the schema, database rows, and unchanged generated files.
Cargo controls which selected components need compilation.
Named SQL and operation contract changes refresh generation against the retained database.
SQLx metadata is prepared again only when [its inputs](running-tests.md#application-generation-and-sqlx) changed.
Changes to generator inputs, dependency locks, checker tools, or grants also invalidate the corresponding generated state.
The generator inputs are the files of the package, without `node_modules` and without any directory that the Git ignore rules cover, such as `web/dist`.
The watcher uses the same ignore rules.
In a package that authors `wamn.k`, the watcher takes `wamn.k` as the manifest input and `wamn.json` in the build output, `apps/target/wamn/<package>`, as generated output.
Missing or changed generated files prevent reuse.
Receiving and Acme use the shared SQLx CLI 0.9.0 commands and their existing verifier targets.

Schema inputs include migrations, package identity, model structure, and internal relations.
When these inputs change, the loop stops the host before it applies them.

These changes keep the target database and the rows it holds:

- a migration file appended after the last applied migration
- a changed `wamn.json` or `wamn.k` at the same package coordinate
- a change to `enum_fields`, `server_owned_fields`, or `audit_log`

The loop applies them to the kept target and reads its catalogs again.

These changes recreate the target database:

- a new model or a new internal relation
- a change to `field_owners`, `constraint_owners`, or `client_field_extensible`
- a package version bump or a changed `predecessor_version`
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
`tools/build-components` builds each palette component that a wiring of a selected package names.
The loop admits it into the scope of that package, with the platform packages that its declaration states.
A `__STORE_ALIAS__` connection takes the `store-alias` of the component's selection in `--local-bindings`.
That is the name that the operator passes to `bind-connection --store-alias` in the cluster.
The runtime still enforces operation grants, connection bindings, and credentials for each database role.

Local preparation limits each metadata or version command to 60 seconds.
SQLx preparation, component builds, and virtualization each have a 45-minute limit.
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

## Clean correctness checks

`wamn dev clean-check` runs the existing exact test for a change, independent of the retained developer database.
It takes the arguments and result rules of the [change checks](delivery.md#change-checks).
Local reuse does not establish [release qualification](delivery.md#candidate-qualification).

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
The runner refuses occupied ports `54332`, `4224`, `3201`, and `4319`.
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
