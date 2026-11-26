# Epic 4 scoping report

Scope: Epic 4 in `docs/plan/review-restructure.md` ("Org boundary on kind"). It covers T3, T9, T12, the kind pin and refusal tests of T13, and the T16 bullet "A failed signing-key fetch answers 401 (Epic 4)" with its `authentication_event` row. The checkout was read at commit `faa69d1fa`. This report changes no code.

F means read in the code or in the vendored upstream chart at that commit. I means inference or proposal. Five read-only agents wrote the inventories in the appendices. Root checked the facts that the beads and questions below depend on. The appendices give every `path:line`:

- `appendix-a-pod-security.md`: namespaces, every pod spec and its security context, image users, and low ports (T3).
- `appendix-b-certificates-csrf.md`: every generated key and CA, the CA consumers, trust-manager, and the CSRF parts (T9).
- `appendix-c-network-kind.md`: every NetworkPolicy, the `platform` callers, `hosts` egress, the kind pin, and the refusal test (T12, T13).
- `appendix-d-signing-key.md`: the verifier error classes, the callers and their answers, event stores, and tests (T16).
- `appendix-e-images-docs.md`: Docker targets and their crates, the rebuild of each change, Part 1 edits, and the journey targets.

Environments still share the org namespace until Epic 5 (T1). So in Epic 4 the enforced namespaces are `platform`, `hosts` and `<org>`.

## 1. Facts in the plan that the code contradicts

- T3 says that the cert-manager and CloudNativePG operator namespaces belong to their own charts. The wamn infrastructure chart creates the `cert-manager` namespace (`deploy/charts/infrastructure/templates/namespaces.yaml:1`) (F). The CloudNativePG controller and its webhook run in `platform` (`deploy/charts/infrastructure/values.yaml:6`, appendix a) (F). So `restricted` on `platform` covers the CloudNativePG controller. Its rendered pod already meets `restricted` (F, offline render).
- The order-of-work table names "the non-root `USER` in the `Dockerfile`" as the reason for the host image rebuild. The stock host pod already runs as UID 65532 with `runAsNonRoot: true` and seccomp `RuntimeDefault` (`runtime-operator/values.yaml:399`) (F). The real reasons for the rebuild are in section 4.
- Part 1 accepts an unsafe request with a session cookie when both `Sec-Fetch-Site` and `Origin` are absent (`docs/plan/wamn-restructure.md:143`) (F). T9 refuses it.
- T9 says the change rebuilds only the web proxy and the web runtime. The `ingress` guest also reads `x-wamn-csrf` for its cache decision (`apps/guests/ingress/src/lib.rs:263`) (F). The `ctl` and `control` images embed that guest (F). The CA move changes `wamn-platform-install`, which the CLI, control and identity link (F).
- Section 8 asks for a kind run with `restricted` to confirm the CloudNativePG pods, including the backup sidecar. T7 keeps backups off on kind (`docs/plan/review-restructure.md:480`) (F). So the kind run cannot admit the backup sidecar.
- T13 asks for a new cross-org refusal test. The existing `isolation::envs_in_two_orgs_are_isolated` already probes a host pod and a Postgres primary in another org, with positive controls (`tests/journeys/tests/isolation.rs:74`) (F).

## 2. Proposed beads

| Bead | Content | Depends on |
| --- | --- | --- |
| E4.1 | Kind pin (T13). Pin kind 0.33.0 with its checksum in `mise.toml`, set `kindest/node:v1.36.4` by digest in `deploy/kind/kind-config.yaml`, and name kind 0.33 in `docs/operations/deployment.md`. | none |
| E4.2 | Control listens on 8080 (T3). Change `services/control/src/lib.rs::HTTP_PORT`, the container port in `deploy/charts/org/templates/control.yaml`, the three port-80 policy rules (`network-policies.yaml:33`, `provision-org.yaml:81`, `:96`) and the port-forward in `deployment.md:157`. The Service keeps port 80. | none |
| E4.3 | Security contexts on wamn pods (T3). Control, the CDC reader, identity, the web proxy and the system SQL Job in the charts. The Job builders `crates/kubernetes/src/job.rs::manifest` and `crates/platform-install/src/org_install.rs::org_job`. The journey probe Jobs in `isolation.rs` and `lifecycle.rs`. | E4.2 (shares `control.yaml`) |
| E4.4 | Security contexts on upstream pods through exposed values (T3). NATS with its reloader and init container, and the OpenTelemetry collector. Settle the journey capture `hostPath` (`xtask/src/journey_cluster.rs:229`). | Question 9 |
| E4.5 | Enforce (T3). Put the `enforce` and `warn` labels on `hosts`, `<org>` and `platform`. Make the install path own the `platform` labels (`crates/platform-install/src/platform_install.rs`). Extend `xtask` preflight to dry-run pod templates under `restricted`. Add the goal-2 case: a server-side dry run of a privileged pod is refused. | E4.3, E4.4 |
| E4.6 | CAs from cert-manager, public roots by trust-manager (T9). The `genCA` and `lookup` blocks go from `identity-certificate.yaml` and `system-database.yaml`. Add trust-manager to the infrastructure chart. Remove the CA values from the org values (`crates/platform-install/src/{org,org_install}.rs`). | Question 6 |
| E4.7 | The `Sec-Fetch-Site` check replaces the CSRF cookie (T9). Change `services/web/src/browser_session.rs::forward`. The key Secret, the cookie, the header, `web/runtime/src/csrf.ts` and the `ingress` cache branch go. Amend Part 1. | Questions 4 and 5 |
| E4.8 | Network policies (T12). One default-deny ingress policy and one allow per real caller in `platform`. One egress policy for the web proxy in `hosts`. Turn off the stock runtime-operator policy, which allows any source, through `runtime-operator.networkPolicy.enabled`. | E4.2 (control port) |
| E4.9 | Signing-key fetch failure (T16). The host answers 503 `authentication-unavailable`, logs a warning and writes an `authentication_event` row. An invalid token stays 401. | Questions 1 to 3 |
| E4.10 | Org boundary checks on kind (T13, section 5 goal 1). Select `isolation::envs_in_two_orgs_are_isolated` with the smoke run. Add one case that runs `kubectl auth can-i` for each org service account against the other org. | E4.1, E4.8, Question 11 |
| E4.11 | Epic close. One host image build, then the kind journeys on a fresh pinned cluster with `restricted` enforced, after your go. Update Part 1 and the operations pages. | all |

