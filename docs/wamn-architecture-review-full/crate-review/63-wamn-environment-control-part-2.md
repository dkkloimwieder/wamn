# 63 wamn-environment-control, part 2

Scope: the `env apply` steps in `crates/control/environment/src/environment/` at `epic/dep7` (b94833fd): `analyse`, `stage`, `expand`, `write`, `readiness`, `drain`, `bindings`, `connections`, `clients`, `lock`, `status`, `show`, `rollback`, `delete`, `host_report` (about 5,500 lines with their `plan.rs` files). Nothing built or run.

## Earlier work checked first

- Tab 62 rulings: Helm history is the one record of the applied release (2 A); Helm owns long-lived objects, SQL Jobs and credential Secrets are explicit steps (2 A); the registry holds environment configuration (3 B); platform settings from the installed platform (1 A).
- Tab 60: SQLx `Migrator` for platform scopes (5 B); one migration role (6 A3); overlay check moves to build (3 B). Tab 62 finding 5 A: the run-plane converge engine goes.
- U2: SQLx pools (tab 57 finding 1). Tab 58 finding 6: `kubectl` and `helm` in `wamn-kubernetes`.
- Owner 2026-11-12: far too restrictive at apply and deploy.

## Files read

- `analyse.rs`: `analyse`, `read_release_packages`, `read_applied_releases`, `read_live_set`, `read_pods`, `replica_refusals`; `analyse/plan.rs` in full (non-test).
- `stage.rs`: function list, `write_runtime_grants`, `retire` (in full), the sequence and order tests; `expand.rs` (in full).
- Heads and function lists of `bindings.rs`, `drain.rs`, `readiness.rs` (in full), `write.rs`, `lock.rs`, `connections.rs`, `clients.rs`, `status.rs`, `show.rs`, `rollback.rs`, `delete.rs` and `delete/*`, `host_report.rs`.
- `deploy/sql/catalog-schema.sql`: `catalog.connection_binding`, `release_connection`, `releases`.

## Callers read

- Every caller of the `stage` functions, `application_refusals`, `select_application` and `host_report` (grep).
- Runtime readers of `catalog.releases`, `connection_binding`, `release_connection` (`wiring_resolution.rs`, `workflow_resolution.rs`, `claims.rs`, `local_application.rs`) and of `release_manifest_source` (`services/host/src/host.rs`).

## One purpose per file

- `analyse`: read every authority once and return refusals, plan and notices.
- `stage`: initialize baselines, cache releases, principals and grants of integration applications, number sequence defaults, retire old release facts.
- `expand`: write the selected release's control facts before Helm starts it.
- `write`, `readiness`, `drain`: Helm upgrade, wait for role workloads, wait for old pods to leave and report stranded runs.
- `bindings`, `connections`, `clients`: release-scoped connection bindings, integration documents through control routes, web client checks.
- `lock`: advisory locks per environment and per application. `status`, `show`, `rollback`, `delete`: the other verbs. `host_report`: pod status and logs attached to failed rollouts.

## Findings

**1. Which packages make up an application is declared twice.**

Fact: `environment.k` lists `packages` per application, and the release manifest lists its packages. `deploy` finds the application whose package set equals the release's (`mod.rs::deploy_as`). `application_refusals` checks the two agree in six product places (`analyse/plan.rs`, `expand.rs`, `sql.rs`, `sql/schema.rs` twice, `sql/reset.rs`). `retire` filters cached releases by the document's set; `sequence_defaults` groups by it.

Options:

- A: an application is the release it serves. The document names each application with its hostname, client and connections; its packages come from its release. `deploy <application> <digest>` names the application. `application_refusals` and the set matching go.
- B: keep both lists and the agreement check.

**2. The same checks and connections repeat across steps.**

Fact: `select_application` is called 11 times in product code; `application_refusals` 6 times; `expand::prepare` checks again that the digest matches the selection. The `environment` module opens the system database 23 times (`connect(&platform.system_database_url)`); `analyse` alone opens it twice.

