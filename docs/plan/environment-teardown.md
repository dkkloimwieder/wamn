# Environment teardown

Updated through: 2026-09-30, `main` at `0836e2cd2`.

## 1. Goal

One verb, `delete-project-env`, deletes one project environment and everything that its instance names. It replaces the hand procedure of section 6.7 of `docs/operations/gcp.md` (finding `wamn-psss`). After the verb, `provision-project-env` for the same triple mints a new instance suffix, and the package, gate and CDC steps run again as written.

The verb adds no new rule. It runs the steps of the hand runs of 2026-09-28 and 2026-09-29 in one order. It adds the three catalog tables that section 3 finds missing. The second epic of the platform UI (`wamn-zua8`) waits on this verb.

## 2. Fixed rules

- One run deletes one triple, `--org`, `--project` and `--env`. No run deletes a project, an org or a list.
- The verb computes every name from the registry row and the naming functions. It reads no name from a Kubernetes object or from the database ACL. A second run after a failed first run finds the same names.
- Every step is safe to run twice. A missing object is a finished step, not a refusal.
- The registry rows go last. Until they go, a second run can still read the instance suffix and the tenant.
- The database and its CDC objects go before any role. The control rows go before the registry rows.
- The verb turns off an immutability trigger for its one delete statement only, inside one transaction, and turns it on again before the commit. No other session sees the trigger off.
- The event stream is deleted, not purged. No NATS permission is added: the provisioning user of the stream already holds `STREAM.DELETE` and `CONSUMER.DELETE` (`test-support/infrastructure/event_broker.rs:101-131`).
- The verb prints each step with its result on one line. It prints no credential.
- Without `--confirm`, the verb prints the plan and the names that a confirmed run deletes. It changes nothing.

## 3. Current state

Measured on `main` at `0836e2cd2`, and on wamn-dev, on 2026-09-30.

| Place | Today |
| --- | --- |
| Hand procedure | Section 6.7 of `docs/operations/gcp.md`. It removed Receiving `zf7o454t`, Receiving `ra3cel54` and WMS `bnarqpnc` on 2026-09-28 and 2026-09-29. Each step took 1 second or less, and the stop of the workloads took 23 seconds. |
| Registry row | `registry.project_envs` (`deploy/sql/system-schema.sql:556`), key `(org, project, env)`. `upsert_project_env_sql` never changes a stored suffix (`crates/control/registry/src/sql.rs:160-169`), so a new suffix needs the row gone. |
| Rows that cascade | `identity.project_env_memberships`, `registry.event_readers` and `provisioning.dumps` have `ON DELETE CASCADE` to `registry.project_envs`. `registry.capture_gap` cascades from `registry.event_readers`. |
| Retired suffix | The trigger `project_envs_retire_instance` runs `BEFORE DELETE` on `registry.project_envs` and inserts `org`, `project`, `env` and `instance_suffix` into `registry.retired_project_envs`, with `retired_at` from `now()` (`system-schema.sql:635-655`). The table has no foreign key. wamn-dev holds three rows. No code reads the table when a suffix is minted (`system-schema.sql:619-621`). |
| Tenant row | `catalog.tenant_environments` (`deploy/sql/control-portable-store.sql:254`), key `tenant_id`, with the triple and the suffix. No foreign key and no trigger. |
| Control rows | Nine tables in `wamn_system` key their rows by `tenant_id`, and eight have a `<table>_immutable` trigger that refuses `UPDATE` and `DELETE` (`control-portable-store.sql:370-381`). Their foreign keys have no `ON DELETE` clause (section 4.2 gives the order). |
| Missing from the hand list | `catalog.package_migrations` and `catalog.effective_release_packages` (both immutable) and `catalog.effective_release_heads`. Rows in them make the hand order fail on a foreign key. `wamn_authority.author_login_tenants` (`control-portable-store.sql:390`) maps each control-scoped login to its tenant and triple. wamn-dev holds 6 rows, one of them for the aborted registry reader generation `b` of 2026-09-30. |
| Gate rows | `catalog.authoring_command_audit` and `wamn_run.gate_reports`, both immutable. A leftover row refuses the next gate with `command-id-reuse`. |
| Database | `wamn-db-<org>--<project>--<env>--<instance>` (`crates/control/provision/src/name.rs:182`). |
| CDC objects | One name, `cdc_object_name`, serves the publication, the slot and the replication role (`name.rs:364-385`). `drop_replication_slot_sql` and `drop_publication_sql` exist (`crates/control/provision/src/sql/cdc.rs:219-226`). |
| Generation roles | `<prefix>_<40 hex scope hash>_<a or b>` (`crates/control/provision/src/workload_role.rs:495-505`). The scope of the project-database families names the database, so their names change with the suffix. The control families (ControlAuthor, RegistryReader, IdentityReader) hash no suffix, so a new provision of the triple uses the same names. A retire needs a live replacement of the same scope, and an abort needs the database (`crates/control/lib/src/provision_project_env/workload.rs:784-871`). No production code runs `DROP ROLE`. |
| Event plane | Source stream `EVT_<len>_<org>_<len>_<project>_<len>_<env>` and advisory stream `WAMN_EVENT_ADVISORIES_<source>`. Materializer consumers live on the source stream. Stream and consumer deletes exist in tests only. The hand procedure deletes the source stream, which deletes its consumers, and leaves the advisory stream. |
| Kubernetes | No verb has a Kubernetes client. The CloudNativePG `Database` has `ensure: present`. If it stays, it creates the database again. The Secrets carry triple names without the suffix: `wamn-db-`, `wamn-cdc-`, the family Secrets and the two PAT Secrets. |
| Service principals | The operator and management author services of a triple, and their PATs, are not named by the instance. The hand procedure kept them. |
| Similar verbs | `copy-project-env --deprovision-old --confirm` in `wamn-ctl-ops` (`services/ctl/src/ops_verbs.rs:246-250`), and `recover-capture-gap`, which drops a slot. |

