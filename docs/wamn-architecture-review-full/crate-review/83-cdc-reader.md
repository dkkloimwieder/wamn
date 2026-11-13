# 83 wamn-cdc-reader

Scope: `services/cdc-reader` at `epic/dep7` (b94833fd): `src/lib.rs` (3,070), `src/main.rs` (86), `tests/event_reader_live.rs` (1,257). One process per project environment: a `pg_walstream` replication session that publishes row events to JetStream and advances the slot only after acks. Nothing built or run.

## Earlier work checked first

- Tab 03: `wamn-event-wire` is the wire contract. Tab 68: CDC and event modules go to one events owner. Tab 79 1 B: the host admits events natively.
- Tab 62 1 A: one config file per component. Tab 65: provisioning steps are internal; CNPG managed roles.
- Tab 71 2 A: the capture gap verbs become control routes. Tab 78: TLS on every PostgreSQL connection; SQLx replaces `tokio_postgres` (U2). Tab 82: names ordered org, environment, project.
- Owner 2026-11-12: too restrictive at apply and deploy; one owner per fact.

## Files read

- `lib.rs`: header, `EventReaderArgs`, item list, `walsender_url`, `preflight_url`, `read_registration`, `connect_system`, relation classification constants.
- `Cargo.toml`.

## Callers read

- Repository grep for `wamn_entities`, `wamn_cdc_exclusions`, `CONTROL_OWNED_RELATION_TABLES`.

## One purpose per file

- `lib.rs`: arguments, registration read, preflight, capture gap wait, session open and re-open ladder, transaction drain, JetStream publish and ack, stall and slot monitors.
- `main.rs`: parse arguments and run.

## Findings

**1. The reader re-checks what provisioning created.**

Fact: before every session the reader compares the server version (floor 17), the publication (operations and scope) and the slot (plugin, lifetime, two-phase, failover) with the provisioning declaration in `wamn_control_provision::sql::cdc`, and refuses on any difference (`CAPTURE SHAPE DRIFT`). It also takes `--stream-replicas` and the duplicate window as arguments that are "asserted against a pre-existing stream, not applied". Provisioning creates the publication, slot and streams from its own declaration.

Options:

- A: delete the shape and stream checks and their arguments; provisioning is the one owner of that shape.
- B: keep them.

Recommend A. Inference: a shape that differs can come only from a change outside provisioning.

**2. A per-environment reader holds a system database login.**

Fact: the reader connects to the system database (`WAMN_SYSTEM_URL`, a `wamn_registry_reader` login) to read its own `registry.event_readers` row (publication, slot, stream) and, every 30 seconds while a capture gap is open, its newest `registry.capture_gap` row; it resumes when an operator sets `resync_at`.

Options:

- A: the publication, slot and stream names come from the reader's config file (tab 62 1 A). On a missing or invalid slot the reader exits with an error; the operator's recovery route repairs the slot and restarts the reader. The system database login, the registration read and the gap poll go.
- B: keep the system database read.

Recommend A: the reader keeps only its replication login and its NATS credential.

**3. The control-owned relation names are defined four times.**

Fact: `wamn_entities` and `wamn_cdc_exclusions` are constants in `wamn-catalog::manifest::CONTROL_OWNED_RELATION_TABLES`, in `cdc-reader/src/lib.rs` (two constants), and in `wamn-schema-introspection::postgres::ENTITY_MAP_TABLE`.

Options:

- A: one definition in `wamn-catalog`; the others import it.

Recommend A.

## Follows from earlier rulings

- Tab 78: `--sslmode` defaults to `disable` and both connections use `NoTls`; TLS becomes required. U2: SQLx for the preflight and system reads; `pg_walstream` stays for the replication session.
- Tab 71 2 A: `recover-capture-gap` and `close-capture-gap` become control routes.
- Tab 82: `Nats-Msg-Id` (`<project>_<env>:<lsn>`) and stream subjects follow the org, environment, project order.
- Tab 72 3 A: the header's bead ids, plan labels and "MVP outcome".

## Fine as is

- Ack-before-advance at transaction granularity, the re-open ladder, the stall and slot-headroom monitors.

## Tests deleted, changed or moved

- Finding 1 A: shape-drift and stream-assertion cases in `event_reader_live.rs` and the `lib.rs` unit tests.
- Finding 2 A: registration and gap-poll cases; a gap test checks the reader exits and resumes after restart.

## Not read

- `drain`, `publish_txn`, the monitors and the re-open ladder bodies; the live test bodies.

## Rulings

- 2026-11-12: all findings A (shape and stream re-checks go; registration from the config file, no system database login; one definition of the control-owned relation names).
