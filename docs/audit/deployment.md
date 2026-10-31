# Audit lane D: Deployment

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-xm69b.1, wamn-xm69b.2, wamn-xm69b.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane D1, bead wamn-xm69b.1 -->

## Part 1: variants

This audit used file reads, reference searches, and YAML inspection. No files changed. No builds, tests, or cluster commands ran. Tests named below are required proof for proposed changes.

The shared baseline records 83 YAML files and 46,235 lines in measurements.md section 1. The copy detector excludes YAML, so its duplication percentage does not measure deployment duplication.

## Finding D1-1: KCL and JSON declare different application sets

Severity: high  
Effort: M  
Area: `deploy/gcp/environments/`, `crates/control/lib/src/platform/`

Evidence: `deploy/gcp/environments/dkk--wms--dev.k:16`, `deploy/gcp/environments/dkk--wms--dev.k:17`, `deploy/gcp/environments/dkk--wms--dev.json:11`, `crates/control/lib/src/platform/build.rs:125`, `crates/control/lib/src/platform/packages.rs:26`, `crates/control/lib/src/platform/broker.rs:131`.

Problem: WMS KCL declares `wamn_wms`, `rest_connector`, `rest_consumer_fixture`, and `wamn_integration`. Its JSON sibling lists only `apps/wamn_wms`. Platform build, package publication, and broker preparation read that sibling instead of deriving the package set from KCL. A clean installation therefore prepares a different package set from the declared environment.

Proposal: Compile each environment once. Derive build, publication, broker, qualification, and deployment inputs from its application declarations and package dependencies.

Deletes: Both environment JSON files, their required-sibling convention, and their package lists. Remove the old delivery configuration reader after its remaining consumers migrate.

Risk: Legacy delivery and `tools/test-changes` consume the JSON format. Replace the test at `crates/control/lib/src/platform/build.rs:433`, which currently requires that format. Add a regression that derives all four WMS packages without JSON siblings, then exercise `platform_apply_kind`.

## Finding D1-2: Cloud rendering ignores declared destination values

Severity: high  
Effort: M  
Area: `deploy/gcp/otel-collector-values.yaml`, `deploy/gcp/letsencrypt.yaml`, `crates/control/lib/src/platform/manifests.rs`

Evidence: `deploy/gcp/cluster.k:11`, `deploy/gcp/otel-collector-values.yaml:8`, `deploy/gcp/letsencrypt.yaml:17`, `deploy/gcp/letsencrypt.yaml:23`, `crates/control/lib/src/platform/manifests.rs:128`, `crates/control/lib/src/platform/manifests.rs:138`, `crates/control/lib/src/platform/manifests.rs:399`.

Problem: The cluster document declares the telemetry bucket, project, cluster, zone, and owner. The collector ConfigMap still emits committed `wamn-dev` values. Certificate rendering changes the wildcard name but leaves the ACME email, DNS project, and zone unchanged. The render test explicitly expects the old telemetry bucket.

Proposal: Render cloud destinations from their owning documents. Let the project document own the DNS zone and guard destination. Let the cluster document supply runtime destinations.

Deletes: `deploy/gcp/otel-collector-values.yaml` as an independent authority. Remove installation-specific defaults from `letsencrypt.yaml` and the test assertion that preserves `wamn-dev-otel`.

Risk: A second project can receive telemetry failures or DNS challenge failures. Extend `both_providers_render_every_install_manifest` with different project, bucket, email, and domain values. Require every rendered destination to follow those inputs.

## Finding D1-3: The RC path installs an incomplete collector variant

Severity: high  
Effort: S  
Area: `deploy/infra/otel-collector.yaml`, `tests/integration/src/rc/jobs.rs`

Evidence: `deploy/infra/otel-collector.yaml:38`, `deploy/infra/otel-collector.yaml:60`, `deploy/infra/otel-collector.yaml:78`, `tests/integration/src/rc/jobs.rs:48`, `tests/integration/src/rc/jobs.rs:56`, `test-support/infrastructure/telemetry.rs:28`, `test-support/infrastructure/telemetry.rs:80`.

Problem: The infrastructure collector is a partial template. Both containers lack images, and its configuration refers to an `otlp` receiver without declaring one. The shared telemetry renderer supplies the missing configuration and images. The RC installer reads the partial template directly and only replaces the separate `serve-echo` image.

Proposal: Give every collector consumer the same complete renderer. Keep test exporter differences as inputs to that renderer.

Deletes: The RC-specific collector loading path. After consolidation, remove the second collector Deployment and Service template.

Risk: Static inspection shows an invalid Pod specification before the RC trace test starts. Add a pure rendering regression that requires images and declared receivers. Then run the RC path at `tests/integration/src/rc.rs:222`.

## Finding D1-4: Historical workloads retain a second deployment model

Severity: medium  
Effort: M  
Area: `deploy/gcp/*flow-http.yaml`, `deploy/gcp/*materializer.yaml`

Evidence: `deploy/gcp/flow-http.yaml:45`, `deploy/gcp/flow-http.yaml:48`, `deploy/gcp/wms-flow-http.yaml:48`, `deploy/gcp/materializer.yaml:45`, `deploy/platform/release/templates/workloads.yaml:17`, `deploy/platform/release/templates/workloads.yaml:54`, `test-support/infrastructure/examples/workload_files.rs:129`.

Problem: Four committed workloads preserve the old `default` and `wms` host groups and short environment keys. The current chart derives release names and selects both host group and release. The historical HTTP declarations also lack interfaces present in the current chart. Their generator writes deployment snapshots, while current deployment reads the release chart.

Proposal: Make the release chart the sole Kubernetes workload definition. Generate inspection output on demand from the same inputs used for deployment.

Deletes: `deploy/gcp/flow-http.yaml`, `deploy/gcp/wms-flow-http.yaml`, `deploy/gcp/materializer.yaml`, `deploy/gcp/wms-materializer.yaml`, and their snapshot generator.

Risk: Historical manual commands name these files. Update those references when deleting them. Use prekind rendering and `environment_apply_kind` to prove release selectors, interfaces, entry images, and Services.

## Finding D1-5: Local development embeds Receiving’s cloud host policy

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/dev/activation.rs`, host values files

Evidence: `crates/control/lib/src/dev/activation.rs:898`, `crates/control/lib/src/dev/activation.rs:903`, `crates/control/lib/src/dev/activation.rs:1056`, `crates/control/lib/src/dev/activation.rs:1072`, `deploy/gcp/host-group-receiving.yaml:97`, `deploy/gcp/host-group-wms.yaml:98`, `crates/control/lib/src/dev/activation.rs:2105`.

Problem: Local activation combines release-chart defaults with an embedded Receiving cloud manifest. It takes database pool sizes, request limits, and Wasmtime limits from that manifest for every application. Receiving allows four concurrent starts, while WMS declares one. The parity test compares against Receiving again, so it does not establish WMS parity.

Proposal: Define one application-independent host policy. Render local process arguments and cluster host values from that policy plus explicit environment inputs.

Deletes: The Receiving manifest embedding, repeated policy values across host overlays, and tests that compare two consumers of the same tenant-specific input.

Risk: Pool and concurrency behavior can change. Extend `local_host_matches_the_deployed_host_contract` to cover Receiving and WMS with explicit expected differences.

## Finding D1-6: Production CDC uses the gate image

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/platform/cdc.rs`, `Dockerfile`, CDC manifests

Evidence: `crates/control/lib/src/platform/cdc.rs:21`, `crates/control/lib/src/platform/cdc.rs:93`, `Dockerfile:189`, `Dockerfile:203`, `Dockerfile:207`, `Dockerfile:212`, `deploy/platform/release/values.yaml:79`, `crates/control/lib/src/platform_build.rs:31`.

Problem: Platform installation runs the CDC reader from the qualification gates image. That image also contains control executables and test fixtures. A dedicated CDC image stage already exists, but the platform build does not publish it. The two historical CDC manifests carry another independently pinned reader image.

Proposal: Publish and select the dedicated CDC artifact through the same image authority as other deployed services. Keep one coordinate-derived CDC Deployment renderer.

Deletes: Both historical CDC YAML files, the redundant `event-reader-rbac.yaml`, and the production dependency on the gates image. Retain the reader executable in gates where tests require it.

Risk: Reader replacement must preserve one reader per replication slot and the environment’s credential names. Extend `readers_use_environment_secret_names_and_detect_live_drift`, then exercise CDC in `platform_apply_kind`.

## Finding D1-7: Operator and host releases select their dependency separately

Severity: medium  
Effort: S  
Area: `deploy/platform/release/`, `crates/control/lib/src/platform/install.rs`

