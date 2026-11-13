# 71 wamn-ctl

Scope: `services/ctl` at `epic/dep7` (b94833fd): 8,558 source lines, 7,475 test lines in `tests/`. Three binaries: `wamn` (`bin/wamn.rs`), `wamn-ctl` (`main.rs`), `wamn-ctl-ops` (`bin/wamn-ctl-ops.rs`, `ops` feature). Nothing built or run.

## Earlier work checked first

- Tab 64 1 A: the control route is the one execution path; kubeconfig only for platform bootstrap and dev; the seven SQL `env` subcommands become internal steps. 2 A: one Job runner.
- Tab 65 1 A: org and environment Helm charts; Rust renderers go. 2 B: CNPG managed roles. 3 A: runbook verbs become internal steps.
- Tab 66: `pools.rs` and `project.rs` to xtask or just. Tab 49 1 A: the OpenTofu renderer moves beside `ProjectDocument`.
- Tab 68: `invite.rs` and `create_user.rs` go; prune functions move to their owners; the identity crate writes project user rows.
- Tab 63: a retention task removes release-keyed rows. Tab 30 3 B: workflow park, release and run list become host control routes.
- Tab 60: one migration role on the `Migrator`; `--resolve` goes; platform scopes on `sqlx::migrate!`.
- Owner 2026-11-11: hyper, not reqwest. Owner 2026-11-12: too restrictive at apply and deploy.
- Tab 70 listed `dev/watch.rs` as fine without reading its body. Finding 6 corrects that.

## Files read

- In full: `main.rs`, `bin/wamn.rs`, `bin/wamn-ctl-ops.rs`, `lib.rs`, `Cargo.toml`, `operation_verbs.rs`, `application_verbs.rs`, `delivery_verbs.rs`, `print_platform_principals.rs`, `role_verbs.rs`.
- In part: `env_verbs.rs` (commands, dispatch), `identity_verbs.rs` (functions), `integration_verbs.rs`, `component_verbs.rs` (commands, route call, tests), `workflow_verbs.rs` (head, connections, `workflow.start` call), `release_verbs.rs` (arguments, publish), `platform_verbs.rs` (commands, product dispatch), `provisioning_verbs.rs` (capture gap arguments), `ops_verbs.rs` (functions), `schema_verbs.rs` and `package_verbs.rs` (connection points).
- `services/cli_help_tests.rs`; the `Dockerfile` build and install lines.

## Callers read

- Job manifests that run `/usr/local/bin/wamn-ctl`: `platform/job.rs`, `platform/bootstrap.rs`, `platform/identity.rs`, `environment/sql.rs`. `environment/operation.rs::run` and its call to `sql::apply_deployed`.
- `test-support/infrastructure/ctl_process.rs`; every file in `services/ctl/tests` (verbs each one drives, by grep).
- Repository grep for every `wamn-ctl` and `wamn-ctl-ops` verb in charts, Rust, scripts and the `Dockerfile`.
- `crates/control/dev/src/dev/watch.rs` (its inotify use).

## One purpose per file

- `bin/wamn.rs`: the user CLI: deploy, rollback, status, schema, platform, build, integration, dev, web, inspect, diff.
- `main.rs`: `wamn-ctl`, 29 top-level verbs: Job entry points, provisioning verbs, route clients, verbs with database URLs.
- `bin/wamn-ctl-ops.rs`, `ops_verbs.rs`: create-user, two prune verbs, broker advisories, org cluster recovery.
- `operation_verbs.rs`: `ControlHost` (route call and Job status polling) and source publication.
- `application_verbs.rs`, `schema_verbs.rs`: `wamn` deploy, rollback, status and schema, each with a route path and a local path.
- `component_verbs.rs`, `role_verbs.rs`, membership in `identity_verbs.rs`: control route clients.
- `provisioning_verbs.rs`: provision-system, provision-org, provision-project-env, enable-cdc, capture gap recovery, org cluster recovery.
- `env_verbs.rs`: 13 `env` subcommands; 7 run inside SQL Jobs.
- `workflow_verbs.rs`: workflow administration over a project-admin database URL; `start` over the route.
- `package_verbs.rs`, `release_verbs.rs`: step-by-step push, publish and apply of packages.
- `platform_verbs.rs`, `project_verbs.rs`, `delivery_verbs.rs`, `print_platform_principals.rs`, `build_verbs.rs`, `integration_verbs.rs`, `web.rs`, `artifact_verbs.rs`, `dev/`: as named.

## Findings

**1. Three binaries; two CLIs overlap.**

