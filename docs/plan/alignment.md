# wasmCloud alignment

@D Kloimwieder

## Goal

wamn follows the wasmCloud runtime-operator path unless it has a stated cause not to. Four changes come out of the architecture review; the last section lists what stays as is and why.

- **One NATS**: the operator, host scheduler and data plane reach one durable NATS; events and the auth memory ride it; per-environment credentials; certificates from a cert-manager CA.
- **Edge and CDN**: an HTTPS load balancer with a Google-managed certificate and Cloud CDN in front of the edge on GCP; nginx reuses upstream connections.
- **WASI version match**: admission matches WASI imports by package and `0.2` family, as the host linker does.
- **Deploy failures**: an install Job that times out reports the host's own error.
- Out of scope: sharing a host across environments, and the edge box.

## One NATS: current shape

The chart's default is one NATS for scheduler and data plane, and it supports an external NATS (`nats.enabled: false`, `global.nats.schedulerUrl`, `global.nats.dataUrl`). wamn instead runs two, and hand-builds the binding the chart would declare.

| Off the chart's path today | Where | The chart's path |
| --- | --- | --- |
| Two NATS: the chart's bundled one (ephemeral storage, scheduler only) and wamn's `evt-nats` (durable JetStream) | `deploy/infra/values-wamn.yaml`, `deploy/infra/nats-jetstream.yaml` | One NATS; scheduler and data URL default to the same server |
| The host's data-plane URL is ignored; events use `WAMN_EVT_NATS_URL` | `services/host/src/host.rs::HostArgs::data_nats_url`, `crates/platform/runtime/src/plugins/wamn_jetstream.rs` | `--data-nats-url` is the host's NATS for workload messaging |
| The materializer's `wasmcloud:nats` binding `events` comes from a host flag file | `services/host/src/host.rs::materializer_nats_bindings`, `--materializer-nats-binding-file` | Host group `plugins` entry `wasmcloud-nats` with `bindings`, grants and `secretFrom` |
| The auth memory bucket sits on the scheduler NATS | `crates/platform/runtime/src/auth_cache.rs`, `crates/identity/platform/src/auth_changes.rs` | No upstream equivalent; it belongs on the data plane |
| The chart generates its CA (365 days, never renewed) | `deploy/infra/values-wamn.yaml` | `global.certificates.generate: false` plus Secrets by the chart's names |
| Two auth schemes: scheduler mTLS with three shared users, `evt-nats` password users per (org, project, env) | `deploy/infra/values-wamn.yaml`, `crates/control/provision` | One server, one scheme |

Already on the path: materializer delivery and settlement use native `wasmcloud:nats` JetStream. `wamn:jetstream` holds only host-owned registration (`prepare`, `triggers`, `admit`) and duplicates nothing upstream.

Inference: wamn added `evt-nats` when the bundled NATS lacked durable JetStream. The repo records no reason.

## One NATS: rulings

Each ruling names the check that shows it holds.

| # | Ruling | Invariant |
| --- | --- | --- |
| R1 | The platform installs one NATS: durable JetStream (file storage), TLS on. The operator chart runs with `nats.enabled: false`. | The cluster has one NATS StatefulSet; the operator release renders no NATS object |
| R2 | `global.nats.schedulerUrl` and `global.nats.dataUrl` both name that NATS. | In a rendered host Deployment, `--scheduler-nats-url` equals `--data-nats-url` |
| R3 | cert-manager owns the CA and renews it. It issues the Secrets the chart names (NATS server, operator, runtime, data). The chart runs with `global.certificates.generate: false`. | The operator release renders no Secret; each of those Secrets has an owning cert-manager `Certificate` |
| R4 | Every client authenticates by a client certificate that the server maps to a NATS user (`verify_and_map`): the operator, the host scheduler and data connections, identity, `wamn-ctl`, the CDC reader, the provisioning Jobs, and each environment. No client uses a password. | A client with no certificate is refused; the authorization file has no `password` entry |
| R5 | Each environment has one client certificate: a cert-manager `Certificate` whose name maps to that environment's NATS user. The user keeps the permission set `crates/control/lib/src/platform/broker.rs` builds today: `evt.<org>.<project>.<env>.>`, and `$JS.API.STREAM.*`, `$JS.API.CONSUMER.*` and `$JS.ACK.*` on that environment's length-prefixed stream and consumer names. It adds `$KV.wamn-auth.<env_key>.>`, read-only watch on $KV.wamn-auth.=identity.> (PAT and session drops), $JS.API.STREAM.INFO.KV\_wamn-auth (bucket metadata), direct gets keyed by that prefix, and the consumer-create subject A7 verifies. Workloads receive the certificate through the binding's `tls-*` keys from `secretFrom`. Host-owned code acting for that environment uses the same certificate. | Environment A's certificate is refused on an environment B subject, stream, consumer and key (workspace test with two environments) |
| R6 | The materializer's binding is declared in the release chart: host group `plugins` entry `wasmcloud-nats`, binding `events`, grants within R5's scope, `workloadConfig: deny`. | `--materializer-nats-binding-file` does not exist; a render test checks every grant is inside its environment's prefix |
| R7 | The auth memory bucket `wamn-auth` lives on the one NATS: memory storage, the same keys, the same 60 s bound, the same revision-guarded put. The control host's user covers every environment of its org. Identity, wamn-ctl and the control host may publish deletes on $KV.wamn-auth.=identity.> and on the environment prefixes of their org. | `auth_cache` tests pass unchanged against the one-NATS fixture |
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

