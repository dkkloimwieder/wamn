# wasmCloud alignment

@D Kloimwieder

## Goal

wamn follows the wasmCloud runtime-operator path unless it has a stated cause not to. Four changes come out of the architecture review; the last section lists what stays as is and why.

- **One NATS**: the operator, host scheduler and data plane reach one durable NATS; events and the auth memory ride it; per-environment credentials; certificates from a cert-manager CA.
- **Edge and CDN**: an HTTPS load balancer with a Google-managed certificate and Cloud CDN in front of the edge on GCP; nginx reuses upstream connections.
- **WASI version match**: admission matches WASI imports by package and `0.2` family, as the host linker does.
- **Deploy failures**: an install Job that times out reports the host's own error.
- Out of target: sharing a host across environments, and the edge box.

## One NATS: current shape

The chart's default is one NATS for scheduler and data plane, and it supports an external NATS (`nats.enabled: false`, `global.nats.schedulerUrl`, `global.nats.dataUrl`). wamn instead runs two, and hand-builds the binding the chart would declare.

| Off the chart's path today | Where | The chart's path |
| --- | --- | --- |
| Two NATS: the chart's bundled one (ephemeral storage, scheduler only) and wamn's `evt-nats` (durable JetStream) | `deploy/infra/values-wamn.yaml`, `deploy/gcp/nats-jetstream.yaml` | One NATS; scheduler and data URL default to the same server |
| The host's data-plane URL is ignored; events use `WAMN_EVT_NATS_URL` | `services/host/src/host.rs::HostArgs::data_nats_url`, `crates/platform/runtime/src/plugins/wamn_jetstream.rs` | `--data-nats-url` is the host's NATS for workload messaging |
| The materializer's `wasmcloud:nats` binding `events` comes from a host flag file | `services/host/src/host.rs::materializer_nats_bindings`, `--materializer-nats-binding-file` | Host group `plugins` entry `wasmcloud-nats` with `bindings`, grants and `secretFrom` |
| The auth memory bucket sits on the scheduler NATS | `crates/platform/runtime/src/auth_cache.rs`, `crates/identity/platform/src/auth_changes.rs` | No upstream equivalent; it belongs on the data plane |
| The chart generates its CA (365 days, never renewed) | `deploy/infra/values-wamn.yaml` | `global.certificates.generate: false` plus Secrets by the chart's names |
| Two auth schemes: scheduler mTLS with three shared users, `evt-nats` password users per (org, project, env) | `deploy/infra/values-wamn.yaml`, `crates/control/provision` | One server, one scheme |

Already on the path: materializer delivery and settlement use native `wasmcloud:nats` JetStream. `wamn:jetstream` holds only host-owned registration (`prepare`, `triggers`, `admit`) and duplicates nothing upstream.

Inference: wamn added `evt-nats` when the bundled NATS lacked durable JetStream. The repo records no reason.

## One NATS: rulings

Each ruling names the test that shows it holds.