Evidence: `deploy/platform/release/Chart.yaml:12`, `deploy/platform/release/Chart.lock:4`, `crates/control/lib/src/platform/install.rs:24`, `crates/control/lib/src/platform/install.rs:1008`, `tests/conformance/tests/chart_seam_governance.rs:14`.

Problem: The host subchart, lockfile, installer constant, and governance test repeat `2.11.0`. The host uses the vendored chart archive, while cluster installation fetches the operator chart independently. Updating one owner can leave the operator and host chart on different revisions. The release chart changed in 111 baseline commits, so this is a recurring maintenance surface, per measurements.md section 9.

Proposal: Select one pinned operator chart artifact. Use it for both cluster installation and the host-only subchart.

Deletes: The independent installer version constant, independent artifact selection, and the test’s separate expected-version literal.

Risk: Operator CRDs and host rendering must remain compatible. Use `chart_seam_governance` and a kind installation with the selected artifact.

## Finding D1-8: Manual operator Jobs freeze an older runner

Severity: medium  
Effort: M  
Area: `deploy/gcp/operator/`, legacy delivery rendering

Evidence: `deploy/gcp/operator/invite.yaml:33`, `deploy/gcp/operator/mint-pat.yaml:36`, `deploy/platform/release/Chart.yaml:5`, `docs/operations/gcp.md:973`, `crates/control/lib/src/upgrade_environment.rs:1412`, `crates/control/lib/src/platform/install.rs:448`.

Problem: Both operator Jobs pin runner `0.1.36`, while the current platform chart declares `0.1.137`. Legacy delivery rewrites the minting template’s image, but the documented manual substitution does not. These paths therefore select different executables for the same operation. The current platform already reads its runner from the stamped chart.

Proposal: Render operator Jobs through the existing Job owner with the stamped runner image and explicit operation arguments.

Deletes: Both operator YAML templates and the separate text-substitution renderer.

Risk: Invitation and token minting require different arguments and output handling. Preserve their behavior through Job rendering tests and an identity integration test using the stamped runner.

## Finding D1-9: The named orphan candidates have different consumer states

Severity: low  
Effort: S  
Area: Named manifests and stale executor references

Evidence: `deploy/gcp/cdc-reader-wms.yaml:1`, `docs/history/gcp-install-history.md:1564`, `docs/operations/gcp.md:718`, `deploy/platform/identity-db.example.yaml:18`, `deploy/infra/prometheus-local.yaml:14`, `crates/platform/runtime/tests/guest_environment_isolation.rs:237`.

Problem: `cdc-reader-wms.yaml` has historical documentation consumers, but current platform installation renders CDC in Rust. `evt-nats-check.yaml` remains an active manual runbook input, so it is not an orphan. Searches found no external consumer for `identity-db.example.yaml` or `prometheus-local.yaml`. The missing `executor.yaml` appears in test prose and failure text, not in a file read.

Proposal: Delete unused examples and historical CDC carriers. Retain the NATS diagnostic until its runbook operation moves to the broker owner. Update the security test’s explanation to cite the current host.

Deletes: `identity-db.example.yaml`, `prometheus-local.yaml`, the CDC files covered by D1-6, and stale executor references. Do not recreate `executor.yaml`.

Risk: Preserve `a_guest_never_sees_the_host_process_environment`; its behavioral assertion remains valid. Keep the NATS diagnostic until an equivalent operation proves stream access.

## Finding D1-10: Prototype deployment families survive through examples and test expectations

Severity: low  
Effort: M  
Area: `deploy/platform/`, `deploy/infra/`, `deploy/mvp/`

Evidence: `deploy/platform/postgres.yaml:33`, `deploy/platform/wamn-sysdb.yaml:39`, `deploy/platform/registry.yaml:215`, `deploy/platform/scenario-worker.yaml:149`, `crates/control/provision/src/secret.rs:432`, `tests/system/tests/deployment_declarations.rs:69`, `deploy/mvp/tests/bootstrap.sh:5`.

Problem: The tree retains a legacy Postgres Deployment, a separate system database cluster, a local registry, and application-specific service examples. Some survive through generic declaration tests or credential contract tests rather than current installation. Those tests preserve obsolete deployment shapes alongside the current platform. Retention and bootstrap examples also encode a separate operational lifecycle.

Proposal: Keep current installation resources and canonical SQL. Move surviving behavioral assertions to current renderers, then remove prototype carriers and their shell wrapper.

Deletes: The unused Postgres, system-cluster, registry, credential-example, retention-example, and bootstrap families identified below. Remove associated test prerequisites after equivalent behavior is covered.

Risk: Repository searches cannot establish whether operators still apply these files externally. Preserve credential boundary tests, `deployment_declarations`, and current cluster journeys before deletion.

### Variant inventory

Path abbreviations below expand as follows: `G` = `deploy/gcp`, `I` = `deploy/infra`, `P` = `deploy/platform`, `R` = `deploy/platform/release`, `C` = `crates/control/lib/src/platform`, and `D` = `crates/control/lib/src/dev`.

“Delete after migration” means the consumer remains useful, but its separate deployment definition disappears.