- Host group `plugins`: the `wasmcloud-nats` entry, binding `events`, grants from the environment, `secretFrom` the environment credential Secret.
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

The first issue checks every assumption and stops on a false one.

| # | Assumption | How to verify | If false |
| --- | --- | --- | --- |
| A1 | The operator chart at 2.11.0 renders and runs with `nats.enabled: false`, `global.certificates.generate: false` and TLS on, given Secrets by its names. | `helm template` with those values, then `platform_apply_kind` | Keep the chart's NATS for scheduler only, and use `dataUrl` for everything else |
| A2 | `verify_and_map` maps each environment certificate to its own user, and a reload adds a user without dropping connected clients. | A workspace test: two environment certificates, then a third user added by reload while the first two stay connected | Stop and report: per-environment accounts are the next fork |
| A3 | The `wasmcloud:nats` `plugins` binding keys (`servers`, `subject-allow`, `stream-allow`, `tls-*`) cover what the binding file carries today. | Compare the materializer's binding file with the chart's documented keys | The release chart's host group passes the missing keys through `config` |
| A4 | NATS permissions can confine the operator and runtime users to wasmCloud's control subjects, and keep them off `evt.>` and `$KV.wamn-auth.>`. | Subscribe as the runtime user in a workspace test | The runtime user keeps today's reach, which is the current boundary |
| A5 | JetStream file streams and the memory-storage KV bucket share one server without loss of either guarantee. | `auth_cache` and materializer tests against the one-NATS fixture | Two JetStream domains on one server |
| A6 | One NATS outage now stops placement, events and the auth memory together. Each already survives its own outage. | A NATS restart during `environment_apply_kind` | Accepted: the owner ruled the scheduler outage window, and hosts read Postgres when the auth memory is gone |
| A7 | A KV watch can be confined to `$KV.wamn-auth.<env_key>.>`, because the filtered consumer-create subject carries the filter. | A workspace test: environment A watches its prefix and is refused a watch on B's | Stop and report |

## Edge and CDN

On GCP an HTTPS load balancer with Cloud CDN sits in front of the same edge; `wamn dev`, the local bench and kind do not change. Today `crates/schema/generator/src/tofu.rs` renders a `google_compute_target_tcp_proxy`, and `deploy/platform/edge/templates/configmap.yaml` opens a new upstream connection per request.

| # | Ruling | Invariant |
| --- | --- | --- |
| C1 | GCP ingress is a global HTTPS load balancer on the existing global address: a Certificate Manager certificate map holding one Google-managed certificate for wamn.dev and \*.wamn.dev, issued by DNS authorization, a URL map whose default service is the backend, no host or path rules; one backend service on the edge NEG. | `tofu.rs` render test: a target HTTPS proxy, a URL map whose default service is the backend service and that has no host or path rules, a certificate map whose certificate covers wamn.dev and \*.wamn.dev through a DNS authorization; no compute SSL certificate; no target TCP proxy |
| C2 | The backend service has Cloud CDN on, cache mode `USE_ORIGIN_HEADERS`, backend protocol HTTPS. | Render test reads `enable_cdn`, the cache mode and the protocol |
| C3 | The origin decides what caches. Digest paths stay `public, max-age=31536000, immutable`; `config.json` and pages stay `no-cache`; `/api` keeps the route's `cache-control`, so authenticated reads stay `private`. Anonymous `/api` reads are not cached at the CDN either, because every read sends `Vary: Authorization, Cookie` (`apps/platform/ingress/http-route/src/lib.rs`). No change: public API caching is not a goal of this epic. | On wamn-dev, a second fetch of a digest path carries an `age` header; no `/api` GET does |
| C4 | nginx keeps its own TLS in kind and on GCP; the load balancer re-encrypts to it. The edge chart has one shape. | The edge chart renders the same server blocks with `neg: true` and `neg: false`, apart from the NEG annotation |
| C5 | nginx reuses upstream connections: `worker_processes auto;`, `keepalive 32;` in each `upstream api_N`, `proxy_set_header Connection "";` in `location /api/`. | `tests/system/tests/deployment_declarations.rs` pins all three lines with these exact values |