| # | Ruling | Invariant |
| --- | --- | --- |
| R1 | The platform installs one NATS: durable JetStream (file storage), TLS on. The operator chart runs with `nats.enabled: false`. | The cluster has one NATS StatefulSet; the operator release renders no NATS object |
| R2 | `global.nats.schedulerUrl` and `global.nats.dataUrl` both name that NATS. | In a rendered host Deployment, `--scheduler-nats-url` equals `--data-nats-url` |
| R3 | cert-manager owns the CA and renews it. It issues the Secrets the chart names (NATS server, operator, runtime, data). The chart runs with `global.certificates.generate: false`. | The operator release renders no Secret; each of those Secrets has an owning cert-manager `Certificate` |
| R4 | Every client authenticates by a client certificate that the server maps to a NATS user (`verify_and_map`): the operator, the host scheduler and data connections, identity, `wamn-ctl`, the CDC reader, the provisioning Jobs, and each environment. No client uses a password. | A client with no certificate is refused; the authorization file has no `password` entry |
| R5 | Each environment has one client certificate: a cert-manager `Certificate` whose name maps to that environment's NATS user. The user publishes `evt.<org>.<project>.<env>.>`, reads its streams by `$JS.API.STREAM.INFO`, and reaches every consumer of its own event stream by wildcard: `$JS.API.CONSUMER.INFO.<stream>.*`, `$JS.API.CONSUMER.MSG.NEXT.<stream>.*` and `$JS.ACK.<stream>.>`. It holds no stream or consumer create, update or delete: only the provisioning client (wamn-ctl) holds them, as `capabilities.md` requires. The one exception is the filtered KV watch consumers on the environment's own prefix (R7). It adds `$KV.wamn-auth.<project_env_key>.>`, read-only watch on $KV.wamn-auth.=identity.> (PAT and session drops), $JS.API.STREAM.INFO.KV\_wamn-auth (bucket metadata), direct gets keyed by that prefix, and the consumer-create subject A7 verifies. It adds the same set for the connection memory bucket `wamn-connection` on `$KV.wamn-connection.<project_env_key>.>`, with no identity prefix. Each release sets `global.certificates.dataCertSecretName` to that Secret, so the host's data connection is the environment user. The binding leaves out `servers` and inherits `tls-*` from that connection. Host-owned code acting for that environment uses the same connection. The scheduler connection keeps the runtime certificate. | Environment A's certificate is refused on an environment B subject, stream, consumer and key, and on consumer create on its own event stream (workspace test with two environments) |
| R6 | The materializer's binding is declared in the release chart: host group `plugins` entry `wasmcloud-nats`, binding `events`, grants within R5's target, `workloadConfig: deny`. | `--materializer-nats-binding-file` does not exist; a render test makes sure of every grant is inside its environment's prefix |
| R7 | The auth memory bucket `wamn-auth` lives on the one NATS: memory storage, the same keys, the same 60 s bound, the same revision-guarded put. The connection memory bucket `wamn-connection` follows the same rules. Identity creates both buckets on start, and creates them again after a NATS restart. Hosts open them with a get. When a bucket is absent, a host caches nothing and reads Postgres, as [execution](../architecture/execution.md) specifies. The control host's user covers every environment of its org. Identity, wamn-ctl and the control host may publish deletes on $KV.wamn-auth.=identity.> and on the environment prefixes of their org. | `auth_cache` tests pass unchanged against the one-NATS fixture |
| R8 | Everything reaches NATS by the data URL. `WAMN_EVT_NATS_URL`, the `evt-nats` Service and the password authorization file are deleted. | No source or chart names `WAMN_EVT_NATS_URL` or `evt-nats` |
| R9 | The operator, its CRDs, the release chart's `WorkloadDeployment`s and the subchart host Deployment stay as they are. | `deploy/platform/release/templates/workloads.yaml` keeps its kinds; only `plugins` and NATS values change |
| R10 | A new or changed environment user reaches the server by an authorization reload, never a restart. A config reloader signals NATS when the authorization Secret changes; the StatefulSet carries no authorization digest annotation. | `platform apply` that adds an environment leaves the NATS pod's start time unchanged, and the operator logs no reconnect |

## One NATS: changes

**NATS and certificates** (`deploy/infra`, `crates/control/lib/src/platform`)

- One NATS manifest: today's `nats-jetstream.yaml` gains TLS, client-certificate users for platform clients, and the per-environment credentials of R5.
- A cert-manager self-signed root, a CA `Issuer`, and `Certificate`s for the four chart Secrets in the namespaces the chart reads them from.
- `values-wamn.yaml`: `nats.enabled: false`, `global.nats.schedulerUrl` and `dataUrl`, `global.certificates.generate: false`. The CA-expiry comment and recreate procedure go.
- `platform/install.rs`, `platform/steps.rs`: install order is cert-manager CA, NATS, operator release.

**Host** (`services/host`, `crates/platform/runtime`)

- Delete `--materializer-nats-binding-file` and `materializer_nats_bindings`. The binding arrives from the chart `plugins` entry.
- `plugins/wamn_jetstream.rs`: connect by the data URL with the environment credential.
- `auth_cache.rs`: watch and put on the one NATS with the environment credential.
- Fix the `HostArgs::data_nats_url` comment: it is now the event plane.

**Release chart** (`deploy/platform/release`)

- Host group `plugins`: the `wasmcloud-nats` entry, binding `events`, grants from the environment, no `servers`, so it inherits the data connection's `tls-*`.
- `global.certificates.dataCertSecretName`: the environment's Certificate Secret.
- No change to `workloads.yaml` kinds.

**Other clients**

- `crates/identity/platform/src/auth_changes.rs`: the writer connects with its client certificate.
- `crates/control/lib/src/platform/cdc.rs`, `services/ctl`: the CDC reader and the verbs use the data URL and client certificates.
- `crates/control/provision` and `crates/control/lib/src/platform/broker.rs`: certificate users with today's permission set, one cert-manager `Certificate` per environment, and a config reloader in place of the StatefulSet digest annotation (R10).
- `crates/control/lib/src/dev`, `services/ctl/src/dev/up.rs`, test support and the bench script: one NATS container with TLS.

**Deleted**

