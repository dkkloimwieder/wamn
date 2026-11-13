# 65 wamn-environment-control, part 4

Scope: environment provisioning at `epic/dep7` (b94833fd): `environment/ensure.rs` (1,577), `provision_project_env*` (about 3,500 with `workload`, `grants`, `registry`, `pat_secrets`, `output`, `database`), `provision_org.rs` (439), `provision_system.rs` (494), `enable_cdc_project_env.rs` (523). Settles tab 59 finding 5 (Kubernetes manifests written in Rust). Nothing built or run.

## Earlier work checked first

- Constraint row 7: one login and one Secret per role; a host restart on change; credential generations go. Owner 2026-11-10: separate logins per function are wanted.
- Tab 62 2 A: Helm owns long-lived objects; SQL Jobs and credential Secrets are explicit steps; credentials never Helm values. Tab 64 1 A: the control route is the one execution path; kubeconfig only for bootstrap and dev. Tab 64 2 A: one Job runner.
- Tab 57 R-02: service principals keep PATs. Tab 62 7 A: `catalog.tenant_environments` goes.
- Owner 2026-11-10: admin grants and user grants are per environment.

## Files read

- `environment/ensure.rs::provision_resources` (in full) and the function list.
- Heads of `provision_project_env.rs`, `provision_org.rs`, `provision_system.rs`, `enable_cdc_project_env.rs`; `provision_project_env/registry.rs::project_env_key_environment`.
- `wamn-control-provision::host_credentials` (the credential list) and `WorkloadRoleFamily`.
- `platform/job.rs`: `secret_output`, `collect`, `emitted_secret`; `platform/bootstrap.rs` Job arguments.
- `wamn-platform-identity::org::materialize_admin_grants`.
- CloudNativePG documentation, declarative role management (1.28).

## Callers read

- `provision_project_env`, `provision_org`, `provision_system` and `enable_cdc_project_env` functions from `ctl` `provisioning_verbs.rs`, dev `environment.rs` and `up.rs`, `platform/bootstrap.rs`, `environment/ensure.rs`, `sql/reset.rs`, `delete/resources.rs` (grep).

## Findings

**1. An environment's Kubernetes objects are written in Rust and applied one by one.**

Fact: `provision_resources` applies, in sequence, the CNPG `Database` (`kubectl apply`, then waits), the database and PAT Secrets, the event broker objects including a `Certificate` (`broker::environment_objects`), the CDC reader `Deployment` (`enable_cdc_project_env::reader_deployment`), and the materializer workload; it patches the org control host's administration Secret. `wamn-control-provision` renders these (`database.rs`, `secret.rs`, `org.rs`, `backup.rs`, `recovery.rs`, `materializer_workload.rs`; tab 59 finding 5). `provision_org` renders the org's CNPG `Cluster`, `ObjectStore` and `ScheduledBackup` objects.

Options:

- A: one environment Helm chart (with `Database` kept on uninstall, `Certificate`, CDC reader, materializer, host group) and one org chart (`Cluster`, backup objects), rendered from registry configuration. `ensure` becomes a Helm upgrade plus the SQL and credential steps. The renderers in `wamn-control-provision` and the per-object apply and wait code go.

**2. Credentials made in a Job are copied out of the cluster and applied back.**

Fact: SQL and bootstrap Jobs run `ctl` with `--emit-secret`, `--emit-apply-secret` or `--emit-control-secret`. The Job writes a Secret document into an in-memory volume (`secret_output`, 1 MiB); the caller hands it off and parses it (`emitted_secret`), then applies it (`apply_and_shred`, `patch_and_shred`) from its own machine. A failed handoff keeps the Job so the next run can recover it.

Options:

- A: the Job writes its Secrets itself, in-cluster, through its service account. The emit flags, the handoff, `emitted_secret` and the shred steps go.
- B: CloudNativePG managed roles. Fact (CNPG 1.28 docs): `Cluster.spec.managed.roles` declares roles with `login`, `passwordSecret`, `inRoles`, `inherit`, `ensure: present | absent`; manual changes are reverted at reconciliation. A Job or chart creates the Secrets; CNPG creates the logins; wamn writes no password SQL. Inference: on a shared cluster every environment's roles sit in one `Cluster` spec; the docs do not say whether a changed password Secret is reconciled (their examples label Secrets `cnpg.io/reload`).

**3. Runbook verbs remain as public commands.**

Fact: `provision-system`, `provision-org`, `provision-project-env` and `enable-cdc-project-env` describe a runbook ("renders + records; the runbook/Job applies the emitted artifacts"). Today they run inside platform Jobs (`platform/bootstrap.rs`, `sql.rs`) and from dev. `env apply` already does the same work through `ensure`.

Options:

- A: they become internal steps of the one executor (tab 64 1 A). The public verbs, their emit modes and the runbook steps in their headers go.

**4. Administrator rows are copied down the hierarchy when a project or environment is provisioned.**

Fact: `materialize_admin_grants` (identity crate) inserts a `project-admin` row for each `org-admin`, then an environment membership for each `project-admin`. Rows that exist stay. Not checked: whether a later `org-admin` grant also writes rows into existing environments.

Options:

- A: derive it. An `org-admin` is `project-admin` of every project and a member of every environment by rule, checked when authorizing; no copied rows.
- B: keep the copies (owner 2026-11-10: grants are per environment).

## Follows from earlier rulings

- Row 7: `provision_project_env/workload.rs` and `plan.rs`, `identity_issuer.rs` and `plan.rs` reduce to one login and Secret per role; "generation a" in `ensure` goes. The nine environment credentials in `HOST_CREDENTIALS` stay as separate logins.
- Tab 62 1 A and 4 A: `ensure`'s environment and namespace reads come from the installed platform and constants.
- Tab 62 7 A: `project_env_key_environment` and `claim_environment_instance` go. Tab 02: `provision_project_env/registry.rs` SQL moves to `wamn-registry`.
- Tab 62 5 A: `ensure_project_database` stops calling `reconcile_run_plane`.
- Tab 60 6 A3: `ensure_package_roles` goes.

## Fine as is

- Separate logins per function (`HOST_CREDENTIALS`), as the owner wants.
- `pat_secrets.rs`: PATs for the management-author and operator service principals (R-02 keeps service PATs); its Secret delivery follows finding 2.

## Tests deleted, changed or moved

- Finding 1 A: renderer tests in `wamn-control-provision` (`database`, `secret`, `org`, `backup`, `recovery`, `materializer_workload`); `ensure` apply tests become chart tests.
- Finding 2 A or B: `platform/job.rs` handoff tests; `emitted_secret` tests; `--emit-*` tests in `ctl`.
- Finding 3 A: `ctl` provisioning verb tests; `provision_project_env/tests.rs` parts that test emitted artifacts.

## Not read

- Bodies of `provision_org.rs`, `provision_system.rs`, `enable_cdc_project_env.rs` beyond heads; `provision_project_env/grants*` (990 lines of plan); `pat_secrets.rs` body.

## Rulings

- 2026-11-12: finding 1 A (environment and org Helm charts; the Rust renderers go; settles tab 59 finding 5). Finding 2 B (CloudNativePG managed roles; fall back to A if a changed password Secret is not reconciled). Finding 3 A (runbook verbs become internal steps).
- 2026-11-12: finding 4 A. Owner: org admins are admins in every environment (derived by rule when authorizing; no copied rows). A user made an environment admin is not thereby an org admin; environment admin grants stay explicit per environment. Owner confirmed the rules in the ruled hierarchy (org, environment, project, application): org admin is admin in every environment; environment admin is admin of every project and application in that environment and is not an org admin; project admin is admin of every application in that project only. Derived when authorizing; \`materialize\_admin\_grants\` goes. Carried: the code orders org, project, environment (\`Triple\`, \`registry.project\_envs\`, the \`\<org>--\<project>--\<env>\` key in 34 files); reordering belongs to the registry crate.
