# 02 `wamn-control-registry`

Path `crates/control/registry`. 1,826 non-test lines in six modules, about 1,120 test lines, no workspace dependencies, 16 crates depend on it. Every module was read in full. Every public symbol's callers were searched; the callers named below were opened. Nothing was built or run.

## Verdict

- The crate holds two things under one name: the identity of an environment (used everywhere) and an in-memory document model of the whole registry (used almost nowhere). (fact)
- The real store is the SQL tables in `deploy/sql/system-schema.sql`. Production code reads and writes them through the statements in `sql.rs`. The document model is a second description of the same data. (fact)
- About 500 non-test lines and most of the test lines serve the document model. (inference: counted by module and section, not line by line)

## What it holds and who calls it

Callers are files outside the crate, test code excluded, unless stated.

| Part | Lines | What it is | Callers | Finding |
| --- | --- | --- | --- | --- |
| `Triple`, `project_env_key`, `Env` (`types.rs`) | about 80 | The identity `(org, project, env)` and its key `<org>--<project>--<env>` | `Triple` 38 files, `project_env_key` 34 files | The core. |
| `identifiers.rs` | 163 | Shape rules for ids | `valid_project`, `valid_env_key`: runtime Postgres plugin, host, provision, scenario-worker. `valid_runner`, `valid_schema`: runtime `claims.rs` and host `operation.rs` only | See Ergonomics. |
| `EnvPolicy`, `OrgEnvPolicy`, `Placement`, `RecoveryDomain`, `DurabilityClass`, `cluster_of` (`types.rs`) | about 250 | Database cluster sizing and placement per org and env: instances, storage, cpu, memory, image, backup, hibernation | `control/environment`, `control/provision`, `control/release`, `wamn-ctl` | Deployment. `DurabilityClass` has one variant. |
| `template.rs` | 137 | Three presets: `trials`, `standard`, `dedicated` | `provision_org`; `wamn-ctl` `TemplateArg`; dev uses `trials` only | Deployment. |
| `sql.rs` | 313 | 20 statements and 2 lock constants | See next table | Five topics in one file. |
| `Registry` with `from_json`, `to_json`, `env_policy`, `org_env_policies` (`types.rs`) | about 60 | The whole registry as one in-memory document | One: `control/environment/src/provision_org/plan.rs::stamped_org` builds a registry of one org and no projects, only to call `validate` | Used as a validator for one org id and its policies. |
| `resolve.rs` | 103 | `Registry::resolve`, `Resolution`, `RegistryError`, lookups | None | Only this crate's tests call it. |
| `validate.rs` | 499 | `validate` over the whole document, `Issue`, `Severity`, `validate_org_id` | `stamped_org` only. `validate_org_id`: none. `Severity::Warning`: never built | The project and project-env sections cannot run from the one caller, because it passes no projects. |
| `Project`, `ProjectEnv`, `EventReader`, `SecretRef` (`types.rs`) | about 90 | Row shapes of the document model | None import them | Production code reads the rows by column index from `sql.rs` statements. |
| `Triple::host_label` | 5 | A host name from the triple | None |  |
| `environment_nats_user`, `control_nats_user` | 10 | NATS user names | `services/host`, `control/release` chart plan, `control/environment` broker | Naming for the broker. |

### `sql.rs` by caller