- `deploy/infra/wasmcloud-ca-issuer.yaml` (replaced by the cert-manager CA), `deploy/gcp/auth-nats-clients.yaml`, `deploy/platform/host-environment-certs.example.yaml`, the `evt-nats-authorization` Secret and its generator.
- `deploy/gcp/flow-http.yaml`, `deploy/gcp/materializer.yaml`, `deploy/gcp/wms-materializer.yaml` (unreferenced today).
- The architecture and operations sentences that keep the event broker apart from the scheduler broker.

## One NATS: assumptions

The first issue makes sure of every assumption and stops on a false one.

| # | Assumption | How to verify | If false |
| --- | --- | --- | --- |
| A1 | The operator chart at 2.11.0 renders and runs with `nats.enabled: false`, `global.certificates.generate: false` and TLS on, given Secrets by its names. | `helm template` with those values, then `platform_apply_kind` | Keep the chart's NATS for scheduler only, and use `dataUrl` for everything else |
| A2 | `verify_and_map` maps each environment certificate to its own user, and a reload adds a user without dropping connected clients. | A workspace test: two environment certificates, then a third user added by reload while the first two stay connected | Stop and report: per-environment accounts are the next fork |
| A3 | The `wasmcloud:nats` `plugins` binding keys (`servers`, `subject-allow`, `stream-allow`, `tls-*`) cover what the binding file carries today. | Compare the materializer's binding file with the chart's documented keys | The release chart's host group passes the missing keys through `config` |
| A4 | NATS permissions can confine the operator and runtime users to wasmCloud's control subjects, and keep them off `evt.>` and `$KV.wamn-auth.>`. | Subscribe as the runtime user in a workspace test | The runtime user keeps today's reach, which is the current boundary |
| A5 | JetStream file streams and the memory-storage KV bucket share one server without loss of either guarantee. | `auth_cache` and materializer tests against the one-NATS fixture | Two JetStream domains on one server |
| A6 | One NATS outage now stops placement, events and the auth memory together. Each already survives its own outage. | A NATS restart during `environment_apply_kind` | Accepted: the owner ruled the scheduler outage window, and hosts read Postgres when the auth memory is gone |
| A7 | A KV watch can be confined to `$KV.wamn-auth.<project_env_key>.>`, because the filtered consumer-create subject carries the filter. | A workspace test: environment A watches its prefix and is refused a watch on B's | Stop and report |

## Kept as is

These review points change nothing. Each has a stated cause.

| Point | Kept | Cause |
| --- | --- | --- |
| Host per environment | One release, one host Deployment, operator placement | At 1 to 3 applications per environment, a shared host saves at most one pod per org and mixes environments the rulings keep isolated |
| `poolSize: 0` on the composed entry | Fresh store per call | Upstream's default; the entry holds authored code; measured instantiation is about 11% of host CPU on a get. Warm reuse stays a per-digest owner choice after the post-A5 measurement |
| CLI orchestration and install Jobs | `env apply` in process, `platform apply` Jobs | Install and upgrade only; upstream has no mechanism for migrations, streams or credentials |
| runtime-gateway | Not used | Deprecated upstream; wamn already uses operator EndpointSlices behind a Service |
| `wamn:postgres` | Host-owned transactions | `wasmcloud-postgres` takes a new connection per statement and has no transaction API |
| wash-runtime dependencies on the edge | Accepted | NATS, Redis and OTLP gRPC are not optional upstream; the binary is under its ceiling |

## Build cost and tests

Only One NATS forces a platform build, once, at its close. The other epics ride that build or the next one.

| Epic | Images that change | Forces its own build |
| --- | --- | --- |
| One NATS | host, identity, ctl, CDC reader (connection config) | yes, once at close |

- Per issue: build, clippy, unit tests. Per merge: the live targets that touch the merged files.
- One NATS close: `platform_apply_kind` and `environment_apply_kind` once each, then the wamn-dev cutover (`platform apply`, then `env apply` per environment; streams are re-created, no event data carries over).

## Issues

One epic is targeted at a time. The next is targeted after the previous closes and is reviewed. Order: Deploy failures, Edge and CDN, WASI version match, One NATS. One NATS closes with the one platform build, which covers all four.

**One NATS** (targeted, epic `wamn-j1r05`)

1. A1 to A7: test each assumption with kind and workspace tests, and record the result (`wamn-j1r05.1`).
2. R1 to R4: one NATS with TLS, the cert-manager CA and the chart Secrets, certificate users, and the operator values (`wamn-j1r05.2`).
3. R5, R7, R8, R10: environment certificates, the auth memory and the clients on the data URL, the config reloader, and the `evt-nats` deletions (`wamn-j1r05.3`).
4. R6, R9: the materializer binding from the release chart `plugins` entry (`wamn-j1r05.4`).
5. The deleted files, the docs, and convergence of an existing install under `platform apply` (`wamn-j1r05.5`).
