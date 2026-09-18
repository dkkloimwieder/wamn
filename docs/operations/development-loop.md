# Development loop

The local developer loop builds components and loads them from files.
It keeps the application database for most saved changes.
When the package structure changes, it recreates that database.
Local saves do not publish components or release manifests to a registry.
[Building](building.md) gives the required build commands.

## [WAMN-DEV-ENVIRONMENT] Developer session

Build `wamn`, `wamn-host`, `wamn-identity`, and the flow-http component before starting a session.
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
The emitted `local_artifacts` configuration names the local output directory and this component.
For declared connections, pass `--local-bindings` with an absolute path to the selection file.
The emitted configuration stores that path in `local_artifacts.bindings`.

Supply `--platform-domain` with the domain of the platform principal emails, for example `example.invalid`.
The emitted configuration stores it in `platform_domain`.
Supply event runtime credentials separately from provisioning credentials.
Declare `--stream-replicas` and `--dup-window-secs` explicitly.
Use `--event-provisioning-username` and `--event-provisioning-password-file` for stream creation.
Use `--event-nats-username` and `--event-nats-password-file` for runtime access.

The command starts `wamn-identity`, writes private `dev.json`, prints the next developer command, and exits.
The identity process keeps running across application builds and operator exits.
Startup creates its signing key once. Application rebuilds preserve that key and the identity database.
The `session_identity` field supplies the issuer address, certificate authority file, and environment instance to each application host.
The operator terminal uses that issuer for password login and receives no PAT from the launcher.
An older configuration without `session_identity` retains its explicit `operator_bearer_token` path.
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
  --overlay-root "$PWD/apps/client_acme_receiving" --watch --tui receiving
```

Use this Acme overlay command for an Acme change.
For a Receiving change, give `wamn dev up` only `--package "$PWD/apps/wamn_receiving"` as its package root.
Then run the loop with Receiving as the overlay:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/wamn_receiving" --watch --tui receiving
```

This loop migrates, generates, builds, and releases only the Receiving package.

For `[RECEIVING-TUI]` and `[GENERATED-TUI]`, `--tui receiving` opens the app-owned operator.
Bare `--tui` opens the developer console. Without a terminal, `--hold` keeps the activation alive until interruption.
The loop supplies the route, target instance, and private personal access token.
Do not copy that token into command arguments.

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
Missing or changed generated files prevent reuse.
Receiving and Acme use the shared SQLx CLI 0.9.0 commands and their existing verifier targets.

Schema inputs include migrations, package identity, model structure, and internal relations.
When these inputs change, the loop stops the operator and host before it applies them.

These changes keep the target database and the rows it holds:

- a migration file appended after the last applied migration
- a changed `wamn.json` at the same package coordinate
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
The operator clears records, revisions, cursors, drafts, and pending submissions for the previous instance.
It does not replay interrupted mutations.

Control store rows keyed by a non-empty target instance accumulate only within one `wamn dev up` standup.
Nothing deletes them one at a time, and the immutability trigger stays unconditional.
The control store reset of `wamn dev up` is the retention step, outside the recreate path.
A disposable projection made by hand with `wamn-ctl provision-project-env --disposable` against a durable control store has no such bound.

For code and compatible SQL changes, the previous application remains available during candidate preparation.
Build, Gate, and binding failures keep that application running.
A failed stage prints its top context first and every cause under it.
The printed line names the refusal that stopped the stage.
The loop replaces the host and operator only after the candidate passes preparation.
The local Gate uses the same wiring rules as release authoring and authenticates the configured publisher.
The runtime still enforces operation grants, connection bindings, and credentials for each database role.

Local preparation limits each metadata or version command to 60 seconds.
SQLx preparation, component builds, virtualization, and native operator builds each have a 45-minute limit.
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

The local Receiving terminal test uses an HTTP fixture and requires its built operator:

```bash
cargo build --locked --offline -p wamn-receiving-tui
python3 apps/wamn_receiving/tests/operator_pty.py --binary "$CARGO_TARGET_DIR/debug/wamn-receiving"
```

## Receiving password login

The [Receiving operator guide](../../apps/wamn_receiving/operator-guide.md) covers sign-in, receipt entry, keyboard controls, history, recovery, and logout.

Build `wamn-receiving` before starting the terminal.
Configure `WAMN_BASE_URL`, `WAMN_HOST` when needed, and `WAMN_TARGET_INSTANCE` for the selected deployment.
Set `WAMN_SESSION_ISSUER` to its HTTPS identity issuer and `WAMN_SESSION_AUDIENCE` to the exact provisioned environment audience.
For a private issuer CA, set `WAMN_SESSION_CA` to its certificate bundle.
The bundle replaces the default trust roots for identity requests.

If `WAMN_TOKEN` is set, the terminal retains the existing PAT path.
For password login, unset `WAMN_TOKEN` before launching `wamn-receiving`.
Enter `I` to accept an invitation, `L` to log in, or `R` to recover your password.
Invitation acceptance asks for one complete emailed code and a new password entered twice.
The code carries the account binding. You do not copy a principal UUID or create a PAT.
After enrollment, enter the email and password again to log in.
Menu choices and email addresses are visible. Passwords and email secrets display only masking characters.
Paste is supported, and Esc or Ctrl-C cancels.
For recovery, enter your email, the emailed reset secret, and your new password twice.
After reset, use normal login. A notification delivery failure does not undo the password change.
Do not put human passwords or invitation secrets in arguments, environment variables, or files.

For several environments, set `WAMN_RECEIVING_TARGETS` to a public JSON file with these fields:

```json
[
  {
    "audience": "<exact provisioned environment audience>",
    "base_url": "https://receiving.example.com/",
    "host": "receiving.example.com",
    "target_instance": "<served activation instance>"
  }
]
```

Add one entry for each configured environment. The `host` field is optional.
This file replaces the single-environment address and audience variables for password login.
Keep `WAMN_SESSION_ISSUER` and any required `WAMN_SESSION_CA` configured.
All entries use that issuer. The file contains no credentials.

After password authentication, the terminal matches authorized environments against this file.
One match opens directly. Several matches produce a numbered choice.
No matches refuse login. The issuer repeats authorization when it issues the selected session.
For PAT access, unset `WAMN_RECEIVING_TARGETS` and use the single-environment variables.

The terminal keeps access and renewal credentials only in process memory.
When an application request needs a new access token, the client renews once before submission.
Concurrent requests share that renewal. Idle terminals send no renewal requests.
After renewal failure, inactivity expiry, or absolute expiry, exit and log in again before explicitly submitting the operation.
The client never replays an application operation automatically.
Quitting clears local credentials and asks the issuer to revoke the login.
If logout fails, the terminal reports that server revocation is unconfirmed.
After server logout commits, issued password tokens fail the next request admission.
All permitted application operations accept the renewable session without another password prompt.

The password terminal test uses owned HTTPS and HTTP fixtures, plus OpenSSL for a disposable certificate:

```bash
python3 apps/wamn_receiving/tests/password_login_pty.py --binary "$CARGO_TARGET_DIR/debug/wamn-receiving"
```

This test covers hidden input, enrollment, login, environment selection, expiry, local logout, cancellation, and refusal.
It does not establish the deployed Receiving journey.

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
