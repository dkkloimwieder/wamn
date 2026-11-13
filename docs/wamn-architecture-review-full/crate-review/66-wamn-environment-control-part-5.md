# 66 wamn-environment-control, part 5

Scope: platform install, `crates/control/environment/src/platform/*` at `epic/dep7` (b94833fd), about 9,600 lines in 17 files. Settles the check carried from tab 62 (host settings, `envFrom`). Nothing built or run.

## Earlier work checked first

- Tab 62: 1 A (one Helm-rendered config file per component; `ctl` reads installed settings), 2 A (Helm owns long-lived objects; Jobs and credential Secrets are explicit steps), 4 A (namespace constants). Tab 64: 1 A (control route; kubeconfig only for bootstrap and dev), 2 A (one Job runner). Tab 65: 1 A (environment and org charts), 2 B (CNPG managed roles).
- Tab 44: tooling in xtask and just. Owner: use existing tooling, do not emulate it. wasmCloud alignment: keep the runtime operator.

## Files read

- Module heads of all 17 files; `deploy.rs::deploy`, `update`, `infrastructure` (step list); `resources.rs` function list and constants; `manifests.rs` function list; `status.rs` function list.
- `deploy/platform/release` (chart with `runtime-operator` 2.11.0 dependency), `deploy/platform/*.yaml`, `deploy/infra/*.yaml` (file list).
- runtime-operator 2.11.0 chart `templates/runtime/deployment.yaml` (from the vendored `.tgz`).

## Callers read

- `ctl` `platform_verbs.rs`, `project_verbs.rs`, `bin/wamn.rs`, `delivery_verbs.rs`; dev `control.rs`, `activation.rs`, `activation/plan.rs` (grep).

## One purpose per file

- `deploy.rs`, `resources.rs`, `manifests.rs`, `artifact_plan.rs`: install and update the platform from a selected chart and repository manifests.
- `bootstrap.rs`, `job.rs`, `identity.rs`, `control.rs`, `broker.rs`, `edge.rs`, `oauth.rs`, `operations.rs`: system initialization Jobs, identity service, org control host, NATS users, edge, OAuth key, operation inputs.
- `document.rs` (`cluster.k`), `project.rs` (GCP project for OpenTofu), `provider.rs` (kubeconfig per provider), `pools.rs` (GCP node pools on and off), `status.rs` (read-only state).

## Findings

**1. Platform install uses three mechanisms.**

Fact: `deploy::infrastructure` applies repository YAML (`deploy/infra/cnpg-operator.yaml`, `cert-manager.yaml`, `nats-jetstream.yaml`, `nats-certificates.yaml`, `cnpg-cluster.yaml`, `barman-cloud-plugin.yaml`, `otel-collector.yaml`, RBAC and registry objects) object by object with `kubectl`, after `manifests::provider_edits` changes them per provider, waiting on rollouts and certificates and retrying cert-manager webhook refusals. It then installs Helm releases (`wamn` with the runtime operator, `identity`, `c-<org>`), and runs bootstrap Jobs. `deploy` orders all three with waits.

Options:

- A: two Helm releases. An infrastructure release with the upstream charts as pinned dependencies (cert-manager, CloudNativePG operator, NATS, barman plugin, OpenTelemetry collector), and the platform release (runtime operator, identity, control host, edge). Provider differences become values files. Bootstrap stays explicit Jobs (tab 62 2 A). The YAML copies, `manifests.rs` edits and per-object waits go. Inference: two releases, because cert-manager's webhook must answer before `Certificate` objects apply.
- B: keep the repository YAML; move only wamn's own objects into charts.

Recommend A: upstream charts are the existing tooling; one install record per layer (Helm history).

**2. Each host's settings are copied by name from another release's values.**

Fact: `environment/runtime.rs::host_group` reads `helm get values c-<org>` and the environment host release, keeps seven named variables plus five event variables, checks some are non-empty, and writes them into each new host group. Fact: the runtime-operator 2.11.0 chart accepts chart-wide `runtime.envFrom`, and per host group `envFrom`, `volumes` and `volumeMounts`.

Options:

- A: platform settings live in one ConfigMap (and credentials in Secrets) that every host group references through `runtime.envFrom` or a mounted file (tab 62 1 A). Environment hosts inherit them; `host_group` copying and its checks go.

Recommend A: it is the operator chart's own mechanism.

**3. Cloud provider tooling sits in the control crate.**

Fact: `project.rs` (408, GCP project for OpenTofu), `pools.rs` (357, wakes and sleeps GCP node pools), `provider.rs` (738, kubeconfig creation per provider: GCP, kind). `ctl` exposes them (`project_verbs.rs`, `platform_verbs.rs` pool resize).

Options:

- A: node pool wake and sleep and the OpenTofu project move to xtask or just (operator tooling, tab 44). Kubeconfig creation stays with platform install.
- B: keep them in the platform install crate.

Recommend A for pools and the project document; they act on cloud billing, not the platform.

**4. A host update locks through the database superuser.**

Fact: `deploy::update` for a host reads Secret `platform/wamn-pg-superuser`, port-forwards to `service/wamn-pg-rw`, and connects as `postgres` to take the lifecycle lock.

Options:

- A: the lock uses the registry login the other verbs use; with tab 64 1 A, host updates run through the control route like other changes.

Recommend A: the superuser is not needed for an advisory lock.

**5. The installed platform is read in several places.**

Fact: Helm values are read by `deploy::installed_values`, `resources::release_values`, `status::owner_values`, `environment/runtime.rs` (`resolve_platform`, `resolve_registry`, `resolve_application`) and `artifact_plan::resolve_chart`. `status.rs` is 1,661 lines.

Options:

- A: one installed-platform reader (tab 62 1 A) used by deploy, update, status and environment verbs.

Recommend A.

## Follows from earlier rulings

- Tab 64 2 A: `job.rs` moves to the one Job runner in `wamn-kubernetes`. Tab 58 6: `resources::helm`, `apply_object`, `port_forward` and `kubectl` calls move there.
- Tab 65 2 B: `bootstrap.rs` and `identity.rs` credential handoffs (`--emit-*`) give way to managed roles or in-cluster Secrets. Tab 65 3 A: `provision-system` and `provision-org` run as internal steps.
- Tab 62 9 A: `operations.rs::worker_chart` stops calling `platform_build`; it takes the stamped chart.
- Tab 62 4 A: namespace literals become constants.

## Fine as is

- `document.rs`: `cluster.k` names the cluster and inputs only (KCL kept, tab 44).
- `oauth.rs`, `edge.rs`: small; follow finding 1 for their objects.

## Tests deleted, changed or moved

- Finding 1 A: `resources.rs` tests of webhook retry and operator archive; `manifests.rs` provider edit tests become values tests.
- Finding 2 A: `runtime.rs` `host_group` tests.
- Finding 3 A: `pools.rs` and `project.rs` tests move with the code.

## Not read

- Bodies of `identity.rs`, `broker.rs`, `bootstrap.rs`, `control.rs`, `artifact_plan.rs`, `status.rs`, `provider.rs`.

## Rulings

- 2026-11-12: all findings A (two Helm releases with upstream charts; host settings by `envFrom` or mount; pools and OpenTofu project to xtask or just; lock without superuser, host updates through the control route; one installed-platform reader).
