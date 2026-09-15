# Receiving change walkthrough

This record measures one Rust-only Receiving change through `wamn dev`.
It follows owner rulings 1, 4, and 6 of Beads `wamn-llst`.
The run used source `8934079994519c5d89e21a65f6514d6c378e8daf` from a clean worktree.

## Change

The edit changes one refusal message in `apps/wamn_receiving/data/src/operation.rs`.
It replaces `nonempty` with `non-empty` in the guest refusal and in the assertion of its unit test.
The edit changes no SQL, migration, manifest, or generated file.

## Environment

The run used a private PostgreSQL 18 server on a loopback port, with a `wamn_system` database.
Compose project `rcv-rerun-llst5` ran scheduler NATS, Tempo, and an event NATS server with JetStream.
The event server had separate runtime and provisioning users, each with a password file.
The build directory already held `wamn`, `wamn-host`, `wamn-identity`, `http_route.wasm`, and earlier component builds.
Those programs were built from the parent source `3b2de3909`, which differs only in one test file.
The commands below use placeholders for the private values.

## Commands

```bash
WAMN_DEV_ENV_SYSTEM_DATABASE_URL="$SYSTEM_DATABASE_URL" \
"$CARGO_TARGET_DIR/debug/wamn" dev up --root "$WAMN_DEV_ENV_DIR" \
  --nats-url "$SCHEDULER_NATS_URL" --event-nats-url "$EVENT_NATS_URL" \
  --event-nats-username "$EVENT_RUNTIME_USER" \
  --event-nats-password-file "$EVENT_RUNTIME_PASSWORD_FILE" \
  --event-provisioning-username "$EVENT_PROVISIONING_USER" \
  --event-provisioning-password-file "$EVENT_PROVISIONING_PASSWORD_FILE" \
  --stream-replicas 1 --dup-window-secs 120 \
  --tempo-query-url "$TEMPO_QUERY_URL" --otel-exporter-otlp-endpoint "$OTLP_URL" \
  --route-host receiving.localhost --platform-domain example.invalid \
  --flow-http-component "$CARGO_TARGET_DIR/wasm32-wasip2/debug/http_route.wasm" \
  --host-binary "$CARGO_TARGET_DIR/debug/wamn-host" \
  --package "$PWD/apps/wamn_receiving" --overlay-root "$PWD/apps/wamn_receiving"
"$CARGO_TARGET_DIR/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$PWD/apps/wamn_receiving" --watch --hold
"$CARGO_TARGET_DIR/debug/wamn" dev clean-check --repository "$PWD" \
  --package wamn-receiving-data-access \
  --case operation::tests::envelope_requires_all_request_ids_before_item_processing \
  --result "$CHANGE_RESULT"
```

The loop ran in two processes with the same configuration.
The first process served its first run, and then the edit.
`wamn dev clean-check` ran between the two processes, with the edit applied.
The second process served a restart with unchanged source.
It then served a restore edit that put back the original message.

## Results

A loop time starts at the process start or at the file save.
It ends at the `run served` line of that run.
The `wamn dev up` and `wamn dev clean-check` times are the whole command.

| Step | Seconds | Stages that ran | Stages skipped | Target oid |
| --- | ---: | --- | --- | ---: |
| `wamn dev up` | 6.143 | not a loop run | not a loop run | 17080 |
| First run | 33.745 | all ten | none | 18239 |
| Warm edit | 22.323 | build, virtualize, admit, gate, release, activate | migrate, introspect, generate, acl | 18239 |
| Restart with unchanged source | 16.931 | generate, build, virtualize, acl, admit, gate, release, activate | migrate, introspect | 18239 |
| Restore edit | 17.660 | build, virtualize, admit, gate, release, activate | migrate, introspect, generate, acl | 18239 |
| `wamn dev clean-check` | 0.763 | not a loop run | not a loop run | not used |

The loop printed these stage times, in the order of the loop rows above:

```text
watch stage-ms: prepare=127ms migrate=261ms introspect=66ms generate=793ms build=4966ms virtualize=900ms acl=1619ms admit=5970ms gate=439ms release=6263ms activate=12137ms
watch stage-ms: prepare=5ms migrate=0ms introspect=0ms generate=549ms build=4209ms virtualize=867ms acl=546ms admit=4553ms gate=446ms release=5224ms activate=5476ms
watch stage-ms: prepare=113ms migrate=0ms introspect=0ms generate=738ms build=4122ms virtualize=720ms acl=1459ms admit=3776ms gate=360ms release=4400ms activate=1064ms
watch stage-ms: prepare=4ms migrate=0ms introspect=0ms generate=490ms build=4286ms virtualize=873ms acl=484ms admit=4338ms gate=436ms release=5196ms activate=1191ms
```

A skipped stage reports the time that the loop used to find its inputs unchanged.
The loop executed 0 tests.
`wamn dev clean-check` executed 1 test, and it passed.

## Target database

`wamn dev up` created the target database with oid 17080.
The first run found no target schema record, so it recreated the target from the environment template.
The new target has oid 18239.
The warm edit, the restart, and the restore edit all served oid 18239.
After the last run, `pg_database` listed `wamn_system` (16384), the template (18238), and the target (18239).

The restart read the target schema record, which holds the schema inputs and the introspected catalogs.
The record matched, so the restart kept the target and skipped Migrate and Introspect.
A new process runs Generate and Acl again.
Generate wrote the 101 generated files again with the same bytes.
Cargo then compiled the data crate, the component, and the operator crates again.
The SQLx inputs matched the committed inputs, so Generate did not prepare SQLx metadata.

## Work that the loop did not do

The loop has no Publish or Apply stage, and the Compose project had no registry.
The Release stage wrote the release to local files.
A script sampled the processes of each loop session every 0.2 seconds.
No sample showed `cargo sqlx`, `cargo test`, or a test binary.
Generate took 793 ms or less in each run, which leaves no time for SQLx preparation.
The target database kept one creation through the edit and the restart, as the oid list shows.

## Requests

After each run, `POST /location/list` with `[{"request_id":"llst5-valid"}]` returned status 200 and `{"rows":[]}`.
The items `[{}]` and `[{"request_id":""}]` returned status 400 with code `schema-invalid`.
The route refuses those items before the guest runs, so the edited message is not visible over HTTP.
The covering case tests the message.

## Covering case

`wamn dev clean-check` built the library tests of `wamn-receiving-data-access` in the apps workspace.
It ran `operation::tests::envelope_requires_all_request_ids_before_item_processing` with the edit applied.
The result records source `8934079994519c5d89e21a65f6514d6c378e8daf` with uncommitted changes, and it passed.
A second run of the same test binary printed `1 passed; 0 failed; 0 ignored; 0 measured; 39 filtered out`.
That binary contains the edited message.
After the restore edit, `git status` listed no change.

## Correction to the cycle spec

`docs/history/dev-cycle-spec.md` section 1 lists 218 s of database tests and a total of 433 s.
The earlier walkthrough recorded 217.667777 s for the complete PostgreSQL controller.
That value was the wall time of the `pg_virtualenv` run around generation, SQLx preparation, and the scalar test.
Those three commands took 214.667077 s together.
No separate database test run took place.
The old total was about 218 s, not 433 s.

The earlier walkthrough added the nullable text column `location.description`.
Beads `wamn-ri4b` tracks that new-column case through `wamn dev`.