E4.1, E4.2, E4.4, E4.6, E4.7 and E4.9 own separate files, so they run in parallel. E4.3 and E4.8 start after E4.2. E4.5 starts after E4.3 and E4.4. Root resolves the overlap in `deploy/charts/platform/templates/edge-deployment.yaml` (E4.3, E4.7) at merge.

## 3. Changes

### Schema

| Change | File and symbol | Mark |
| --- | --- | --- |
| New table `authentication_event` (time, org, env, project, outcome, reason). No such table exists today. | `deploy/sql/app-schema.sql` if it is a project table (Question 1). Grants in `crates/env/src/project_setup.rs`. | I (no table: F) |

No other Epic 4 item changes SQL (F, all five appendices).

### Wire

| Change | File and symbol | Mark |
| --- | --- | --- |
| A failed key fetch on a host session route answers 503 `authentication-unavailable`, not 401 `unauthorized`. Control already does this. The OpenAPI declares only 200, so no generated client changes. | `crates/authentication/src/route_authentication.rs::authenticate_session`, reusing `crates/app/src/ingress.rs::authentication_unavailable`. Control: `route_authentication.rs::authenticate_control`. | F today, I change |
| The cookie `__Host-wamn-web-csrf` and the header `x-wamn-csrf` go. | `services/web/src/browser_session.rs::{set_cookies,valid_csrf,respond}`, `web/runtime/src/csrf.ts::readCsrf`, `session.ts::post`, `transport.ts::createTransport` | F today, I change |
| An unsafe session-cookie request with `Sec-Fetch-Site` `same-site` or `cross-site` is refused. Without that header, `Origin` must match `Host`. With neither header, the request is refused (Question 4). | `services/web/src/browser_session.rs::forward` | I |
| The `ingress` guest no longer reads `x-wamn-csrf` for its cache decision. | `apps/guests/ingress/src/lib.rs:263` | F today, I change |
| The org chart values lose `identityCa`, `postgresCa` and, with Question 6, `natsCa`. | `deploy/charts/org/values.yaml:17`, `crates/platform-install/src/org.rs:141`, `:151` | I |
| The control pod listens on 8080. The Service stays on port 80, so the route URL and the first-owner Job URL do not change. | `services/control/src/lib.rs::HTTP_PORT`, `services/control/src/first_owner.rs:72` | F today, I change |

### Behaviour

| Change | File and symbol | Mark |
| --- | --- | --- |
| `restricted` enforce and warn labels on `hosts`, `<org>` and `platform`. | `deploy/charts/platform/templates/namespaces.yaml`, `deploy/charts/org/templates/namespace.yaml`, `crates/platform-install/src/platform_install.rs` | I |
| Pod and container contexts on five chart pods and two Rust Job builders. Today none of them sets a context. | appendix a, "Wamn-owned templates" | F today, I change |
| NATS, its reloader and its init container, and the collector get contexts through exposed values. The runtime operator, the host, CloudNativePG, Barman, Traefik and cert-manager already pass. | `deploy/charts/platform/values.yaml:85`, `:150`, `:166` | F today, I change |
| Journey capture mounts a `hostPath` in `platform`, which `restricted` refuses. | `xtask/src/journey_cluster.rs:229`, `deploy/charts/platform/values-kind.yaml:9` | F |
| cert-manager issues the identity CA and the system database CA. trust-manager copies the public roots by namespace label. trust-manager is a new dependency. | `deploy/charts/infrastructure/Chart.yaml` | I (absent today: F) |
| The CSRF key Secret and its mount go. | `deploy/charts/platform/templates/edge-csrf-secret.yaml`, `edge-deployment.yaml:43` | F today, I change |
| Platform default-deny ingress with allows for NATS, the system database, the two Poolers, identity, the collector, the CloudNativePG webhook, Barman and kind Traefik. Web proxy egress to hosts, control, identity, DNS and the API server. | appendix c, "T12 `platform` ingress" | I |
| The host logs a warning on a failed key fetch and writes the event row. The 503 does not depend on the insert. | `crates/authentication/src/route_authentication.rs` | I |
| Kind and node image pinned. | `mise.toml`, `deploy/kind/kind-config.yaml` | I |