| Variant | Consumer and evidence | Target disposition |
| --- | --- | --- |
| `G/cluster.k`, `G/project.k` | Cluster compilation and infrastructure provisioning: `C/document.rs:71`, `C/project.rs:94` | Keep distinct runtime and infrastructure responsibilities. Derive their shared cluster identity once. |
| Two `G/environments/*.k` files | Environment discovery: `C/broker.rs:81` | Keep as environment declarations. |
| Two `G/environments/*.json` files | Build, broker, publication, legacy delivery, and `tools/test-changes:318` | Delete after D1-1. |
| Five `G/connections/*.json` files | Computed filenames in `crates/control/lib/tests/support/prekind_render.rs:21`, `:24`, `:40` | Keep owned request examples. They are not orphans. |
| `G/guard/{main.py,guard.py,test_guard.py,requirements.txt,config.json}` | Guard entry point and tests; configuration loads at `G/guard/guard.py:12` | Keep guard behavior. Generate its deployment configuration from project identity. |
| `I/cert-manager.yaml`, `I/cnpg-operator.yaml` | Installer: `C/install.rs:481`, `:641`; cluster fixtures | Keep pinned upstream artifacts. |
| `I/barman-cloud-plugin.yaml`, `I/minio.yaml` | Backup documentation references in `crates/control/provision/src/backup.rs:9`, `:16`; no executable manifest consumer found | Delete unused installation prototypes after confirming external use. Preserve backup behavior separately. |
| `G/cnpg-cluster.yaml` | Current installer and local benchmark: `C/install.rs:652`, `tools/bench/local.sh:152` | Keep one cluster definition with provider inputs. |
| `I/cnpg-cluster.yaml` | Test cluster construction: `test-support/infrastructure/delivery_cluster.rs:438` | Delete after fixtures use the current cluster definition. |
| `P/postgres.yaml`, `P/wamn-sysdb.yaml` | Generic declaration/governance expectations; no current installation consumer found | Delete prototype carriers after test migration. |
| `G/nats-jetstream.yaml` | Current installer and benchmark: `C/install.rs:687`, `tools/bench/local.sh:267` | Keep current broker definition. |
| `I/nats-jetstream.yaml` | Documentation references; no executable manifest consumer found | Delete the separate three-node prototype. |
| `I/values-wamn.yaml` plus `G/values-operator.yaml` | Ordered merge: `C/manifests.rs:84` | Keep operator configuration with one namespace input. Delete the namespace-only overlay. |
| `P/values-host-default.yaml`, Receiving PAT overlay, WMS PAT overlay | Application cluster tests and RC rendering: `apps/wamn_receiving/tests/route_authentication_live/cluster.rs:578`, `apps/wamn_wms/tests/cluster/application.rs:191`, `tests/integration/src/rc/deployment.rs:94` | Delete after tests use release-chart inputs. |
| `G/values-host-base.yaml`, `G/values-host.yaml` | Generated by `test-support/infrastructure/examples/host_values_files.rs:133`; combined values read by `crates/control/lib/src/upgrade_environment.rs:2072` | Delete generated snapshots after legacy delivery migration. |
| `G/host-group-receiving.yaml`, `G/host-group-wms.yaml` | Current platform uses computed names: `C/environments.rs:261`; dev embeds Receiving | Replace with shared host policy and environment inputs. |
| Four historical GCP HTTP/materializer workloads | Historical manual installation; snapshot writer at `test-support/infrastructure/examples/workload_files.rs:129` | Delete under D1-4. |
| `P/http-route-workload.example.yaml`, `P/materializer.example.yaml` | Test renderers and local activation: `D/activation.rs:51`, `:1929` | Delete after workload contracts derive from one owner. |
| `G/control-service.yaml` | Current cluster and local control rendering: `C/control.rs:228`, `D/control.rs:68` | Retain its contract in a provider-neutral workload owner. Delete the cloud-specific carrier. |
| Two GCP CDC manifests and `P/event-reader-rbac.yaml` | Historical installation; current equivalent is `C/cdc.rs:66`, `:78` | Delete under D1-6. |
| `P/identity/` chart | Current installation: `C/identity.rs:267` | Keep. |
| `P/values-identity-default.yaml` | Declaration test only: `tests/system/tests/deployment_declarations.rs:423` | Delete redundant overlay after test migration. |
| `G/values-identity.yaml` | Manual installation values; current installer independently renders values at `C/identity.rs:236` | Delete the frozen snapshot. |
| `P/edge/` chart | Current nginx proxy: `C/edge.rs:58` | Keep. It serves a different role from the Rust web edge and physical edge binary. |
| GCP certificate, auth-client, registry-helper, and placement manifests | Current installer: `C/manifests.rs:62`, `:72`, `C/install.rs:413`, `:499` | Keep required resources. Derive installation-specific values. |
| Host account, control RBAC, operator event RBAC, host certificate examples | Current installation and fixtures: `C/manifests.rs:64`, `C/install.rs:547`, `:598` | Keep required resources with one namespace policy. |
| `P/otel-collector.yaml` | Current installation, dev collector, tests: `C/install.rs:617`, `tools/dev-collector:13` | Keep complete collector definition. |
| `G/otel-collector-values.yaml`, `I/otel-collector.yaml` | Cloud destinations and test collector variant | Delete independent carriers after D1-2 and D1-3. |
| `I/prometheus.yaml`, `I/prometheus-local.yaml` | Only self-contained manual instructions found | Delete unused deployment examples. The local file names a private lane container. |
| `P/registry.yaml`, `P/registry-credentials.example.yaml` | Comments and generic declaration expectations; no current installer consumer found | Delete prototype registry deployment and placeholder credentials. |
| `P/scenario-worker.yaml` | Credential contract tests: `crates/control/provision/src/secret.rs:432`, `:496` | Delete static application deployment after preserving credential assertions. Keep the service. |
| Host and identity database examples | Placeholder Secrets at `P/host-db.example.yaml:35`, `P/identity-db.example.yaml:18` | Delete. Provisioning owns real credential emission. |
| Four audit/run retention examples | Module documentation and paired example references | Delete static examples after choosing the current scheduling owner. Keep pruning operations. |
| Two `G/operator/*.yaml` Jobs | Manual runbook and legacy delivery | Delete under D1-8. |
| `deploy/mvp/bootstrap.sh` and its shell test | Shell test and Rust contract test at `crates/control/lib/src/provision_project_env/pat_secrets/tests.rs:26` | Delete after credential lifecycle coverage moves to its Rust owner. |
| Three gate workloads, event-stream JSON, gate Postgres Dockerfile | Computed Job paths at `tests/integration/src/rc/jobs.rs:98`; stream configuration at `tests/integration/src/rc/deployment.rs:17`; `tools/rc-gate-run:17` | Keep. Exact-path searches alone miss the Job consumers. |
| `deploy/sql/` declarations | Provisioning, catalog, run-state, and test composition: `crates/control/provision/src/lib.rs:148`, `:155`, `:174`, `crates/catalog/model/src/lib.rs:102` | Keep canonical schema inputs. SQL duplication needs its own ownership review. |
| `deploy/sql/corrections/wamn-a04x-wms-0007.sql` | Explicit incident correction with database guard at `:18` | Keep as controlled historical recovery evidence until its owner retires it. |

### Docker artifact inventory

| Target | Consumer | Can it disappear? |
| --- | --- | --- |
| `host` | Current release host and base of `gates`; `Dockerfile:158` | No. |
| `ctl` | Parent of `runner`; carries control and operations executables; `Dockerfile:168`, `:280` | Keep stage. Independent publication is unnecessary for the current platform build. |
| `runner` | Platform Jobs and qualification cases; `Dockerfile:280`, `C/install.rs:448` | No. |
| `gates` | Qualification and gate execution; also current CDC deployment; `Dockerfile:203` | Keep for tests. Remove production CDC use. |
| `identity` | Identity chart; `Dockerfile:196`, `C/identity.rs:236` | No. |
| `edge-web` | Cluster Rust web service; `Dockerfile:109`, `C/edge.rs:194` | No. |
| `roles` | Exported HTTP and materializer components; `Dockerfile:234` | No. |
| `edge` | Physical ARM64 device binary; `Dockerfile:301` | Keep while physical edge devices remain supported. |
| `cdc-reader` | Dedicated service artifact, currently bypassed by platform installation; `Dockerfile:189` | Use it for D1-6. |
| `scenario-worker` | Standalone authoring service image; `Dockerfile:182`; absent from platform image list | Delete this final image stage if deployment remains local-only. Preserve its executable in `ctl` and `runner`. |
| `cranelift-dev` | Optional development shell; `Dockerfile:304` | No deployment consumer. Delete if the shell is no longer supported. |

The remaining Docker stages supply source, builds, composition, case executables, or copied files to these artifacts. They are build dependencies, not separate deployment variants.

### Repeated value inventory

This inventory covers deployment parameters and rendering literals. It excludes comments, test assertions, SQL role identifiers, and Kubernetes API-version boilerplate. Comma-separated numbers identify individual lines. Shared prefixes use the path abbreviations above.

Repetition inside a generated upstream bundle is intentional. Competing authored defaults and committed rendered snapshots are the deletion targets.

#### Images