| Topic | Statements | Caller files |
| --- | --- | --- |
| Orgs and policies | `upsert_org_sql`, `stamp_env_policy_sql`, `select_env_policies_sql`, `select_env_policy_sql`, `select_org_placement_sql` | `control/environment` `provision_org.rs`, `env_policies.rs`, `provision_project_env/registry.rs`; `control/release` `env_policies.rs` |
| Projects and environments | `upsert_project_sql`, `select_project_env_sql`, `upsert_project_env_with_policy_sql` | `control/environment` `provision_project_env/registry.rs` |
| CDC readers and capture gaps | `upsert_event_reader_sql`, `select_event_reader_sql`, `select_event_reader_origin_sql`, `select_capture_gap_sql`, `insert_capture_gap_sql`, `lock_newest_capture_gap_sql`, `close_capture_gap_sql` | `control/environment` `enable_cdc_project_env.rs`, `capture_gap.rs`; `control/provision` `sql.rs`; `services/cdc-reader` |
| Connections | `delete_environment_connections_sql` | `control/environment` `environment/delete/rows.rs` |
| Locks | `ENVIRONMENT_LOCK_SQL`, `ENVIRONMENT_SHARED_LOCK_SQL` | `control/environment` `environment/lock.rs`; `services/cdc-reader` |
| No caller outside tests | `upsert_project_env_sql`, `select_org_project_envs_sql`, `select_retired_project_envs_sql` | tests only |
| No caller at all | `select_connection_projection_environments_sql` | none |

## Ergonomics

- Two descriptions of one store. The crate comment says the document model and the SQL schema are "tied by a drift guard". A test keeps them in step; nothing in production needs the document. (fact)
- The function names say project and mean more. `valid_project` and `project_id_reason` are the rule for org, project and env ids. The module comment says the names were kept for existing callers. (fact)
- `valid_env_key` accepts what cannot exist. It allows upper case and underscore, 1 to 64 characters. A key is built from three lowercase hyphen slugs joined by `--`, so no real key contains either. (fact)
- `valid_runner` and `valid_schema` are checks on Postgres session values. Their only callers are the runtime Postgres plugin and the host. They are not registry topics. (fact)
- `Env` is a string wrapper with seven trait implementations and no validation. Its constructor comment says validation is done elsewhere. (fact)
- `ClusterRef` and `SecretRef` wrap one or two strings. `ClusterRef` appears outside the crate only as the return value of `cluster_of`. (fact)

## Earlier rulings that change this crate

Not decided again here. Listed so the options below do not conflict with them.

- Hierarchy (Architecture tab): org, then env, then project, then application; a database per project. `Triple` is `(org, project, env)` and the key is `<org>--<project>--<env>` in 34 files.
- Credentials (Architecture tab): Kubernetes Secrets. The comment on `environment_nats_user` says the name is carried in a client certificate.
- Deployment waits for deploy.md: the provisioning flows in `control/environment`, `control/provision`, `control/release` that call the policy types, the templates and the SQL.

## Options

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 1 | Delete the document model | `resolve.rs`; `Registry` and its JSON import and export; `Project`, `ProjectEnv`, `EventReader`, `SecretRef`; the project and project-env sections of `validate`; the schema-version check; `validate_org_id`; `Severity`; `Triple::host_label`; `tests/registry.rs` | Nothing in production. `stamped_org` keeps a direct check of one org id, its pool name and its policy set. |
| 2 | Delete the four statements with no production caller | See table | Nothing. |
| 3 | Move `valid_runner` and `valid_schema` into the runtime Postgres plugin | Two functions leave this crate | Nothing. The host's one call imports from the runtime, or keeps its own check. Not checked: whether the host may depend on that module. |
| 4 | Make `valid_env_key` match what a key can be | The loose rule | Nothing, if no stored key uses upper case or underscore. Not checked against test data. |
| 5 | Put each SQL statement with the code that runs it | `sql.rs` as a shared file | One place to read all registry SQL. Three statements have two callers in different crates (`select_capture_gap_sql`, `select_event_reader_sql`, `ENVIRONMENT_SHARED_LOCK_SQL`) and need one owner. |
| 6 | Split identity from sizing | The crate becomes two: ids and keys (used by 38 files, including the runtime and host), and cluster sizing with templates (used by provisioning only) | Nothing. The runtime and host stop compiling sizing and template code they never call. |

Options 5 and 6 touch the deployment crates and may be better decided with deploy.md.

## Rulings (2026-11-10)

Owner ruling: accepted, the preference is removal. The owner also does not want SQL scattered across the code.

