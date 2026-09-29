# CDC reader slot

Updated through: 2026-09-29, `main` at `90c7e336d`. Finding `wamn-59z6`.

## 1. Goal

A CDC reader (`services/cdc-reader`) streams the row changes of one project-env database through one logical replication slot. When Postgres invalidates that slot, the changes between the last confirmed position and the next slot are gone. This spec states what the reader does then, how the slot is kept alive, and how `max_slot_wal_keep_size` is set on wamn-dev. The owner accepted it on 2026-09-29 with the rulings of section 7.

## 2. Fixed rules

- A lost slot is not recoverable. A new slot starts at the current WAL position, and the WAL between the old confirmed position and that point is removed. The rows written in that interval produce no events.
- The reader never creates a slot. `enable-cdc-project-env` creates it with `create_failover_slot_sql` (`crates/control/provision/src/sql/cdc.rs:93`).
- No automatic re-slot. No component creates a slot again without an operator.
- No switch. Nothing in this spec adds a mode, a flag or a fallback.
- On invalidation the reader stops, reports the gap boundaries, and requires a full resync of the materialized set before capture resumes.
- Keeping a slot alive is never an operator's job. The reader keeps its slot moving, and the limit covers only a reader that is down.

## 3. Current state

Measured on `main` at `12a4da7b2` and on wamn-dev on 2026-09-29.

| Place | Today |
| --- | --- |
| Slot preflight | Before every session, `preflight_slot` reads `pg_replication_slots` (`services/cdc-reader/src/lib.rs:777`). A missing slot fails with `CAPTURE GAP (slot incident): replication slot … does not exist` (`:789`). A slot with `invalidation_reason` set or `wal_status = lost` fails with `CAPTURE GAP (slot incident): slot … invalidated` (`:799`). |
| Stream errors | `classify` (`:199`) maps a server message that contains `invalidat` or `can no longer be used` to `SlotIncident`. The session loop then fails with `CAPTURE GAP (slot incident) opening the session` (`:1031`) or `… mid-stream` (`:1071`). |
| Result of a gap | `run` returns the error, `main` returns it, and the process exits with a nonzero code. The Deployment restarts it, the preflight fails again, and the pod stays in `CrashLoopBackOff`. The log carries no gap boundary: no confirmed position, no time, no count. The live test `reader_streams_one_project_env_to_the_evt_stream` covers only the missing slot (`services/cdc-reader/tests/event_reader_live.rs:575`). |
| Confirmed position | The reader advances the flushed and applied position only at a `Commit` whose events JetStream acknowledged (`lib.rs:1306`). `pg_walstream` answers a keepalive with that same position (`pg-walstream` rev `0b007cd`, `src/stream.rs:1123-1136`). |
| Other failures | A connection failure in the preflight (`lib.rs:1024`) also ends `run` with an error. A severed session re-opens through `ReopenLadder`. The ladder ends the reader after 10 re-opens with no commit, or after 20 re-opens in 60 seconds (`:254`, `:258`). |
| Headroom alert | `spawn_slot_monitor` (`:1702`) logs `cdc_slot_health` with `safe_wal_size` every 30 seconds, and `CDC_SLOT_WAL_LOW` below 256 MiB (`classify_slot_health`, `:1598`). |
| Resync | No resync or backfill path exists. The slot has no initial snapshot (`cdc.rs:111`). The events start event runs through the materializer (`docs/architecture/execution.md`, application events). |
| Postgres on wamn-dev | One instance, a 10 Gi volume that holds data and WAL together, `max_slot_wal_keep_size` 1 GB (`deploy/gcp/cnpg-cluster.yaml:13`, `:23`, `:28`). `max_wal_size` 1 GB, `wal_keep_size` 512 MB, `archive_mode` on, `archive_timeout` 300 s, segment size 16 MB. |
| Disk on wamn-dev | 1.4 GB used of 9.8 GB, of which `pg_wal` is 929 MB. |

### 3.1 The slots on wamn-dev

Both readers ran without a restart for 3 hours 44 minutes at the time of measurement.

| Slot | Active | `wal_status` | Retained WAL | `confirmed_flush_lsn` |
| --- | --- | --- | --- | --- |
| `wamn_cdc_dkk__receiving__dev__4pqjfmli` | yes | `reserved` | 64 MB | `0/FA006E78` |
| `wamn_cdc_dkk__wms__dev__0nk1lrpr` | yes | `reserved` | 336 MB | `0/E9004398` |

The WMS reader logs `Sent standby status update: received=0/FE000000, flushed=0/E9004398`. It receives the current position but confirms the position of the last WMS commit. Its `safe_wal_size` fell from 1089833976 bytes at 12:57:25 UTC to 738197504 bytes at 16:42:13 UTC. That is about 94 MB per hour, with no WMS write in the interval. The other databases and the archive timeout write that WAL.