| Repeated value | All declaration locations |
| --- | --- |
| `ghcr.io/cloudnative-pg/postgresql:18` | `G/cnpg-cluster.yaml:39`; `I/cnpg-cluster.yaml:34`; `P/wamn-sysdb.yaml:40` |
| `nats:2.10-alpine` | `G/nats-jetstream.yaml:96`; `I/nats-jetstream.yaml:145` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/components@sha256:7ac3316eb660a630efe7fae991bc7931013c0bcadbb3e3b1a1234c1f7f621105` | `G/flow-http.yaml:45`; `G/wms-flow-http.yaml:45` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/components@sha256:f98dbc816d281b815f4e5d57f1ac92548b478802c1e62198dac92607fb0b296c` | `G/materializer.yaml:42`; `G/wms-materializer.yaml:42` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-cdc-reader@sha256:9b1ed15e87122ce208114582b27ce23af7cc06c80653c798fcac5b79172e1c52` | `G/cdc-reader.yaml:45`; `G/cdc-reader-wms.yaml:27` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-runner:0.1.36@sha256:ef8956e916419aa7d169acedebecc078fde5d7ecdf8bcbe0f13dc21123d0187a` | `G/operator/invite.yaml:33`; `G/operator/mint-pat.yaml:36` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-gates:src-38f6225813d8b634@sha256:eb5cfb8f0f0531bc85ab5e9bb45d16866b902c919c76d48457a31ddd797479c4` | Both `G/environments/*.json:18` |
| `wamn-ctl:dev` | `P/audit-retention.example.yaml:52`; `P/run-retention.example.yaml:68` |
| `wamn-gates:dev` | `deploy/gates/serve-echo.yaml:26`; `deploy/gates/socket-test-job.yaml:27`; `deploy/gates/trace-test-job.yaml:45` |
| `curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777` | `C/check.rs:18`; `C/identity.rs:19` |
| `debian:trixie-slim` | `Dockerfile:109,158,168,182,189,196` |

#### URLs, hosts, and artifact bases

| Repeated value | All declaration locations |
| --- | --- |
| `https://identity.identity.svc.cluster.local` | `G/host-group-receiving.yaml:56`; `G/host-group-wms.yaml:55`; `G/operator/invite.yaml:43`; `G/operator/mint-pat.yaml:58`; `G/values-host.yaml:86,231,320`; `G/values-identity.yaml:10`; `C/control.rs:192,194`; `C/edge.rs:22`; `C/identity.rs:15,352`; `C/install.rs:341`; `C/invite.rs:131`; `C/steps.rs:184,214`. Certificate DNS repeats the host at `G/identity-certificate.yaml:13`. |
| `nats://evt-nats.platform.svc.cluster.local:4222` | `G/cdc-reader.yaml:60`; `G/cdc-reader-wms.yaml:42`; `G/evt-nats-check.yaml:20`; `G/host-group-receiving.yaml:30`; `G/host-group-wms.yaml:29`; `G/values-host.yaml:24,169`; `C/broker.rs:20`; `C/cdc.rs:108`; `C/environments.rs:248`; `C/steps.rs:190,259` |
| `nats://evt-nats.wamn-system.svc.cluster.local:4222` | `G/values-host-base.yaml:29`; `P/values-host-default.yaml:170`; `P/values-host-receiving-pat.yaml:57`; `P/values-host-wms-pat.yaml:58` |
| `nats://nats.platform.svc.cluster.local:4222` | `G/host-group-receiving.yaml:14,15`; `G/host-group-wms.yaml:13,14`; `G/values-host-base.yaml:133,134`; `G/values-identity.yaml:18`; `C/control.rs:182,183`; `C/identity.rs:254`; `C/job.rs:125`; `C/steps.rs:157` |
| `nats://nats.wamn-system.svc.cluster.local:4222` | `P/values-host-default.yaml:77,78` |
| `http://127.0.0.1:8222/healthz` | `G/nats-jetstream.yaml:122,128`; `I/nats-jetstream.yaml:173,179` |
| `registry.wamn-system.svc.cluster.local:5000/wamn/components` | `G/values-host-base.yaml:27`; `P/values-host-default.yaml:165`; `P/values-host-receiving-pat.yaml:55`; `P/values-host-wms-pat.yaml:56` |
| `registry.wamn-system.svc.cluster.local:5000/wamn/releases` | `P/values-host-receiving-pat.yaml:144`; `P/values-host-wms-pat.yaml:147` |
| `registry.wamn-system.svc.cluster.local:5000` | Docker credential authorities at `P/registry-credentials.example.yaml:54,64,74` |
| `registry.wamn-system.svc.cluster.local` | Certificate names at `P/registry.yaml:145,150` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn` | `G/cluster.k:10`; both `G/environments/*.json:4` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/components` | `G/host-group-receiving.yaml:28`; `G/host-group-wms.yaml:27`; `G/values-host.yaml:22,167` |
| `us-central1-docker.pkg.dev/wamn-dev/wamn/releases` | `G/values-host.yaml:113,258` |
| `oci://ghcr.io/wasmcloud/charts` | `R/Chart.yaml:13`; `R/Chart.lock:3`. Installer separately appends the chart name at `C/install.rs:1008`. |
| `wamn-pg-rw.platform.svc.cluster.local` | `C/install.rs:276`; `C/steps.rs:172,202` |
| `postgres.wamn-system.svc.cluster.local:5432/wamn` | URL targets at `P/host-db.example.yaml:35`; `P/audit-retention-db.example.yaml:42`; `P/run-retention-db.example.yaml:58` |
| `receiving.wamn.dev` | `G/environments/dkk--receiving--dev.k:9`; matching JSON `:17`; `G/flow-http.yaml:33` |
| `wms.wamn.dev` | `G/environments/dkk--wms--dev.k:9`; matching JSON `:17`; `G/wms-flow-http.yaml:33` |
| `control.wamn.dev` | `G/cluster.k:13`; `G/control-service.yaml:37`; HTTPS invitation URL at `G/values-identity.yaml:16` |
| `https://{host}{path}` | Rendering expressions at `C/check.rs:150,246` |

#### Namespaces

The following are explicit namespace declarations and namespace lists in manifests. Rust call sites additionally repeat the fixed platform namespaces.

| Namespace | Manifest locations |
| --- | --- |
| `hosts` | `G/cdc-reader-wms.yaml:10`; `G/cdc-reader.yaml:20,28`; `G/control-service.yaml:7,20`; `G/flow-http.yaml:5,20`; `G/materializer.yaml:5`; `G/registry-helper.yaml:11`; `G/values-host-base.yaml:8`; `G/values-host.yaml:3,148,293`; `G/values-operator.yaml:6,8`; `G/wms-flow-http.yaml:5,20`; `G/wms-materializer.yaml:5` |
| `platform` | `G/auth-nats-clients.yaml:29`; `G/cnpg-cluster.yaml:25`; both environment JSON files `:6`; `G/evt-nats-check.yaml:10`; `G/nats-jetstream.yaml:11,41,61,77` |
| `identity` | `G/auth-nats-clients.yaml:11`; `G/identity-certificate.yaml:9`; `G/identity-operator-ca.yaml:11,19,32,41`; `G/operator/invite.yaml:22`; `G/operator/mint-pat.yaml:25` |
| `__ENVIRONMENT_NAMESPACE__` | `P/host-environment-certs.example.yaml:79,102`; `P/runtime-operator-events-rbac.example.yaml:19,29` |
| `wamn-system` | `deploy/gates/serve-echo.yaml:15,39`; socket Job `:17`; trace Job `:26`; `I/cnpg-cluster.yaml:25`; `I/minio.yaml:22,39,85,102`; `I/nats-jetstream.yaml:32,74,97,113`; `I/otel-collector.yaml:8,46,95`; `I/prometheus.yaml:12,35,76`; `I/values-wamn.yaml:169,181`; `P/audit-retention-db.example.yaml:38`; audit CronJob `:29`; `P/event-reader-rbac.yaml:19`; host database example `:32`; HTTP example `:12,28`; identity database example `:13`; materializer example `:41`; `P/postgres.yaml:21,91,114`; registry credentials example `:39,50,60,70`; `P/registry.yaml:119,137,186,198,279`; run-retention database example `:54`; run-retention CronJob `:42`; operator event RBAC `:37`; scenario worker `:133,215`; host default values `:116`; Receiving PAT values `:33`; WMS PAT values `:36`; `P/wamn-sysdb.yaml:36` |
| `cert-manager` | `I/cert-manager.yaml:12789,12804,12819,13234,13254,13274,13294,13314,13334,13354,13374,13394,13416,13472,13511,13535,13542,13556,13563,13587,13611,13640,13704,13787,13913,13959` |
| `kube-system` | `I/cert-manager.yaml:13425,13451,13497,13521` |
| `cnpg-system` | `I/cnpg-operator.yaml:19173,19569,20061,20067,20081,20185,20205,20225,20245,20271,20291,20311,20331,20351`; `I/barman-cloud-plugin.yaml:727,736,923,931,944,960,970,984,999,1070,1088,1108` |

Current Rust namespace reads and writes repeat:

| Namespace | Rendering and operation locations |
| --- | --- |
| `hosts` | `C/packages.rs:127`; `C/environments.rs:287,358,425`; `C/steps.rs:202,208,253`; `C/control.rs:99,152,159,181,269,288,299,320,364`; `C/install.rs:376,530,602`; `C/manifests.rs:38`; `C/cdc.rs:31,52,69,84`; `C/edge.rs:216,219`; `C/web.rs:55,102,109,166`; `C/check.rs:67`; `C/identity.rs:138,214`; `C/broker.rs:319,325`; `D/control.rs:86` |
| `platform` | `C/packages.rs:111,160`; `C/pools.rs:70,97,110`; `C/environments.rs:27,38,240,348,426`; `C/steps.rs:202`; `C/install.rs:48,398,530,553,627,653,667,674,681,695,747,961,985,1006`; `C/manifests.rs:45`; `C/job.rs:686,936,940,1039`; `C/edge.rs:293`; `C/check.rs:92,179`; `C/identity.rs:113,177,339`; `C/broker.rs:105,160,321`; `C/invite.rs:56,97` |
| `identity` | `C/control.rs:123,158`; `C/install.rs:345,422,436,530`; `C/identity.rs:72,91,99,108,177,188,275,294,296,314,317,379,455`; `C/invite.rs:143` |
| `edge` | `C/install.rs:376,518`; `C/edge.rs:60,70,87,106,119,192,209,221,222,223,226,233,259,293,308`; `C/check.rs:47,78,257` |
| `wamn-system` | Namespace translation at `C/manifests.rs:44` |

WIT namespaces also repeat across the workload representations:

| Namespace | Locations |
| --- | --- |
| `wasi` | `G/control-service.yaml:32`; both GCP HTTP workloads `:27`; `P/http-route-workload.example.yaml:52`; `R/templates/workloads.yaml:23`; `D/activation.rs:1185` |
| `wasmcloud` | Both GCP materializers `:24`; `P/materializer.example.yaml:70`; `R/templates/workloads.yaml:87` |
| `wamn` | `G/control-service.yaml:38,42,46`; both GCP HTTP workloads `:34,39`; both GCP materializers `:12,18,31,36`; HTTP example `:56,60,64,68,72`; materializer example `:62,66,75,79`; `R/templates/workloads.yaml:29,33,37,41,46,79,83,92,96`; `D/activation.rs:572,1195,1196,1197,1198` |

#### Database names and versions

| Value | Locations |
| --- | --- |
| `wamn_system` database and matching bootstrap owner | `G/cnpg-cluster.yaml:60,61`; both environment JSON files `:8`; `P/wamn-sysdb.yaml:76,77`; `C/install.rs:297`; `C/control.rs:20`; `C/invite.rs:26,69`; `C/identity.rs:354`; descriptive renderer output at `C/steps.rs:172,184` |
| `wamn` database | `deploy/sql/postgres-init.sql:148,150`; `P/postgres.yaml:72`; database URL examples at host `:35`, audit-retention `:42`, run-retention `:58` |
| `app` database and matching owner | `I/cnpg-cluster.yaml:65,66` |
| Operator `2.11.0` | `R/Chart.yaml:12`; `R/Chart.lock:4`; `C/install.rs:24`; `C/steps.rs:157`; archive filename `R/charts/runtime-operator-2.11.0.tgz`. The archive’s `runtime-operator/Chart.yaml` repeats it as `appVersion` and `version`. |
| Platform `0.1.137` | `R/Chart.yaml:5,6` |
| WIT/chart `0.1.0` | `G/control-service.yaml:40,44`; both GCP HTTP workloads `:36`; both GCP materializers `:20,26,33`; `P/edge/Chart.yaml:5,6`; `P/identity/Chart.yaml:5,6`; HTTP example `:58,66`; materializer example `:68,72,77`; `R/templates/workloads.yaml:31,39,85,89,94`; `D/activation.rs:1195,1197` |
| WIT `0.3.0` | `G/control-service.yaml:34`; both GCP HTTP workloads `:29`; HTTP example `:54`; `R/templates/workloads.yaml:25`; `D/activation.rs:547,1187` |
| WIT `0.4.0` | `G/control-service.yaml:48`; both GCP HTTP workloads `:41`; both GCP materializers `:14,38`; HTTP example `:62,70,74`; materializer example `:64,81`; `R/templates/workloads.yaml:35,43,48,81,98`; `D/activation.rs:572,1196,1198` |
| Guest Rust `1.99.0` | `Dockerfile:117,118,137,141,143,145,147`; native base separately uses `rust:1.99-trixie` at `Dockerfile:20` |
| Local image tag `dev` | `P/identity/values.yaml:3`; `P/values-host-default.yaml:101`; also embedded in the repeated image references above |
| Integration version `1` | `G/environments/dkk--wms--dev.k:12,13` |
| Generator metadata `v0.21.0` | `I/cnpg-operator.yaml:12,484,704,8353,8950,9027,9246,18629,18827,19021`; `I/barman-cloud-plugin.yaml:5` |
| cert-manager `v1.21.0` | Bundle labels at `I/cert-manager.yaml:33,3364,3653,3978,4891,8843,12795,12810,12825,12838,12870,12896,12922,12957,12998,13058,13095,13112,13135,13163,13185,13211,13227,13247,13267,13287,13307,13327,13347,13367,13387,13408,13431,13457,13478,13503,13527,13548,13569,13593,13617,13646,13661,13710,13725,13793,13808,13887,13926`; image tags at `:13675,13739,13822` |

The equal `0.1.0` literals represent several separate contracts. They require ownership by package/interface, not a single global version constant.

#### Shared installation identity

| Value | Independent locations |
| --- | --- |
| Project `wamn-dev` | `G/cluster.k:5`; `G/project.k:4`; `G/guard/config.json:2`; `G/letsencrypt.yaml:23,24`; `G/otel-collector-values.yaml:10`. It also appears inside the registry references listed above. |
| Zone `us-central1-a` | `G/cluster.k:6`; `G/project.k:5,13`; `G/guard/config.json:3` |
| Cluster `wamn` | `G/cluster.k:7`; `G/project.k:6`; `G/guard/config.json:4`; both environment JSON files `:2`; `G/otel-collector-values.yaml:9` |
| Web bucket `wamn-dev-web` | `G/cluster.k:11`; both environment JSON files `:14` |
| Telemetry bucket `wamn-dev-otel` | `G/cluster.k:11`; `G/otel-collector-values.yaml:8` |
| State bucket `wamn-dev-tofu-state` | `G/cluster.k:11`; `G/project.k:21`; `G/guard/config.json:5` |
| Sender `noreply@wamn.dev` | `G/cluster.k:15`; `G/values-identity.yaml:15` |
| Owner email `dkkloimwieder@gmail.com` | `G/cluster.k:9`; `G/letsencrypt.yaml:17` |

## Open questions for the owner

1. Which prototype manifests remain in use outside repository automation, especially backup, retention, registry, and bootstrap resources?
2. Must local WMS retain its distinct concurrency limit, or does one platform host policy replace both application policies?
3. Does the platform image identity need to cover deployed CDC bytes, alongside the host and role components?

## Proposed fix epics

### One environment declaration

1. Derive build and publication packages from compiled KCL
2. Remove legacy environment JSON configuration
3. Render cloud destinations from their owning documents

### One workload and host definition

1. Move test and local consumers onto shared workload contracts
2. Remove historical workloads and host snapshots
3. Publish the dedicated CDC artifact through the platform image owner

### Retire deployment prototypes

1. Reuse the pinned operator artifact for cluster and host installation
2. Replace manual operator templates with current Job rendering
3. Remove orphan examples and preserve surviving behavioral tests

<!-- lane D2, bead wamn-xm69b.2 -->

## Part 2: pipeline

The target is one artifact build graph and one deployment reconciler. The root Dockerfile survives. The older upgrade image builder, separate RC Dockerfile, and deployment-time compilation disappear.

This was a static, read-only audit. No builds or tests ran. No files changed, and the final Git status was clean.

The current pipeline has separate test, platform-build, and deployment paths:

```text
Commit
 |
 +-- tools/train-run                         [repository test owner]
 |    detached worktree + shared target
 |      -> tools/test-changes                [test selection owner]
 |      -> generated inputs + native/guest/web tests
 |      -> lint
 |    Stops here: no published release or deployment receipt.
 |
 +-- Application packages                    [schema generator / package owner]
 |    wamn build + verification PostgreSQL
 |      -> generated SQL, guests, clients, receipts
 |      -> push-package + publish-release     [wamn-control publication owner]
 |
 +-- Platform image set                      [platform_build.rs]
 |    UI build + input hashes
 |      +-> local Docker builds
 |      +-> Cloud Build, explicitly or when another builder appears busy
 |    -> five container images + two role components
 |    -> stamped release chart
 |
 +-- qualify-release                         [delivery qualification owner]
 |    release digest + chart image digests
 |      -> runner container
 |      -> Receiving/WMS cases on owned kind clusters
 |      -> qualification artifact
 |
 +-- env apply                               [environment lifecycle owner]
      qualification admission + lifecycle lock
        -> database/package expansion
        -> Helm rollout
        -> runtime-operator WorkloadDeployments
        -> readiness -> drain -> contract
        -> running release

Parallel older path:
upgrade-environment                          [upgrade_environment.rs]
  source -> native builds -> three source-tagged images -> two guests
    -> preflight -> schema -> packages -> qualify -> deploy -> retire
  Qualification and deployment still select the externally supplied chart.
```

The train ends at `tools/train-run:80`. The older upgrade stages appear at `crates/control/lib/src/delivery/run_record.rs:35`. The environment sequence appears at `crates/control/lib/src/environment/mod.rs:276`.

The shell-out inventory has these replacement boundaries:

Docker manifest inspection, tag copying, and registry publication belong in the existing OCI client, the library for container registry operations. Docker or BuildKit remains appropriate for image construction. Container execution remains appropriate for isolated qualification fixtures. Evidence: `crates/control/lib/src/platform_build.rs:326`, `crates/control/lib/src/platform_build.rs:739`, `crates/control/lib/src/platform_build.rs:834`, and `crates/control/lib/src/platform_build.rs:544`.

Kubectl object reads, writes, waits, and context inspection become Kubernetes client calls. First-party chart rendering becomes typed Rust resource construction. Third-party charts remain build inputs, rendered and pinned before deployment. An operator, a service that continually applies declared state, owns rollout and recovery. Evidence: `crates/control/lib/src/kubectl.rs:63`, `crates/control/lib/src/environment/analyse.rs:687`, `crates/control/lib/src/environment/mod.rs:193`, and `crates/control/lib/src/release_chart.rs:667`.

Gcloud token, secret, Cloud Build, and pool operations become provider API calls with one credential source. Interactive cluster bootstrap can retain gcloud. Kind remains a test-only process boundary. Evidence: `crates/control/lib/src/platform_build.rs:1018`, `crates/control/lib/src/platform_build.rs:1275`, `crates/control/lib/src/platform/provider.rs:26`, `crates/control/lib/src/platform/provider.rs:86`, and `crates/control/lib/src/platform/pools.rs:50`.

Git worktree operations, Cargo compilation, pnpm builds, and PostgreSQL dump/restore remain external tools. Replace recursive calls to WAMN binaries with their library operations. Keep local host, identity, scenario-worker, and CDC processes under explicit supervision. Evidence: `crates/control/lib/src/upgrade_environment.rs:282`, `crates/control/lib/src/integration.rs:104`, `crates/control/lib/src/platform/build.rs:57`, `crates/control/lib/src/qualify_upgrade/scratch.rs:101`, and `crates/control/lib/src/dev/cdc_reader.rs:73`.

The shared count is 71 `Command::new` occurrences in control and one in ctl, including embedded tests. These are constructor counts, not release execution counts. The ctl occurrence is a signal-test subprocess at `services/ctl/src/dev/command.rs:302`. Source: measurements.md section 2.

A cold platform publication produces five container images: host, runner, gates, identity, and edge-web. It also produces two WebAssembly role artifacts, for seven platform registry artifacts. Local execution makes five image-build calls and one role-export call. Cached execution builds zero to five container images but still requests the role export. Stamp-only builds none. Evidence: `crates/control/lib/src/platform_build.rs:31`, `crates/control/lib/src/platform_build.rs:782`, and `crates/control/lib/src/platform_build.rs:799`.

An application release can reuse all platform images and build zero new native images. Its package and component artifact counts depend on its manifest. The older upgrade path separately prepares three container images and two guests. RC separately builds host, gates, and PostgreSQL images. Evidence: `crates/control/lib/src/upgrade_environment.rs:32`, `crates/control/lib/src/upgrade_environment.rs:37`, and `tools/rc-gate-run:10`.

No defensible build-time total exists in the supplied measurements. Measurements.md sections 1–11 contain no timed release build. Stage timers exist, but they do not establish cold, cached, or commit-to-running durations.

## Finding D2-1: A clean upgrade lacks the application builds that it consumes

Severity: high  
Effort: M  
Area: crates/control/lib/src/upgrade_environment.rs  
Evidence: `crates/control/lib/src/upgrade_environment.rs:282`, `crates/control/lib/src/upgrade_environment.rs:320`, `crates/control/lib/src/upgrade_environment.rs:348`, `crates/control/lib/src/upgrade_environment.rs:769`, `crates/schema/generator/src/receipt.rs:178`, `docs/architecture/overview.md:13`.  
Problem: The upgrade creates a detached worktree, then builds native programs and two platform guests. It never builds the declared application packages before requiring their current receipts. Generated application output is absent from Git, so the upgrade cannot complete from a fresh worktree.  
Proposal: Build and publish the complete package closure once before deployment. Make an upgrade consume package and platform artifact digests without compiling source.  
Deletes: Application build assumptions in the upgrade stages and deployment-time native/guest preparation.  
Risk: Existing upgrades can depend on manually prepared output. A new clean-worktree upgrade test must reach qualification without preexisting `apps/target` contents.

## Finding D2-2: The older image pipeline builds artifacts that deployment does not select

Severity: high  
Effort: M  
Area: crates/control/lib/src/upgrade_environment.rs; tools/journey-image-cache  
Evidence: `crates/control/lib/src/upgrade_environment.rs:385`, `crates/control/lib/src/upgrade_environment.rs:434`, `crates/control/lib/src/upgrade_environment.rs:927`, `crates/control/lib/src/upgrade_environment.rs:984`, `crates/control/lib/src/upgrade_environment.rs:1415`, `tools/journey-image-cache:19`, `Dockerfile:124`, `Dockerfile:171`.  
Problem: The older path builds host, identity, ctl, and two workload guests, but qualification and deployment select the platform chart. Only ctl has an explicit downstream image-record consumer, for the PAT Job. Its separate cache also omits `tools/guest-rustflags`, although the ctl image embeds a guest built with that script. A script-only change therefore leaves this cache identity unchanged.  
Proposal: Keep `platform_build.rs` and the root Dockerfile as the sole platform artifact producer. Supply the administrative Job image through that same published artifact set.  
Deletes: `tools/journey-image-cache`, the upgrade image and guest stages, their leases, relabeling, and duplicate registry authentication.  
Risk: PAT provisioning still needs a runnable control binary. A pipeline test must prove that qualification, deployment, and administrative Jobs consume the declared artifact set.

## Finding D2-3: Cloud failures leave published version tags without a complete platform set

Severity: high  
Effort: M  
Area: crates/control/lib/src/platform_build.rs  
Evidence: `crates/control/lib/src/platform_build.rs:785`, `crates/control/lib/src/platform_build.rs:811`, `crates/control/lib/src/platform_build.rs:1370`, `crates/control/lib/src/platform_build.rs:1378`, `crates/control/lib/src/platform_build.rs:1392`, `crates/control/lib/src/platform/steps.rs:121`.  
Problem: Local execution finishes every build before publication starts. Cloud execution pushes each image directly under the platform version while later targets remain unfinished. A later failure leaves partial version tags, and immutable registries prevent replacing those tags after a source fix. Local publication can also stop after publishing only part of the set.  
Proposal: Publish content-addressed outputs first. Publish one platform-set manifest only after every required artifact succeeds. Make stamping consume that manifest digest.  
Deletes: Independent resolution of seven version tags as the platform publication boundary.  
Risk: Stamp-only and version-based reinstall behavior change. Fault-injection tests must fail each build and push stage, then prove that no incomplete set becomes deployable.

## Finding D2-4: Several command owners bypass bounded process supervision

Severity: high  
Effort: M  
Area: crates/control/lib/src/owned_command.rs; kubectl.rs; platform/provider.rs; platform_build.rs  
Evidence: `crates/control/lib/src/owned_command.rs:35`, `crates/control/lib/src/owned_command.rs:58`, `crates/control/lib/src/kubectl.rs:78`, `crates/control/lib/src/platform/provider.rs:195`, `crates/control/lib/src/platform_build.rs:1423`, `crates/control/lib/src/upgrade_environment.rs:249`, `crates/control/lib/src/environment/lock.rs:87`.  
Problem: The shared executor owns process groups, deadlines, signals, and cleanup. Kubernetes, provider, Helm, and platform-build helpers instead await subprocess completion directly. An outer readiness deadline cannot bound a subprocess that never returns, and an environment verb retains its lifecycle lock. The older upgrade timeout also kills only its direct child through `kill_on_drop`.  
Proposal: Remove subprocesses where client calls suffice. Route every remaining finite command through the existing owned executor with an operation deadline.  
Deletes: Duplicate `output`, `run`, and command-timeout implementations.  
Risk: Some commands need cleanup time after cancellation. Extend `stuck_children_cancel_and_reap_descendants_with_diagnostics` to exercise each retained command adapter and prove lock release.

## Finding D2-5: Deployment ownership is spread across CLI tools and Helm history

Severity: medium  
Effort: L  
Area: crates/control/lib/src/environment; crates/control/lib/src/release_chart.rs; crates/control/lib/src/platform  
Evidence: `crates/control/lib/src/environment/analyse.rs:576`, `crates/control/lib/src/environment/analyse.rs:600`, `crates/control/lib/src/environment/write.rs:97`, `crates/control/lib/src/environment/mod.rs:276`, `crates/control/lib/src/kubectl.rs:26`, `crates/control/lib/src/platform/install.rs:1026`.  
Problem: Rust already owns deployment decisions, but it reads deployment history through Helm and writes resources through kubectl. Transport classification depends on an English stderr prefix. Recovery requires another verb invocation because the lifecycle sequence runs inside the caller.  
Proposal: Store desired release and observed progress under one environment reconciler. Use the Kubernetes client for resources and watches. Construct first-party resources in Rust, and render third-party charts during artifact production. Preserve database expansion, readiness, drain, and contract ordering.  
Deletes: Kubectl wrappers, Helm history parsing, first-party chart templates, and CLI-owned reconciliation loops.  
Risk: Rollback, credentials, and database transitions retain strict ordering requirements. Existing environment lifecycle tests must prove retry, rollback, concurrent-apply refusal, drain, and deletion behavior against the reconciler.

## Finding D2-6: Installation couples package freshness to an unrelated platform stamp

Severity: medium  
Effort: M  
Area: crates/control/lib/src/platform/build.rs  
Evidence: `crates/control/lib/src/platform/build.rs:30`, `crates/control/lib/src/platform/build.rs:39`, `crates/control/lib/src/platform/build.rs:57`, `crates/control/lib/src/platform/build.rs:165`, `crates/control/lib/src/platform/packages.rs:35`, `crates/control/lib/src/platform/install.rs:80`.  
Problem: An existing platform stamp skips package builds and the UI build before their freshness is considered. Later package publication still requires current package receipts. A changed application under an unchanged platform version therefore reaches a stale-output refusal. The library also invokes a debug `wamn-ctl` executable to perform its own stamp operation.  
Proposal: Make installation consume published package, UI, and platform artifacts. Give each artifact its own identity. Call the stamp library directly during artifact production.  
Deletes: Installer PostgreSQL container management, package compilation, pnpm invocation, debug executable lookup, and recursive ctl execution.  
Risk: Installation loses its implicit local preparation behavior. A test with an unchanged platform and changed application must select the new package without rebuilding platform images.

## Finding D2-7: Cluster gates retain separate artifact recipes and duplicated lifecycle code

Severity: medium  
Effort: M  
Area: tools/*gate-run; tools/*journey-run; tests/integration/src/rc/Dockerfile  
Evidence: `tests/integration/src/rc/Dockerfile:17`, `Dockerfile:68`, `tools/rc-gate-run:12`, `tools/receiving-cluster-journey-run:17`, `tools/wms-cluster-journey-run:19`, `tools/identity-jwks-journey-run:183`, `tools/kind-gate-build:93`; measurements.md section 7 records 78 duplicated lines between the application runners.  
Problem: RC builds debug host and gate images through a separate Dockerfile, while platform publication builds release images. The identity journey rebuilds another pair of images directly. Receiving and WMS correctly consume candidate images, but duplicate cluster, image-loading, broker, and cleanup mechanics. These paths maintain different recipes for proving the deployed system.  
Proposal: Make release qualification consume only the canonical published artifacts. Keep debug builds for development. Put disposable infrastructure ownership in one shared test fixture, while applications retain their assertions.  
Deletes: The RC Dockerfile, redundant journey image builds, and duplicated lifecycle branches. Retire per-node gate-image cleanup when disposable clusters replace retained gate clusters.  
Risk: Preserve exact test counts, negative-effect proofs, resource ownership, and teardown evidence. RC, Receiving, WMS, JWKS/session, and `tools/kind-cluster-delete-test` must retain their existing assertions.

## Finding D2-8: The train cannot establish release build cost or commit-to-running time

Severity: medium  
Effort: S  
Area: tools/train-run; tools/test-changes; crates/control/lib/src/platform_build.rs  
Evidence: `tools/train-run:60`, `tools/train-run:74`, `tools/train-run:80`, `tools/test-changes-suites.json:2`, `tools/test-changes:293`, `crates/control/lib/src/platform_build.rs:60`, `crates/control/lib/src/platform_build.rs:71`; measurements.md section 3 records missing generated inputs during direct workspace linting.  
Problem: The train records broad step timestamps but deletes successful logs and ends after source tests. Platform builds print stage durations, while their receipt stores only the UI digest. No retained record joins source testing, artifact production, qualification, and readiness. The always-selected quality suite also generates application inputs, so “selected tests” alone does not describe the work performed.  
Proposal: Retain one release receipt with stage durations, cache outcomes, artifact digests, built/copied counts, qualification result, and readiness time. Measure cold and cached runs separately.  
Deletes: Success-path evidence deletion and duplicate textual timing summaries.  
Risk: Receipts must exclude credentials and distinguish reused work from executed work. Train-tool tests must assert retained successful evidence, and a timed release run must reconcile its receipt with actual registry artifacts.

## Open questions for the owner

1. Is `upgrade-environment` still a supported entry point, or can the artifact-driven pipeline replace its build stages entirely?
2. Must kind and GCP both execute builds, or can local execution remain a developer adapter to one release build graph?
3. Which cold-build, cached-build, qualification, and commit-to-running budgets must the retained release receipt enforce?

## Proposed fix epics

### One artifact build graph

1. Build application packages before publication from a clean checkout
2. Delete obsolete upgrade and RC artifact recipes
3. Publish complete platform sets by digest

### One deployment owner

1. Replace kubectl and provider shell-outs with client calls
2. Move environment lifecycle work into an operator
3. Remove installer compilation and recursive ctl execution

### One qualification and evidence path

1. Share disposable cluster fixtures across gate owners
2. Bound every retained child process
3. Retain release timing and artifact receipts

<!-- lane D3, bead wamn-xm69b.3 -->

## Part 3: target path

## Finding D3-1: Installation still builds and publishes its own candidate

Severity: high  
Effort: L  
Area: `crates/control/lib/src/platform/`  
Evidence: `crates/control/lib/src/platform/build.rs:30`, `crates/control/lib/src/platform/build.rs:39`, `crates/control/lib/src/platform/releases.rs:67`, `crates/control/lib/src/platform/install.rs:107`.

Problem: `platform apply` builds packages and the UI, stamps images, and publishes releases through the target cluster. It can return success after requesting another commit and another invocation. The installer therefore remains part of candidate creation, rather than consuming one finished candidate.

Proposal: Keep `platform apply` as the single deployment entry point. Its input must name an immutable bundle, meaning a set of artifacts fixed by their digests. The target path is:

1. Select the exact integrated commit and submodule revisions.
2. Run one build workflow for packages, native images, role components, web assets, and the chart.
3. Publish the candidate from an isolated build catalog, without production database access.
4. Run one qualification workflow against those published bytes.
5. Commit the qualified release selection in the environment document.
6. Run `platform apply`, which consumes artifacts and performs provisioning, rollout, readiness, drain, and contraction.

Keep `env apply` as the shared environment implementation beneath that entry point. Fresh installation and later deployment use the same stages.

Deletes: Package and UI compilation from `platform/build.rs`, publication from `platform/releases.rs`, and the installer’s request to commit newly discovered release digests.

Risk: Publication currently uses target credentials and database state. A proposed disconnected-install test must deploy a completed bundle without Cargo, pnpm, source files, or registry write access.

## Finding D3-2: Qualification does not identify all deployed behavior

Severity: high  
Effort: L  
Area: `crates/control/lib/src/delivery/qualification_artifact.rs`  
Evidence: `crates/control/lib/src/delivery/qualification_artifact.rs:57`, `crates/control/lib/src/delivery/qualification_artifact.rs:92`, `crates/control/lib/src/delivery/qualification.rs:174`, `crates/control/lib/src/platform/cdc.rs:21`.

Problem: Qualification reuse depends on the release, host, HTTP component, and materializer component. It excludes chart templates, identity, edge, gates, and the runner. The gates image also supplies the deployed CDC reader, so its exclusion reaches production behavior. The current qualification proves a narrower unit than the proposed deployment path needs.

Proposal: Qualify the release against the complete deployment bundle. That bundle fixes the chart and dependencies, deployed images, components, web assets, and qualification executable. Reuse evidence only when those inputs and required cases match. Preserve the environment-independent release digest, and record platform compatibility separately in the qualification artifact.

This proposal changes the existing R12 policy explicitly. Replace the artifact format and refuse older evidence rather than adding compatibility branches. Keep `qualify-upgrade` as a database transition proof inside the qualification workflow.

Deletes: The qualification identity split between `ImageSet` and `QualificationImages`, and reuse across changed deployment templates or deployed auxiliary images.

Risk: Broader identity reduces qualification reuse. A proposed mutation test must change the chart, identity, CDC reader, edge, or case executable independently and require fresh evidence.

## Finding D3-3: The merge train stops before deployment qualification

Severity: high  
Effort: L  
Area: `tools/train-run`  
Evidence: `tools/train-run:67`, `tools/train-run:80`, `tools/test-changes:436`, `crates/control/lib/tests/platform_apply_kind.rs:96`, `docs/plan/delivery.md:6`.

Problem: The train tests a committed revision, but its final step is `tools/test-changes run`. The deployment supplement runs prekind cases and one native host case, rather than the full ignored platform installation. The train produces no completed deployment bundle or qualification record. Successful logs also disappear after the run.

Proposal: Keep the current change selection as the first train stage. Add the single artifact build, full platform installation on kind, application qualification, and retained results for the exact combined commit. Merge only that tested commit, or restart when the integration head changes. A separate trusted promotion job must select the qualified bundle and apply it.

The CI service supplies triggers, credentials, concurrency, and result storage. Repository commands continue to own behavior. Preserve generated package preparation, because measurements.md section 3 records compile failures when those inputs are absent.

Deletes: Successful-run evidence deletion from `train-run`, and the assumption that a passing native test train also qualifies deployment.

Risk: Testing a combined head and merging different bytes invalidates the result. Extend `tools/train-tools-test` with moved-head, failed-qualification, missing-artifact, and promotion-refusal cases.

## Finding D3-4: Desired deployment inputs still have several owners

Severity: medium  
Effort: M  
Area: `deploy/gcp/environments/`  
Evidence: `deploy/gcp/environments/dkk--receiving--dev.k:8`, `deploy/gcp/environments/dkk--receiving--dev.json:3`, `deploy/gcp/environments/dkk--receiving--dev.json:17`, `crates/control/lib/src/platform/build.rs:125`, `crates/control/lib/src/platform/environments.rs:261`.

Problem: Each environment combines KCL, an adjacent JSON file, and a project-specific host YAML file. The JSON file repeats the route host and supplies cluster, registry, package roots, and a gates image. The installer reads those independent files rather than deriving one complete set of inputs.

Proposal: Give each fact one owner:

1. `wamn.k` owns package declarations and build inputs.
2. `project.k` owns cloud infrastructure managed by OpenTofu.
3. `cluster.k` owns provider configuration and references to external resources.
4. `environment.k` owns the coordinate, application mapping, release selection, policy, integration selections, and floors.
5. The immutable bundle owns artifact digests, chart dependencies, and qualification tools.

Derive package locations from the build graph and host configuration from these inputs. Credentials and live integration records retain their existing authorities. Generated JSON, YAML, and `.tf.json` remain outputs.

Deletes: Both committed environment JSON sidecars, `EnvironmentFile`, and the authored `host-group-receiving.yaml` and `host-group-wms.yaml` overlays.

Risk: Package discovery must preserve base-before-overlay order. Extend the existing environment round-trip tests to cover both applications without sidecars or project-named host files.

## Finding D3-5: Helm plus KCL removes more machinery than a Rust operator

Severity: medium  
Effort: L  
Area: `deploy/platform/`  
Evidence: `deploy/platform/release/Chart.yaml:7`, `crates/control/lib/src/platform/manifests.rs:26`, `crates/control/lib/src/platform/manifests.rs:112`, `crates/control/lib/src/platform/cdc.rs:78`, `crates/control/lib/src/platform/edge.rs:187`.

Problem: Kubernetes resources come from Helm templates, committed YAML with Rust edits, and Rust-built JSON objects. Provider differences also appear as recursive mutations of rendered objects. A custom Rust operator adds another controller without first removing these competing resource definitions.

Proposal: Recommend Helm plus KCL. KCL compiles and constrains inputs, while Helm alone renders WAMN Kubernetes resources. Rust retains lifecycle decisions, database transactions, credential operations, and bounded execution.

Use one WAMN chart package with explicit platform and environment scopes. Install separate instances for cluster services and each environment, so environment removal cannot remove shared services. Fold identity, edge, CDC, and host resources into that package. Retain upstream runtime-operator, CloudNativePG, and cert-manager as pinned dependencies with explicit installation order.

A Rust operator offers continuous repair and Kubernetes status conditions. It also requires custom resources, upgrades, watches, permissions, and deletion finalizers. The current lifecycle already supplies observation-based recovery through a bounded command, so that trade adds maintenance without a named requirement.

Deletes: `platform/manifests.rs` substitutions, Kubernetes object builders in `platform/cdc.rs` and `platform/edge.rs`, and independent WAMN identity and edge chart packages after their templates move.

Risk: Consolidation changes ownership and deletion behavior. Extend `release_chart_render` and `environment_apply_kind` to prove resource ownership, independent environment removal, and unchanged shared operators.

## Finding D3-6: Nine deployment routes must collapse into one fixture

Severity: medium  
Effort: L  
Area: `tools/`, `test-support/`, `deploy/mvp/`  
Evidence: `tools/receiving-cluster-journey-run:69`, `tools/wms-cluster-journey-run:71`, `tools/rc-gate-run:10`, `test-support/infrastructure/delivery_cluster.rs:9`, `docs/operations/gcp.md:932`. Measurements.md section 7 records 78 duplicated lines between the two application journey scripts.

Problem: The routes below overlap in resource creation, rendering, and lifecycle management. Application journeys put PostgreSQL and NATS in Docker containers, while platform installation uses Kubernetes infrastructure. Those journeys therefore cannot establish the complete deployment path used on GKE.

Proposal: Use `platform apply` and one shared fixture for all cluster-backed qualification. Treat kind and GKE as provider values of that path.

| Current route | Target disposition |
| --- | --- |
| Manual GCP installation | Delete the alternative shell implementation. Retain owner prerequisites and recovery instructions. |
| `platform apply` | Retain as the public deployment entry point. Remove build and publication stages. |
| Direct `env apply` | Retain its lifecycle library beneath the entry point. Remove separate installation instructions. |
| `environment_apply_kind` | Retain lifecycle assertions. Replace its independent infrastructure setup with the common fixture. |
| Kept delivery cluster | Retain coexistence assertions. Remove its separate installer in `delivery_cluster.rs`. |
| Receiving cluster journey | Retain application assertions. Delete its independent lifecycle script. |
| WMS cluster journey | Retain application assertions. Delete its independent lifecycle script. |
| RC gate cluster | Retain relevant protocol assertions. Remove its alternate images and host installation. |
| MVP bootstrap | Delete after credential lifecycle coverage moves to the common path. |

Kind values select local registry transport, storage, certificates, and object storage. GKE values select Artifact Registry, storage classes, Workload Identity, bucket references, and load-balancer integration. Both run the same CNPG, broker, identity, host, CDC, and edge installation stages.

Keep the native `wamn dev` loop as an editing tool. Its process-based activation already declares that it owns no promotion operation at `crates/control/lib/src/dev/activation.rs:1`.

Deletes: `tools/receiving-cluster-journey-run`, `tools/wms-cluster-journey-run`, `tools/delivery-owned`, `tools/rc-gate-run`, `tools/kind-gate-build`, `tests/integration/src/rc/Dockerfile`, and `deploy/mvp/`. Retain generic verdict and cleanup tools where callers still need them.

Risk: Deleting fixtures can discard failure assertions. Move those assertions first, then require Receiving, WMS, coexistence, rollback, drain, credential recovery, and cleanup cases through the common fixture.

## Finding D3-7: The installation lock cannot serialize different deployment machines

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/platform/lock.rs`  
Evidence: `crates/control/lib/src/platform/lock.rs:24`, `crates/control/lib/src/platform/lock.rs:35`, `crates/control/lib/src/platform/install.rs:42`, `crates/control/lib/src/environment/lock.rs:21`.

Problem: Platform installation locks a file in a local work directory. Different machines or work directories can therefore modify the same cluster concurrently. The environment database lock protects individual environments, but it does not serialize shared platform resources or establish promotion order.

Proposal: Give each cluster one deployment queue and a cluster-visible installation lock. Keep the existing environment locks for their database lifecycle. The promotion job must compare its selected commit with the current protected selection before applying. Rollback changes that selection deliberately and follows the same deployment path.

A Kubernetes Lease can coordinate installers before the system database exists. The installer must stop when it loses ownership. Retain Helm as deployment history rather than adding another deployment database.

Deletes: The local file lock as the authority for cluster installation, and duplicated manual deployment writers in the runbook.

Risk: A delayed worker can continue after another worker takes ownership. Proposed concurrency tests must cover two machines, lock loss, interruption, stale promotion, and explicit rollback.

This assessment uses repository reads and shared measurements. No build, test, or deployment ran. The checkout remained clean.

## Open questions for the owner

1. Which CI service and protected reference will own integration and promotion?
2. Which disposable GKE project and budget will support qualification of provider-specific behavior through the same apply path?
3. Can R12 change so that qualification identifies the complete deployment bundle, including the chart and deployed auxiliary images?

## Proposed fix epics

### Epic: One declaration and renderer

1. Consolidate deployment inputs into KCL documents
2. Fold WAMN Kubernetes resources into one Helm package
3. Replace provider mutations with kind and GKE values

### Epic: One built and qualified candidate

1. Move candidate creation entirely into the build workflow
2. Qualify the complete immutable deployment bundle
3. Migrate nine deployment routes to the common fixture

### Epic: One protected promotion path

1. Extend the merge train through artifact qualification
2. Serialize cluster deployment and refuse stale promotion
3. Prove fresh installation, recovery, and rollback on kind and GKE