Fact:

- `wamn` is the user CLI. `wamn-ctl` is an operator CLI and also the entry point of every in-cluster Job (`operation`, the SQL `env` subcommands, `system-schema`, `provision-system`, `provision-org`, `provision-identity-issuer`, `invite`). `wamn-ctl-ops` is a third binary behind the `ops` feature.
- Overlaps: `wamn deploy|rollback|status` and `wamn-ctl env apply|rollback|status`; `wamn schema` and `wamn-ctl env schema-apply|schema-adopt|reset`; `wamn platform schema` and `wamn-ctl system-schema`; `wamn platform manage` is `wamn-ctl platform`; `integration` is the same `IntegrationArgs` in both.
- Two publication paths: `wamn deploy <package>` and `wamn schema publish` (through `source_deploy`), and `wamn-ctl push-package` plus `publish-release`. The second pair has no caller outside `services/ctl/tests/application_publication.rs`.
- The doc comment on `main.rs::Command` warns that a verb behind `#[cfg(feature = "ops")]` compiles and passes tests but is absent from the image.

Options:

- A: two programs, one job each. `wamn` is the only user CLI. `wamn-ctl` becomes the image's internal worker: only the Job entry points left after the rulings, hidden from users. `push-package` and `publish-release` go; `source_deploy` is the one publication path. `wamn-ctl-ops` and the `ops` feature go (finding 4).
- B: one binary `wamn` with hidden internal subcommands for Jobs.

Recommend A. Inference: B puts Kubernetes, SQL and provisioning code into every user's client binary.

**2. User verbs hold database URLs instead of calling a route.**

Fact:

- `workflow author|enable|disable|list|park|release|runs` connect with `--admin-database-url` (project admin, `NoTls`). Only `workflow start` calls a route.
- `integration publish` records the version with `WAMN_SYSTEM_DATABASE_URL` (system database, control authority).
- `wamn status`, `recover-capture-gap` and `close-capture-gap` need `WAMN_SYSTEM_ADMIN_URL`.
- `apply-package` and `reconcile-package-data-access` take `WAMN_PG_ADMIN_URL`.
- Route clients already exist for roles, permissions, membership, connections, external systems and integration administration (`component_verbs::invoke`).

Options:

- A: every user verb calls a control route; the CLI holds only `WAMN_CONTROL_URL` and a token. Workflow author, enable, disable and list join park, release and runs (tab 30 3 B) as routes. Integration publish: build and push stay on the client; recording the version becomes a route. Status and the two capture gap verbs become routes.

Recommend A. It applies tab 64 1 A to the remaining verbs.

**3. The operation Job starts a second Job.**

Fact: the host's `operation_job.rs` starts a Job that runs `wamn-ctl operation`. `environment::operation::run` calls `sql::apply_deployed`, then `sql::run_deployed`, which starts a second Job of the same `ctl` image running `wamn-ctl env schema-apply` with the system administrator URL mounted. Reset does the same through `sql/reset.rs`.

Inference: the second Job keeps the administrator credential out of the operation Job.

Options:

- A: the operation Job runs the SQL step in process, as the migration role, with that role's CNPG Secret mounted (tab 60 6 A3, tab 65 2 B). The SQL Jobs, `run_deployed` and the seven SQL `env` subcommands go.
- B: keep the second Job.

Recommend A. The migration role is not a superuser, so the reason for a separate Job is gone; one Job per operation.

**4. `wamn-ctl-ops` has nothing left to own.**

Fact:

- `create-user`: tab 68 3 A deletes `create_user.rs`.
- `prune-run-history`, `prune-record-history`: no chart, CronJob or script calls them (grep); only tests do.
- `recover-org-cluster`: renders a CNPG Cluster in Rust; tab 65 1 A replaces Rust renderers with the org chart.
- `event-advisories`: reads broker advisories. No ruling covers it.

Options:

- A: delete the binary, `ops_verbs.rs`, the `ops` feature and its `Dockerfile` line. Retention runs as a CronJob in the environment chart that calls the worker. Recovery is an org chart value. `event-advisories` moves to the events owner as a control route or an xtask command.

Recommend A.

**5. Two hand-written HTTP route callers on reqwest.**

Fact: `component_verbs::call` and `invoke` (reqwest) serve every route verb and `ControlHost::run`. `workflow_verbs` builds its own reqwest request for `workflow.start`.

Options:

- A: one route client on hyper, used by every verb.

Recommend A (owner direction 2026-11-11).

**6. `wamn` builds only on Linux.**