## 4. Design

### 4.1 What the reader does on a gap today

It exits with an error that names the slot and says `CAPTURE GAP`. Kubernetes restarts it, and it exits again, without end. That is the "stops for good" of the finding. It reports no boundary, so the operator cannot tell which rows the gap holds. The recovery of `docs/operations/gcp.md` section 6.3 drops the slot and creates it again. The reader then resumes at the new slot like a reader with no gap. No record of the gap remains outside the bead.

### 4.2 A lost slot is not recoverable

Postgres removes the WAL segments that the invalidated slot held (`invalidation_reason = wal_removed`). A new slot begins at the current position. Logical decoding cannot read removed WAL, and the reader has no snapshot. The rows written between the old `confirmed_flush_lsn` and the new slot's first position are therefore not in any event. Only a resync from the tables can bring them back.

### 4.3 What the reader does on invalidation

1. The reader stops capture and does not open a session. It stays running and does not exit.
2. It logs one structured event `CDC_CAPTURE_GAP` per process. The event names the slot and the reason (`missing`, or the `invalidation_reason`). Its log line carries the detection time.
3. Every 30 seconds, on the cadence of the slot monitor, it reads the newest `registry.capture_gap` row of its own `registry.event_readers` row. That read logs nothing.
4. Two conditions let it capture: its slot is healthy, and that row has `resync_at` set or no row exists. It then resumes on the slot that the registry names, with no restart. A new slot alone does not start capture again.

The reader has no readiness probe, in `deploy/gcp/cdc-reader.yaml` or `deploy/gcp/cdc-reader-wms.yaml`, and this spec adds none. The reader stays running and stopped. The gap shows in the `CDC_CAPTURE_GAP` event and in `pg_replication_slots`, and `recover-capture-gap` writes the row. The reader keeps its `SELECT` credential.

The gap record is one row per gap in `registry.capture_gap`. Its key is the `registry.event_readers` row (`org`, `project`, `env`) and its own `created_at`. That `created_at` carries the time of the recovery verb. Its fields:

| Field | Value |
| --- | --- |
| `slot` | The name of the lost slot. |
| `start_lsn` | The invalidated slot's `confirmed_flush_lsn`. For a missing slot, the LSN in the `Nats-Msg-Id` of the last event on the source stream. An empty stream gives null, which means "from registration". |
| `start_at` | The `commit_ts` of the last event on the source stream. When the stream is empty, the `created_at` of the `registry.event_readers` row. |
| `reason` | `missing`, or the `invalidation_reason`. |
| `end_lsn` | The first position of the new slot. |
| `resync_at` | Null until `close-capture-gap` sets it. |

After issue 1, the confirmed position can sit past the last commit, so `start_at` is an earlier bound than the true gap start. The resync of section 6 then reads more rows than the gap changed, and misses none.

Two verbs write the row with the control credential:

- `wamn-ctl recover-capture-gap --org --project --env` drops the lost slot, creates the new slot, updates `registry.event_readers`, and writes the row with its end position. It reads the last source-stream event with the NATS flags of `enable-cdc-project-env`.
- `wamn-ctl close-capture-gap --org --project --env` sets `resync_at` on the newest row. Until the resync of section 6 exists, the operator runs it on the owner's word.

Section 6.3 of the operations page names these two verbs and nothing else.

### 4.4 The slot of an idle database

Section 3.1 shows a second cause of loss. An active reader on a database with no writes never moves its slot, because it confirms only at a commit. The WAL of the whole cluster then counts against that slot. At the measured 94 MB per hour, an idle WMS loses its slot about 11 hours after its last write, with its reader running. On 2026-09-29 the WMS slot had 738 MB of headroom at 16:42 UTC. That runs out at about 00:30 UTC, before the guard stops the cluster at 07:00 UTC.

The fix has two parts. The fork `dkkloimwieder/pg-walstream` changes only so that `next_event` returns a keepalive as an event with its `wal_end`. The reader applies the rule, because only the reader knows that every publish was acknowledged. With no transaction open and the last returned commit acknowledged, the reader confirms the keepalive's `wal_end`. Up to that position, the reader published every change of this database. The rule "the position advances only past acknowledged events" therefore still holds. One live test: an idle environment's slot follows the writes of another database.

### 4.5 `max_slot_wal_keep_size` on wamn-dev

With the fix of section 4.4, the limit covers only a reader that is down or stalled on JetStream. At the idle rate, 1 GB covers about 11 hours of reader downtime, and 4 GB covers about 43 hours. A bulk load writes much faster: the Receiving seed of 1000 wrote more than 1 GB, so no limit covers a reader that is down during a bulk load.