**Changes**

- `tofu.rs`: replace the TCP proxy with a target HTTPS proxy, a URL map, a DNS authorization, a Google-managed certificate and its certificate map, and a CDN-enabled HTTPS backend service. Keep the global address, forwarding rule and NEG data source.
- `deploy/platform/edge/templates/configmap.yaml`: C5.
- `docs/operations/gcp.md` 4.20 and the `deployment.md` "Google Cloud edge" section describe the new path. The stale "backend bucket" text and `bucket.endpoint` go.
- The DNS authorization needs one CNAME record in the wamn.dev zone, made once. It is an owner input: the agent posts the command and runs it after approval. New orgs and environments under wamn.dev need no certificate change; only their A record.
- wamn-dev cutover: `tofu apply` under manual approval. The certificate goes active before the forwarding rule moves to the HTTPS proxy, so the route hosts see no gap.

## WASI version match

Admission matches WASI imports the way the host linker does: by package and family, not by exact patch. Today `crates/platform/component-policy/src/lib.rs` (`CAPABILITY_REGISTRY`) admits only `0.2.12`, so every Rust toolchain or WASI-Virt bump needs a registry edit.

| # | Ruling | Invariant |
| --- | --- | --- |
| W1 | Registry rows stay package-grain (`wasi:io`, `wasi:clocks`, …). A `wasi:*` row names the family `0.2`: any `0.2.N` import of that package is admitted up to W3's ceiling. Rows for `wasi:logging@0.1.0-draft` and every `wamn:*` package keep their exact versions. | Policy test admits `wasi:io@0.2.9`, `@0.2.12`. Refuses an unlisted package, an unlisted family, and `wamn:integration/bound@0.1.1` |
| W2 | The WASI-Virt source revision and adapter digest stay exact. | `tests/conformance/tests/capability_registry.rs` still fails on a changed adapter digest |
| W3 | The ceiling is the highest `0.2` patch the host's Wasmtime links, derived at build from the linked `wasmtime-wasi` WIT, never typed by hand. An import above it is refused at admission. | Policy test refuses `0.2.(ceiling + 1)`; a Wasmtime bump moves the ceiling with no source edit |

**Changes**

- `crates/platform/component-policy/src/lib.rs` (`CAPABILITY_REGISTRY`, `connector_import`): family and ceiling matching for `wasi:*` rows; the exact-match test becomes W1's test.
- The build derives the ceiling for W3.
- `tests/conformance/tests/capability_registry.rs`: rows carry families; the adapter-digest check stays.
- The capability pages say "family" where they say an exact version.

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
| Edge and CDN | none (chart, OpenTofu renderer in ctl) | no |
| WASI version match | ctl and host (policy crate) | no |

- Per issue: build, clippy, unit tests. Per merge: the live targets that touch the merged files.
- One NATS close: `platform_apply_kind` and `environment_apply_kind` once each, then the wamn-dev cutover (`platform apply`, then `env apply` per environment; streams are re-created, no event data carries over).
- Edge and CDN close: the wamn-dev `tofu apply` under manual approval, then C3's two fetches.

## Issues

One epic is scoped at a time. The next is scoped after the previous closes and is reviewed. Order: Deploy failures, Edge and CDN, WASI version match, One NATS. One NATS closes with the one platform build, which covers all four.

**Edge and CDN** (scoped, epic `wamn-wio2s`)

1. C5: nginx upstream keepalive in `deploy/platform/edge/templates/configmap.yaml`, pinned by `deployment_declarations`.
2. C1 to C4: the HTTPS load balancer, managed certificate and CDN in `tofu.rs`, the edge chart test for `neg`, and the docs.
3. The wamn-dev cutover from the hand-built load balancer, and C3's two fetches.

**WASI version match**, **One NATS**: scoped later.