## 4. Design

### 4.1 The verb

`wamn-ctl-ops delete-project-env`, beside `copy-project-env`, because it destroys data. Flags, with the names and variables of the existing verbs:

| Flag | Variable | Use |
| --- | --- | --- |
| `--org`, `--project`, `--env` | | The triple. |
| `--system-database-url` | `WAMN_SYSTEM_ADMIN_URL` | Superuser URL of `wamn_system`. The verb sets `ROLE wamn_system` for the registry and control rows. |
| `--admin-database-url` | `WAMN_PG_ADMIN_URL` | Superuser URL of the `postgres` database of the target cluster, for the slot, the database and the roles. |
| `--nats-url`, `--nats-username`, `--nats-password-file` | `WAMN_EVT_NATS_*` | The provisioning user of the stream. |
| `--confirm` | | Without it, the verb prints the plan and changes nothing. |

### 4.2 The steps

The verb reads the registry row, the tenant row and the event reader row first. It then runs these steps in this order:

1. Refusals of section 4.3.
2. Delete each materializer consumer of the source stream, then the source stream. Answer "not found" as done.
3. Drop the replication slot, then the database `WITH (FORCE)`. The publication goes with the database.
4. Drop the CDC role and the generation roles of the project-database families, `a` and `b` of each, with one `DROP ROLE IF EXISTS` each. The names come from `workload_generation_role` with the database of this instance.
5. In one transaction, delete the control rows of the tenant, leaves first: `deployment_attestations`, `connection_requirements`, `component_library`, `effective_release_heads`, `effective_release_packages`, `effective_releases`, `package_migrations`, `packages`, `authoring_command_audit`, `wamn_run.gate_reports`. Each immutable table gets `DISABLE TRIGGER`, the delete, and `ENABLE TRIGGER`.
6. In one transaction, delete `registry.project_envs` and `catalog.tenant_environments` for the triple. The trigger records the suffix, and the cascade removes the memberships, the event reader, the capture gap rows and the dumps.
7. Print the Kubernetes objects that the operator deletes: the CloudNativePG `Database` and the Secrets of section 3, with their namespaces.

### 4.3 Refusals

Each refusal names its reason on one line and changes nothing.

- No `registry.project_envs` row for the triple.
- `--admin-database-url` reaches a database other than `postgres` on the cluster of the registry row.
- The replication slot is active: a reader still streams from it.
- The database has a session of a login other than the verb.
- A `catalog.tenant_environments` row names the tenant with another triple or another suffix.

### 4.4 What `registry.retired_project_envs` records

One row for each run: `org`, `project`, `env`, the deleted `instance_suffix`, and `retired_at`. The trigger writes it, not the verb. A second run after a finished run finds no registry row and refuses, so the table never gets a second row for one suffix.

### 4.5 What stays

- The org, the project, `identity.project_roles` and every human principal.
- The control-family generation roles and their `author_login_tenants` rows, because a new provision of the triple uses the same names (question 3 of section 7).
- The service principals of the triple and their PATs (question 4 of section 7).
- The advisory stream (question 2 of section 7).

## 5. Issues

One branch. Each issue lands with its tests.

1. The library and the verb: the read, the plan output, the refusals and the seven steps, in `crates/control/lib` and `services/ctl`. Unit tests for the plan and the delete order.
2. A live test on a disposable Postgres 18 and a disposable NATS: provision a triple, enable CDC, publish a package and run a gate, run the verb, then provision the triple again. The new suffix differs, `registry.retired_project_envs` holds the old one, the next publish and gate pass, and a second verb run refuses with "no registry row".
3. The operations page: section 6.7 becomes the verb run and the Kubernetes deletes. A run on wamn-dev waits for the next teardown that the owner orders.
4. Closeout: close `wamn-psss` with the commit and the test run, and report this plan as done.

## 6. Out of scope

- Refusing a retired suffix at mint time. No code reads `registry.retired_project_envs` today, and this verb does not change that.
- Deleting a project or an org.
- A Kubernetes client in the control verbs.
- Record history and run history of the environment. They live in the dropped database.

## 7. Questions for the owner

1. Is `wamn-ctl-ops`, beside `copy-project-env`, the right binary, given that the production image builds only `wamn-ctl` without features?
2. Does the verb delete the advisory stream `WAMN_EVENT_ADVISORIES_<source>` too? The hand procedure left it, and a test requires that it survives a delete of the source stream.
3. Do the control-family generation roles and their `author_login_tenants` rows stay, as section 4.5 says, or go?
4. Do the service principals and PATs of the triple stay, as the hand procedure did?
5. Is step 7 right: the verb prints the `Database` object and the Secrets, and the operator deletes them before the verb runs? The `Database` must go first. If it stays, CloudNativePG creates the database again.
6. The finding says "purge the stream", and the rulings of 2026-09-29 say "deleted, not purged". This spec deletes it. Is that correct?
7. The owner message named `wamn-n5d1` as the platform UI epic. The epic that waits on this verb is `wamn-zua8`, and `wamn-n5d1` is "Provisioning verbs that call identity have no in-cluster run path". Is `wamn-zua8` the one meant?