The disk cost is the limit itself plus `max_wal_size`. With 4 GB, `pg_wal` can reach about 5 GB. The data takes about 0.5 GB today, so the volume holds about 5.5 GB at worst and 4.3 GB stays free. A full volume stops Postgres for every environment (section 6.2 of the operations page), which is worse than one lost slot. The limit therefore stays finite, and the proposal is 4 GB. The value is `max_slot_wal_keep_size: "4GB"` in `deploy/gcp/cnpg-cluster.yaml`, and it changes without a restart. A bulk load with the reader down still loses the slot. The gap report of section 4.3 is the answer there, not a larger limit.

### 4.6 A Postgres move against a true gap

When Postgres stops or moves, as on 2026-09-28 in `wamn-fwzc`, the slot stays on the volume and no WAL is written. The reader's preflight fails to connect, the reader exits, and Kubernetes restarts it with a back-off of up to 5 minutes. When Postgres returns, the reader resumes at `confirmed_flush_lsn`, and no row is lost. The only cost is delay. On 2026-09-28 both slots survived the full disk and the volume resize.

A true gap has one of three causes: a slot limit passed, a slot dropped by hand, or a restore that does not carry the slot. In each case the slot is missing or invalidated. The reader tells the two apart already. A connection failure is a restart, and a missing or invalidated slot is `CAPTURE GAP`. The difference that this spec adds is the report and the stop of section 4.3. A reader that restarts after a Postgres move does not report a gap.

## 5. Issues

One branch per issue, in the order 1, 3, 2. Workspace tests only, with live tests on a disposable database.

1. The idle slot (section 4.4). The fork returns a keepalive from `next_event`, and the reader confirms its `wal_end` with no transaction open and the last commit acknowledged. Live test: two databases, the idle one's `confirmed_flush_lsn` advances with the writes of the other.
2. The gap stop and report (section 4.3). The `CDC_CAPTURE_GAP` event with its boundaries, `registry.capture_gap`, the two verbs, and the reader's wait for `resync_at` without a restart. Live tests: an invalidated slot gives one event with the start position. The test sets `max_slot_wal_keep_size` low and writes past it. A missing slot gives one event too. The reader does not capture on a new slot until `close-capture-gap` sets `resync_at`, and then it resumes in the same process.
3. The limit on wamn-dev (section 4.5). Applied on 2026-09-29 in commit `90c7e336d`: the server shows `4GB`, and the WMS slot's `safe_wal_size` rose from 738097816 to 3942627736 bytes. Section 6.3 of the operations page is rewritten for the two verbs with issue 2.

## 6. Out of scope

- The resync itself. Its definition: the events that the gap swallowed are produced again from the tables. Every table carries `updated_at`, so the rows changed in the gap are the rows with `updated_at` between `start_at` and `resync_at`. The resync presents each row as one event at the gap end position, and the materializer runs the same conditions on them. A delete in the gap is not in the tables and stays lost. The resync finding states that.
- A second Postgres instance or failover slot synchronization. wamn-dev has one instance.
- A separate WAL volume.
- The kind cluster values in `deploy/infra/cnpg-cluster.yaml`.

## 7. Owner rulings

Owner rulings of 2026-09-29:

- No operator write to save the WMS slot, and no recorded loss. The 4 GB limit applies at once (issue 3), with no restart.
- The keepalive confirm is issue 1 and comes first. It is the common cause, and the limit passed only because of it.
- A reader with a gap stays running and stopped. It logs `CDC_CAPTURE_GAP` once and reads the gap record every 30 seconds. After `resync_at` is set, it resumes with no restart. No readiness probe exists and none is added. The gap shows in the event and in `pg_replication_slots`.
- 4 GB is right, with the disk cost of section 4.5.
- The definition of the resync is in section 6. Its build is out of scope.
- The names: `registry.capture_gap`, `wamn-ctl recover-capture-gap` and `wamn-ctl close-capture-gap`.
- The gap row (2026-09-29): `start_lsn` and `start_at` come as section 4.3 states, with a null `start_lsn` only for an empty stream. No `detected_at`: the reader's event carries the detection time, and the row's `created_at` carries the verb's time. The reader stays `SELECT` only. The row is keyed by the `registry.event_readers` row and keeps the lost slot's name as a field. One `CDC_CAPTURE_GAP` per process, then a silent read every 30 seconds.
- The keepalive confirm is split (2026-09-29). The fork returns a keepalive from `next_event` as an event with its `wal_end`, and nothing more. The reader applies the confirm rule. The new fork rev is pinned in `Cargo.toml`. A reader that raises its flushed position while idle and relies on the library's cap is out.