Options:

- A: `analyse` returns one checked plan value; later steps take it and do not check again. One SQLx pool per verb (U2).

**3. Release-keyed facts are added before each rollout and removed after it.**

Fact: before Helm, `expand` writes the release manifest into `catalog.releases`, the release's `release_connection` and `connection_binding` rows, and runtime grants for integration application roles. After drain, `stage::retire`, `bindings::contract` and `stage::retire_runtime_grants` remove what only older releases used, refusing if releases or hosts changed meanwhile (about 300 lines). The runtime reads these rows by manifest digest (`wiring_resolution.rs`, `workflow_resolution.rs`, `claims.rs`). The host also reads the same manifest from OCI (`release_manifest_source`).

Options:

- A: keep the release-keyed rows (they are content-addressed and immutable) but stop removing them during apply. A retention task removes rows of releases that no host serves. `retire`, `contract` and `retire_runtime_grants` leave the apply path.
- B: keep the add-then-remove cycle.

**4. `env delete` removes resources one at a time.**

Fact: `delete.rs` and `delete/*` (about 970 lines) check ownership of and delete, in order, the CDC reader, materializer, PATs, Secrets, control administration key, streams, event certificate, Publication CR, replication slot, `Database` CR, logins, roles, the credential store Secret, the ConfigMap and the registry row.

Options:

- A: under tab 62 ruling 2 A, the long-lived objects belong to Helm releases, so `env delete` uninstalls them and then runs the explicit SQL, NATS and credential steps. The per-object ownership checks in `delete/resources.rs` go.

**5. Failed rollouts carry a pod report gathered while Helm waits.**

Fact: `host_report` (570 lines) polls pod status and logs every 10 seconds during `helm upgrade`, because Helm runs with `--rollback-on-failure` and the failed pods are gone when it returns. `readiness` collects it again on failure. `platform/resources.rs` uses it too.

Options:

- A: Helm keeps a failed release (no automatic rollback); the error names the release and `wamn status` or `kubectl` shows the pods. `rollback` stays the explicit undo. `host_report` goes.
- B: keep the report.

## Follows from earlier rulings

- Tab 62 2 A: `persist_selection`, `select_release`, `selection_warning` and the `retire` use of `selection.previous` change to Helm history; `expand`'s "activation digest differs from the application selection" check goes.
- Tab 62 3 B: `show` and `status` read the registry, not the ConfigMap.
- Tab 62 1 A: `replica_refusals` reads replicas from the installed host group, not `WAMN_RELEASE_HOST_GROUP`.
- Tab 60 5 B: the platform schema baseline notices in `analyse` (`observe_system`, the project `schema_report`) go with the hand-written history; physical fingerprints are part 3.
- Tab 58 6: `read_pods`, `helm`, `helm_in` and the `kubectl` calls move to `wamn-kubernetes`. Tab 02: lock SQL already comes from the registry crate; it moves with it to `wamn-registry`.

## Fine as is

- `readiness`: waits on each role `WorkloadDeployment` with the rest of one budget. Inference: Helm's own wait does not cover these custom resources.
- `drain`: observes Kubernetes' grace period and reports stranded runs without failing them.
- `lock`: one advisory key per environment and per application.
- `rollback.rs` (21 lines).

## Tests deleted, changed or moved

- Finding 1 A: `application_refusals` tests in `analyse.rs`; membership filters in `stage` tests.
- Finding 3 A: retirement tests in `stage.rs` and `bindings.rs`.
- Finding 5 A: `host_report` tests and `host_report/plan.rs`.

## Not read

- `bindings.rs` bodies beyond the function list; `status.rs`, `delete.rs` bodies; `clients.rs`; `write/plan.rs`.

## Rulings

- 2026-11-12: all findings A. Noted with 1 A: one package may then appear in more than one application's release. Noted with 3 A: an integration application role keeps an older release's permissions until retention removes them.