| # | Ruling |
| --- | --- |
| 1 | Accepted. The document model is deleted. |
| 2 | Accepted. The four statements with no production caller are deleted. |
| 3 | Accepted. `valid_runner` and `valid_schema` move to the runtime Postgres plugin. |
| 4 | Accepted. `valid_env_key` matches what a key can be. |
| 5 | Reversed by the owner's second sentence. Registry SQL is not spread over its callers; it lives only in the registry crate. Confirmed by the owner 2026-11-12 (tab 55 dependency check). |
| 6 | Accepted. Ids and keys are split from cluster sizing and templates. |

Owner ruling (2026-11-12): the crate is renamed `wamn-registry`, the same name as its system database schema `registry`. "Registry" then means one thing: the OCI modules are renamed `oci_pull.rs` and `oci_credentials.rs` in `wamn-package-artifact`, and OCI wording in `runtime/src/release_manifest_artifact.rs` says "OCI". Facts: 112 Rust files import `wamn_control_registry`.

### Registry SQL is already scattered (fact)

`sql.rs` says it is the single source of registry SQL. It is not. A search for statements that read or write a `registry.` table, test code excluded, finds 21 in `sql.rs` and about 75 more in 29 other files.

| Crate | Files with registry SQL |
| --- | --- |
| `control/environment` | 13 |
| `identity/platform` | 4 (`connection_projection.rs` 10 statements, `org.rs` 8) |
| `platform/runtime` | 3 (`connection_credentials.rs` 9) |
| `execution/host` | 3 |
| `control/provision` | 3 |
| `control/dev`, `control/release`, `services/identity` | 1 each |

The table hit most often is `registry.project_envs`: 32 statements.

Method: a pattern match on `FROM`, `INTO`, `UPDATE` or `JOIN` followed by `registry.<name>` in Rust string text. A statement split across lines in an unusual way is missed, so the real count is higher.

### SQL across the whole repository (fact, same method and limits)

SQL text appears in 130 non-test Rust files in 23 crates. The largest: `control/environment` 30 files, `platform/runtime` 15, `control/provision` 13, `identity/platform` 12, `control/package` 10, `schema/control` 9, `schema/generator` 7 (it writes SQL as its output). 18 files are named `sql.rs` or `*_sql.rs`.

No option is written for this yet. Each owning crate has to be read first. The finding is carried to every crate tab that follows.

## Not read

- The crate's tests, beyond their names.
- The 38 files that use `Triple`, beyond the search.
- `deploy/sql/system-schema.sql` for this tab.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab.

Deleted:

- `tests/registry.rs` (11 tests): the document model (ruling 1).
- `src/validate.rs` (16 tests) and `src/types.rs::registry_json_round_trips` (ruling 1). `stamped_org` keeps a direct check of one org id, its pool name and its policy set; that check needs its own test.

Changed:

- `src/identifiers.rs::env_key_validation`: the key rule matches what a key can be (ruling 4).
- Tests that set up rows with the four test-only statements (ruling 2) use product statements instead: `services/cdc-reader/tests/event_reader_live.rs`, `control/environment/tests/environment_cdc_probe_live.rs`, `capture_gap_live.rs`, `control/provision/tests/control_storage.rs`. (`tests/integration/src/cdcbench.rs` goes with tab 86.)

Moved:

- Any `valid_runner` and `valid_schema` cases in `src/identifiers.rs` tests, to the runtime Postgres plugin (ruling 3).
- Sizing tests to the sizing crate (ruling 6): `src/types.rs` `cluster_of_derives_dedicated_clusters_per_recovery_domain`, `cluster_of_collapses_a_pooled_org_onto_its_pool`, `dedicated_placement_has_no_pool`, `default_policies_are_dev_and_prod`, `recovery_domain_json_shape`, `durability_class_literals_are_exact_and_old_rows_default_standard`; `src/template.rs` (3).
- Name tests join the one name module of `wamn-registry` (tab 88 finding 9).
