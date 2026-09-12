# Development loop

The current developer command builds and publishes components before activation.
It requires its configured database, registry, NATS, and identity services.
Local loading without publication and persistent service reuse remain [delivery work](../plan/delivery.md).
[Building](building.md) gives the required build commands.

## [WAMN-DEV-ENVIRONMENT] Developer session

Build `wamn`, `wamn-host`, `wamn-scenario-worker`, and `wamn-identity` before starting a session.
Use only disposable PostgreSQL, scheduler NATS, event NATS, registry, and telemetry services.
The system administrator URL must name `wamn_system` without a query or fragment.
`wamn dev up` resets its control store, so shared or durable targets are unsuitable.

Read the current required arguments from the existing command:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev up --help
```

Supply an explicit environment directory, package roots, built binaries, endpoints, artifact references, and registry authentication file.
The gate listener needs a fixed, unused port rather than port zero.
Supply event runtime credentials separately from provisioning credentials.
Declare `--stream-replicas` and `--dup-window-secs` explicitly.
Use `--event-provisioning-username` and `--event-provisioning-password-file` for stream creation.
Use `--event-nats-username` and `--event-nats-password-file` for runtime access.

The command writes private `dev.json` and prints the next developer command.
Keep `wamn dev up` running in its terminal.
From a second terminal, use its emitted configuration:

```bash
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/client_acme_receiving" --watch --tui receiving
```

For `[RECEIVING-TUI]` and `[GENERATED-TUI]`, `--tui receiving` opens the app-owned operator.
Bare `--tui` opens the developer console. Without a terminal, `--hold` keeps the activation alive until interruption.
The loop supplies the route, target instance, and private personal access token.
Do not copy that token into command arguments.

The watch loop requires local Git references and a reflog.
Make sure that `git config core.logAllRefUpdates` reports `true`.
After changing generator code, rebuild `wamn` and restart the developer process.

The local Receiving terminal test uses an HTTP fixture and requires its built operator:

```bash
cargo build --locked --offline -p wamn-receiving-tui
python3 apps/wamn_receiving/tests/operator_pty.py --binary "$CARGO_TARGET_DIR/debug/wamn-receiving"
```

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
The runner refuses occupied ports `54332`, `5004`, `4224`, `3201`, `4319`, and `8088`.
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