## 4. The host image rebuild

Three changes touch crates in the host image (F: `services/host/Cargo.toml:21`, `:35`):

- E4.9 changes `wamn-authentication`, and possibly `wamn-http` and `wamn-postgres`.
- E4.3 changes `crates/kubernetes/src/job.rs::manifest`, which is in `wamn-kubernetes`.
- A `USER` line on the `host` target, if Question 8 asks for one.

Each of them forces a host build. To keep to one build, land all three before E4.11 and run no kind run before E4.11. Each bead runs its unit tests, its live tests and offline chart renders. The build at E4.11 then builds all five targets (`host`, `control`, `ctl`, `identity`, `edge-web`) once, and the journeys use that one bundle. A rerun after a test-only fix reuses the bundle, as in R6.8.

## 5. Tests at epic close

The kind run uses all 14 journey targets (32 cases), because the Epic 4 diff touches `deploy/`, which the selection maps to every package (I, appendix e). New cases:

- Goal 2: a privileged pod is refused by a server-side dry run (E4.5).
- The cross-org host and Postgres refusal, by selecting the existing `isolation` case (E4.10).
- Goal 1: no org service account can act in another org (E4.10).
- Web proxy unit cases: `cross-site` refused, `same-origin` passes, neither header refused, bearer-only passes (E4.7).
- Host unit case: a failing JWKS endpoint answers 503 and logs, an invalid token answers 401, a missing `kid` after a good fetch answers 401 (E4.9).
- Live case: the platform login inserts the event row, and the app and query logins cannot read it (E4.9).
- The `restricted` CloudNativePG run with a two-instance project, so that a replica join runs (E4.11).

## 6. Questions for rulings

1. Where the `authentication_event` row lives. (a) A project table, written by the host's existing platform login. (b) A system table, written through a new insert-only host login with a new grant and network path. (c) A log line only, with a plan edit. Recommendation: (a). The host has no route to the system database today (F).
2. How the table reaches databases. (a) Bootstrap SQL only, for fresh installs, as in R6. (b) A numbered project migration. Recommendation: (a). R6 found no install that holds durable data.
3. Retention and throttled refreshes. (a) No pruning now, and the table joins the T16 history-pruning bullet. (b) Deletion in control's maintenance loop. Recommendation: (a). Separately, the refresh limiter can refuse a request before any fetch. Recommendation: answer 503 for it, but write a row only for an actual failed fetch.
4. Both `Sec-Fetch-Site` and `Origin` absent. (a) Refuse, as T9 says, and amend Part 1 line 143. (b) Accept, as Part 1 says. Recommendation: (a).
5. The browser read cache uses the readable CSRF cookie to detect a session change (`web/runtime/src/transport.ts:476`). (a) Turn off read reuse in cookie mode. (b) Reset the cache from the session keeper, which misses a change in another tab. (c) Add a new session-change signal. Recommendation: (a).
6. trust-manager. (a) Add the pinned stock trust-manager chart to the infrastructure chart, as T9 says. Move the NATS CA with the other two roots. (b) Keep the installer copy of the public roots, but issue them with cert-manager. Recommendation: (a), with the trust-manager version that supports cert-manager 1.21.
7. The CloudNativePG controller. (a) Keep it in `platform` under `restricted` and correct T3. (b) Move it to its own namespace. Recommendation: (a). Its pod already passes, and a move changes the webhook and the T12 rules.
8. Image users. (a) Pod specs set `runAsUser` everywhere, and no `USER` is added to the Dockerfile. Correct the order-of-work row. (b) Also add a numeric `USER` to all five targets. Recommendation: (a). The pod spec is what admission checks.
9. Journey telemetry capture under `restricted`. (a) Turn off the `hostPath` file export and collect pod logs. (b) Keep `otel.jsonl` through an allowed volume and copy it out. Recommendation: (a). T16 records that hosts send no traces to the kind collector.
10. Web proxy egress outside kind. (a) Enforce it on kind only in Epic 4. (b) Write a provider-aware policy now, with the GCP storage path. Recommendation: (a). Epic 7 owns GCP.
11. The goal-1 RBAC check. (a) Add it in E4.10 as one kind case that derives the accounts from the cluster. (b) Leave it for later, because section 8 does not name it. Recommendation: (a).
12. The backup sidecar. (a) Confirm it on GKE in Epic 7, because kind runs no backups. (b) Turn on a backup on kind for this one check. Recommendation: (a).
13. The kind pin needs the kind 0.33.0 checksum and the node image digest from the upstream release pages. (a) Root reads them once and cites the source in the commit. (b) You supply them. Recommendation: (a).
14. The per-org node pool in T3. (a) Build it when an org first needs it. (b) Add an org setting now. Recommendation: (a). The plan does not say which pods it covers.
