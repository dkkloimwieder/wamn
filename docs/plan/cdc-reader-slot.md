# CDC reader slot

Updated through: 2026-09-29, `main` at `12a4da7b2`. Finding `wamn-59z6`.

## 1. Goal

A CDC reader (`services/cdc-reader`) streams the row changes of one project-env database through one logical replication slot. When Postgres invalidates that slot, the changes between the last confirmed position and the next slot are gone. This spec states what the reader does then, how the slot is kept alive, and how `max_slot_wal_keep_size` is set on wamn-dev. It is a spec for review. No code changes before the owner accepts it.

## 2. Fixed rules

- A lost slot is not recoverable. A new slot starts at the current WAL position, and the WAL between the old confirmed position and that point is removed. The rows written in that interval produce no events.
- The reader never creates a slot. `enable-cdc-project-env` creates it with `create_failover_slot_sql` (`crates/control/provision/src/sql/cdc.rs:93`).
- No automatic re-slot. No component creates a slot again without an operator.
- No switch. Nothing in this spec adds a mode, a flag or a fallback.
- On invalidation the reader stops, reports the gap boundaries, and requires a full resync of the materialized set before capture resumes.

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

1. The reader stops capture and does not open a session.
2. It logs one structured event `CDC_CAPTURE_GAP` with the slot, the reason (`missing`, or the `invalidation_reason`), and the gap start. The start is the slot's `confirmed_flush_lsn` while the invalidated row exists. For a missing slot, the start is the LSN of the last message on the source stream (`Nats-Msg-Id = <project>_<env>:<lsn>`), with that message's time.
3. The gap end is the first position of the next slot. The operator's recovery step records it, because only that step creates the slot.
4. The reader stays stopped until the gap record says that the full resync is done. A new slot alone does not start capture again.

The gap record is one row per gap in the system database, written by the recovery verb with the control credential: slot, start position, start time, reason, detection time, end position, and resync time. The reader reads it as it reads `registry.event_readers` today. The recovery verb replaces the `psql` statements of section 6.3.

### 4.4 The slot of an idle database

Section 3.1 shows a second cause of loss. An active reader on a database with no writes never moves its slot, because it confirms only at a commit. The WAL of the whole cluster then counts against that slot. At the measured 94 MB per hour, an idle WMS loses its slot about 11 hours after its last write, with its reader running. On 2026-09-29 the WMS slot had 738 MB of headroom at 16:42 UTC. That runs out at about 00:30 UTC, before the guard stops the cluster at 07:00 UTC.

The fix: when no transaction is open and every publish is acknowledged, the reader confirms the `wal_end` of a keepalive. Up to that position, the reader published every change of this database. The rule "the position advances only past acknowledged events" therefore still holds. One live test: an idle environment's slot follows the writes of another database.

### 4.5 `max_slot_wal_keep_size` on wamn-dev

With the fix of section 4.4, the limit covers only a reader that is down or stalled on JetStream. At the idle rate, 1 GB covers about 11 hours of reader downtime, and 4 GB covers about 43 hours. A bulk load writes much faster: the Receiving seed of 1000 wrote more than 1 GB, so no limit covers a reader that is down during a bulk load.

The disk cost is the limit itself plus `max_wal_size`. With 4 GB, `pg_wal` can reach about 5 GB. The data takes about 0.5 GB today, so the volume holds about 5.5 GB at worst and 4.3 GB stays free. A full volume stops Postgres for every environment (section 6.2 of the operations page), which is worse than one lost slot. The limit therefore stays finite, and the proposal is 4 GB. The value is `max_slot_wal_keep_size: "4GB"` in `deploy/gcp/cnpg-cluster.yaml`, and it changes without a restart.

### 4.6 A Postgres move against a true gap

When Postgres stops or moves, as on 2026-09-28 in `wamn-fwzc`, the slot stays on the volume and no WAL is written. The reader's preflight fails to connect, the reader exits, and Kubernetes restarts it with a back-off of up to 5 minutes. When Postgres returns, the reader resumes at `confirmed_flush_lsn`, and no row is lost. The only cost is delay. On 2026-09-28 both slots survived the full disk and the volume resize.

A true gap has one of three causes: a slot limit passed, a slot dropped by hand, or a restore that does not carry the slot. In each case the slot is missing or invalidated. The reader tells the two apart already. A connection failure is a restart, and a missing or invalidated slot is `CAPTURE GAP`. The difference that this spec adds is the report and the stop of section 4.3. A reader that restarts after a Postgres move does not report a gap.

## 5. Issues

One branch per issue. Workspace tests only, with live tests on a disposable database.

1. The idle slot (section 4.4). With nothing open and nothing unacknowledged, the reader confirms a keepalive position. Live test: two databases, the idle one's `confirmed_flush_lsn` advances with the writes of the other.
2. The gap stop and report (section 4.3). The `CDC_CAPTURE_GAP` event with its boundaries, the gap record, the recovery verb, and the reader's refusal until the resync time is set. Live tests: an invalidated slot gives one event with the start position. The test sets `max_slot_wal_keep_size` low and writes past it. A missing slot gives one event too. The reader does not capture on a new slot until the record is closed.
3. The limit on wamn-dev (section 4.5). `cnpg-cluster.yaml`, the apply, and the operations page section 6.3 rewritten for the recovery verb.

## 6. Out of scope

- The resync itself. No resync path exists, and "the materialized set" needs a definition for each consumer before a spec can build one. Today the consumers are the event workflows that the materializer starts.
- A second Postgres instance or failover slot synchronization. wamn-dev has one instance.
- A separate WAL volume.
- The kind cluster values in `deploy/infra/cnpg-cluster.yaml`.