Fact: every item in `bin/wamn.rs` has `#[cfg(target_os = "linux")]`; on other systems `main` returns "wamn dev requires Linux filesystem notifications". The cause is `crates/control/dev/src/dev/watch.rs`, a hand-written watcher on `rustix::fs::inotify`.

Options:

- A: `watch.rs` uses the `notify` crate; the Linux-only `cfg` leaves `wamn`.
- B: keep inotify; only `wamn dev` is Linux-only, the other verbs build on every system.

Recommend A: existing tooling over hand-written code, and `wamn` runs on macOS.

## Follows from earlier rulings

- Tab 65 3 A, 1 A, 2 B: `provision-system`, `provision-org`, `provision-project-env` and `enable-cdc-project-env` become internal steps; rendering and role creation move to charts and CNPG. Most of `provisioning_verbs.rs` (1,420) and `provisioning_verbs/tests.rs` (887) go.
- Tab 65 2 B: `provision-identity-issuer` goes. Tab 68 3 A: `invite` goes. Tab 68 1 A: `print-platform-principals` goes.
- Tab 64 1 A: `apply-package` and the local paths in `application_verbs.rs`, `schema_verbs.rs` and `env_verbs.rs` go; `ControlHost::from_env` stops being optional.
- Tab 62 8 A: `reconcile-package-data-access` goes. Tab 62 9 A and tab 67: `build-platform` and `delivery_verbs.rs` go.
- Tab 66: `platform wake|sleep` and `project render` move to xtask or just.
- Tab 60 6 A3, 5 B: `--resolve`, `MigrationResolution` and `schema adopt` leave `wamn`, `wamn-ctl` and `operation`; `system-schema` runs the system scope on `sqlx::migrate!`.
- Tab 64 4 A: `wamn schema reset` runs `DROP OWNED BY` plus rerun.

## Fine as is

- `wamn build`, `wamn web`, `wamn inspect`, `wamn diff`, `wamn dev`: client-side work with no database URL except the disposable build database.
- `services/cli_help_tests.rs`: checks that secret environment values stay hidden in `--help`.
- Route verbs in `component_verbs.rs` and `role_verbs.rs`: the shape every user verb should take.

## Tests deleted, changed or moved

- Finding 1 A: `services/ctl/tests/application_publication.rs` (publish-release, push-package path); parse tests in `main.rs` for verbs that leave.
- Finding 2 A: `workflow_verbs.rs` connection tests move to the route tests.
- Finding 3 A: SQL Job tests in `environment/sql.rs`.
- Finding 4 A: `prune_run_history_live.rs`, `prune_record_history_live.rs` move with the prune functions; `protected_relations_live.rs` (needs `ops`) is checked when its owner moves.
- Follows: `identity_issuer_live.rs` (904), `pat_bootstrap_live.rs` (709), `provisioning_verbs/tests.rs` (887), `reset_lock_live.rs` (tab 64), the `platform_update.rs` parts on `system-schema` and Helm upgrades (tab 66).
- CLAUDE.md test rule 2: `component_verbs::every_administration_verb_maps_to_a_project_route` pins a list of verbs; it omits `connection.test`, which `ConnectionCommand::Test` sends. Derive the list from `ConnectionCommand` and the route set.
- 40 `try_parse_from` calls in `bin/wamn.rs` and `main.rs` check clap attributes, not wamn contracts (inference). Keep only the ones that check a rule clap cannot state (for example, the refused publisher argument).

## Not read

- Bodies of `provisioning_verbs.rs`, `platform_verbs.rs::deploy`, `schema_verbs.rs`, `package_verbs.rs`, `web.rs`, `artifact_verbs.rs`, `build_verbs.rs`, `dev/` command files.
- Test bodies in `services/ctl/tests`.

## Rulings

- 2026-11-12: findings 1–5 A. 1 A: `wamn` is the one user CLI; `wamn-ctl` is the hidden internal worker; `push-package`, `publish-release` and `wamn-ctl-ops` go. 2 A: every user verb calls a control route. 3 A: the operation Job runs SQL in process as the migration role; SQL Jobs and the SQL `env` subcommands go. 4 A: `wamn-ctl-ops` and the `ops` feature go; retention is a CronJob in the environment chart. 5 A: one route client on hyper. Finding 6: no change; a Linux-only `wamn` is acceptable, `watch.rs` stays on inotify. Notes: `notify` is not in `Cargo.lock`, so it would be a new dependency; on Linux it uses the same inotify API; the selective watching that skips ignored build output would stay hand-written.
