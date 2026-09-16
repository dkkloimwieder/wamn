# Receiving change walkthrough

This file holds two records of Receiving changes through `wamn dev`.
The first record measures one Rust-only change.
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

The restart row comes from a later test run at source `dbfdd492bac0da4518ee7ad021b5c23cb65ee379`, from a clean worktree.
At that source, Generate compares each generated file with the file on disk and writes only the files that differ.
Cargo built `wamn` and `wamn-host` from that source before the test run.
The test run used the same commands in a new environment of the same kind, with Compose project `rcv-rerun-llst8`.
One process served a first run and stopped.
A second process then served the restart with unchanged source.

| Step | Seconds | Stages that ran | Stages skipped | Target oid |
| --- | ---: | --- | --- | ---: |
| `wamn dev up` | 6.143 | not a loop run | not a loop run | 17080 |
| First run | 33.745 | all ten | none | 18239 |
| Warm edit | 22.323 | build, virtualize, admit, gate, release, activate | migrate, introspect, generate, acl | 18239 |
| Restart with unchanged source | 19.869 | generate, build, virtualize, acl, admit, gate, release, activate | migrate, introspect | 18239 |
| Restore edit | 17.660 | build, virtualize, admit, gate, release, activate | migrate, introspect, generate, acl | 18239 |
| `wamn dev clean-check` | 0.763 | not a loop run | not a loop run | not used |

The loop printed these stage times, in the order of the loop rows above:

```text
watch stage-ms: prepare=127ms migrate=261ms introspect=66ms generate=793ms build=4966ms virtualize=900ms acl=1619ms admit=5970ms gate=439ms release=6263ms activate=12137ms
watch stage-ms: prepare=5ms migrate=0ms introspect=0ms generate=549ms build=4209ms virtualize=867ms acl=546ms admit=4553ms gate=446ms release=5224ms activate=5476ms
watch stage-ms: prepare=217ms migrate=0ms introspect=0ms generate=885ms build=785ms virtualize=1022ms acl=1541ms admit=4657ms gate=629ms release=6640ms activate=2872ms
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
The SQLx inputs matched the committed inputs, so Generate did not prepare SQLx metadata.
In the later test run, the first run recreated the target as oid 18239, and the restart served oid 18239.

In the restart, Generate compared the 101 generated files with the files on disk and wrote none of them.
The files kept their modification times, so Cargo compiled no crate.
Build took 785 ms, and no `rustc` process ran.
A fingerprint file is the record that Cargo uses to find changed crates.
No Cargo fingerprint file changed in the build directory.

The Admit, Release, and Activate stages took longer than in the earlier test run, so the restart took 19.869 s in total.
The computer also ran unrelated work, with a one-minute load average of 3.9 at the restart start and 4.7 after its end.

## Work that the loop did not do

The loop has no Publish or Apply stage, and the Compose project had no registry.
The Release stage wrote the release to local files.
A script sampled the processes of each loop session every 0.2 seconds.
No sample showed `cargo sqlx`, `cargo test`, or a test binary.
Generate took 885 ms or less in each run, which leaves no time for SQLx preparation.
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

## Column change record

This record measures the nullable column `location.description` through the same loop.
The run used source `dd7fe6479962edb20a6c16ac3556f834546e2a3a` with the column patch uncommitted.
The patch is never committed, as owner ruling 6 of Beads `wamn-ri4b` states.

The run used a private PostgreSQL 18 server on a loopback port, with a `wamn_system` database.
Compose project `rcv-column` ran scheduler NATS, Tempo, and an event NATS server with JetStream.
`wamn dev up` took a new environment template, because the changed catalog SQL reaches only a new template.
The loop ran with `apps/wamn_receiving` as its only package and as its overlay root.

### Patch files

The save batch wrote six authored files:

- `migrations/0002_location_description.sql`, one statement `ALTER TABLE receiving.location ADD COLUMN description text;`
- `query/location.sql`, which selects the new column
- `wamn.json`, which declares the result field, the select field, and the statement row
- `data/src/operation.rs`, the `LocationValue` field and its unit test
- `tests/generation.rs`, the fixture column and the ACL select set
- `tests/operator_pty.py`, the result descriptor set

The loop then wrote thirteen generated files and two SQLx metadata files.
`tests/.sqlx/query-35923fd6….json` went away and `tests/.sqlx/query-b5c60b00….json` arrived.
The whole change set was 21 paths, all under `apps/wamn_receiving`.

### Results

A loop time starts at the process start or at the file save.
It ends at the `run served` line of that run.
The `wamn dev up` and `wamn dev clean-check` times are the whole command.

| Step | Seconds | Stages that ran | Stages skipped | Target oid |
| --- | ---: | --- | --- | ---: |
| `wamn dev up` | 5.071 | not a loop run | not a loop run | not sampled |
| First run | 29.535 | all ten | none | 18239 |
| Column save | 27.310 | all ten | none | 18239 |
| Restart with unchanged source | 47.311 | generate, build, virtualize, acl, admit, gate, release, activate | migrate, introspect | 18239 |
| `wamn dev clean-check` | 10.074 | not a loop run | not a loop run | not used |

The loop printed these stage times, in the order of the loop rows above:

```text
watch stage-ms: prepare=130ms migrate=261ms introspect=56ms generate=769ms build=4558ms virtualize=750ms acl=1463ms admit=3961ms gate=399ms release=5558ms activate=11417ms
watch stage-ms: prepare=128ms migrate=241ms introspect=64ms generate=2317ms build=5132ms virtualize=1051ms acl=1730ms admit=4481ms gate=475ms release=5571ms activate=5660ms
watch stage-ms: prepare=748ms migrate=0ms introspect=0ms generate=7023ms build=2261ms virtualize=2945ms acl=6203ms admit=11042ms gate=991ms release=11991ms activate=3032ms
```

Every command returned exit status 0.
The one-minute load average was 4.45 at the first run, 5.11 at the column save, and 11.72 at the restart.
Other work compiled Rust on the same computer during the restart, which explains its longer stages.

### Target database

The first run recreated the target from the environment template and served oid 18239.
The column save kept that target: the served `target_instance` stayed 18239, and `pg_database` listed the same four databases.
The seeded row `DOCK-1` survived the save.
`catalog.package_migrations` then held two rows for `wamn_receiving@1.0.0`, `migrations/0001_initial.sql` and `migrations/0002_location_description.sql`.
`catalog.effective_release_packages` already held `wamn_receiving@1.0.0` from the first run.
The release seal was therefore active, and the local exception recorded the second migration.
The local target comment held the marker and the current manifest hash of the package.
`information_schema.columns` showed `description` as nullable with no default.

### SQLx preparation

A script sampled the processes of each loop session every 0.2 seconds.
The first run ran no `cargo sqlx prepare`.
The column save and the restart each ran `cargo sqlx prepare -- --test receiving_sqlx_verifier --locked --offline`.
The Acme verifier never prepared, because the run selected only the Receiving package.
No sample showed `cargo test` or a test binary inside a loop session.

### Application result

Before the save, `POST /location/list` with `[{"request_id":"ri4b-valid"}]` returned the two old fields.
After the save, the same request returned status 200 and `"description":null` for `DOCK-1`.
An `UPDATE` then set the description over SQL, and the same request returned `"description":"North loading dock"`.
The restart served the same row and the same text.

### Covering case

`wamn dev clean-check` ran `operation::tests::bounded_projection_rows_preserve_the_declared_wire_scalars`.
The result records source `dd7fe6479962edb20a6c16ac3556f834546e2a3a` with uncommitted changes, and all four checks passed.
After the run, every authored file, generated file, and `tests/.sqlx` entry went back to the committed bytes, and `git status` listed no change.

### One refusal and its fix

The first attempt of this run refused at Introspect with `dev-stage-owner-failed while introspect package`.
A kept target runs Migrate and Introspect again, so the catalog reader saw the grants that the loop itself had written.
The reader refused the `receiving` schema and the granted columns, because they carry an ACL entry for the application role.
Commit `dd7fe6479` lets the reader skip the ACL entries of the audit retention role and of the application role by name.
A grant to any other role still refuses.

A later lane commit, `856267331`, also changed the ownership preflight of one apply.
A stream that creates a relation and then adds a column to it now applies to an empty database.
None of the four steps above runs that path, so the times stand as recorded.
