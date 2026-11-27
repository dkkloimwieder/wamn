# Epic 6 scoping report

Scope: Epic 6 in `docs/plan/review-restructure.md` ("Public addresses: T11, and client base paths"). It covers T11, findings row 10, and the "client base paths" item of the order of work. The code was read at `03996f014`, the head of `main` after the Epic 5 merge. This report changes no code.

F means read in the code or in a chart at that commit. I means inference or proposal. Four read-only agents wrote the inventories in the appendices, and each appendix gives every `path:line`. Root checked the facts that the corrections, beads and questions depend on: the T11 symbol names, the second route producer, the per-request ConfigMap list, the edge rights, the Namespace labels, the missing prod field, the application name rule, the `routeHost` binding, the host crate dependencies, the session cookie and the kind Ingress.

- `appendix-a-route-table.md`: how web finds routes today, the Service and Namespace watch, the route annotations, who writes them, the hosts egress policy, and the readers and test targets.
- `appendix-b-address-prod.md`: every builder and reader of an application address, the label removal, the prod marker, and the journey readers.
- `appendix-c-client-paths.md`: every client that builds a platform or application URL, the fixed root paths, the session cookie and the `Sec-Fetch-Site` check under the new hosts.
- `appendix-d-certificates-tests.md`: public TLS on kind and GCP, `wamn org create`, how journeys reach app hosts, the default application setting, and the images.

## 1. Corrections to the plan

All are F.

1. T11 names stale paths and symbols. The template is `deploy/charts/app/templates/web-release.yaml`. The host helper is `crates/registry/src/name.rs::app_host`. The limit is `crates/registry/src/identifiers.rs::MAX_APP_LABEL_LEN`, applied by `app_label_reason`. The chart input is `ValuesInput::route_label` in `crates/env/src/app_chart/plan.rs`.
2. T11 names one route producer. The org chart writes a second ConfigMap pair for org control (`deploy/charts/org/templates/control-route.yaml`). Web loses its ConfigMap right only when both producers move.
3. Findings row 10 says that web lists every ConfigMap on every request. It lists the ConfigMaps labeled `wamn.route=true` in every namespace (`crates/kubernetes/src/web_release.rs::Releases::routes`). It does this before every request, `/healthz` included (`services/web/src/lib.rs:276`). Static requests also read the release ConfigMap by name.
4. T11 says that the Namespace watch shows the proxy nothing new. Since Epic 5, the watch supplies the org from `wamn.dev/org-name` and tells control from workload namespaces by `wamn.dev/org-workloads` (`crates/platform-install/src/env_install/manifest.rs:64`). A prod label is a further fact that no Service can supply.
5. T11 says that names may not contain two hyphens in a row. That rule covers org, env and project slugs only. `AppName` accepts any nonempty text without a NUL byte (`crates/registry/src/env_app.rs:36`). After the label goes, the application path segment has no syntax rule.
6. The registry has no prod field. `registry.env` has `active` and `disposable` only (`deploy/sql/system-schema.sql:579`).
7. The order of work says that Epic 6 needs no host image rebuild. The host links `wamn-routes`, `wamn-kubernetes` and `wamn-registry` (`services/host/Cargo.toml`), and the T11 deletions touch all three. The app workload binds the public host as `routeHost` in its HTTP handler (`deploy/charts/app/templates/workloads.yaml:36`), and the host registers its file and flow routes under that host (`services/host/src/host/platform_http.rs::bind`). The web proxy is its own `edge-web` image, so T11 also rebuilds it.
8. T2 says that the only cluster-wide rights are token review and the web proxy ConfigMap read. The edge ClusterRole also grants Namespace list and watch (`deploy/charts/platform/templates/edge-rbac.yaml:16`).
9. On kind, one certificate `*.<platformDomain>` and one Ingress TLS Secret `wamn-edge-tls` serve every host (`deploy/charts/platform/templates/edge-ingress.yaml`, `edge-certificate.yaml`). `wamn org create` makes no public certificate or DNS record today (`crates/platform-install/src/org_install.rs`).
10. Part 1 already states the T11 address scheme and the Epic 5 label bridge (`docs/plan/wamn-restructure.md:42`). The section 7 row "Application hosts use `<label>--<org>.<base>`" is stale.
11. No user setting for a default application exists. `identity.principal` and `identity.org_membership` hold no such field (`deploy/sql/system-schema.sql:220`, `:286`).
12. The plan does not define "client base paths". Appendix C reads it as I: application pages, assets, `config.json` and API calls under `/<project>/<application>/`, with org control and identity at the host root.

## 2. Proposed beads

| Bead | Content | Depends on |
| --- | --- | --- |
| E6.1 | Prod marker (T11, T2). A registry fact that marks one env per org as prod, with a refusal of a second one in SQL. The operator provisioning command sets the prod Namespace label from the committed row. | Question 2 |
| E6.2 | Routes from Services (T11, findings row 10). Web keeps its Namespace watch and adds one Service watch, metadata only. Route data moves to annotations on `<app>-http` and on the control Service. Both ConfigMap pairs, `Releases::read`, the per-request list, the edge ConfigMap right and the project Role app ConfigMap rights go. | Question 6 |
| E6.3 | Address scheme (T11). Web derives the host from the env namespace and the prod label, and the path from the project and application. Web translates the public path to the upstream path. The upstream Host stops being the public host. `registry.env_app.label`, `UNIQUE (org, label)`, `app_host`, `MAX_APP_LABEL_LEN`, `app_label_reason`, `route_label`, `routeHost`, `webClients`, `webDigests` and the env document `label` go. | E6.1, E6.2, Questions 3, 5 and 7 |
| E6.4 | Client base paths. The browser transport, `config.json`, the Vite helper, the web probe, the CLI `--host-url` callers, `xtask bench` and the journey address helpers use the application mount. Control and identity stay at the root. | E6.3, Questions 1 and 7 |
| E6.5 | Reserved root paths (T11). The existing project slug reservation (`project_id_is_reserved`) and its SQL check also refuse the first segments that the root routes use. | Question 4 |
| E6.6 | Per-org certificate on kind (T11). `wamn org create` makes one certificate `*.<org>.<domain>` with `<org>.<domain>` from a cert-manager local CA Issuer, and its Ingress. The shared `*.<platformDomain>` certificate goes. GCP Certificate Manager stays in Epic 7. | Question 9 |
| E6.7 | Landing page, chooser and default application (T11 "To design"). | Question 8 |
| E6.8 | Epic close. The image builds of section 5 after your go, then the kind journeys on a fresh wamn-local per group. Plan and Part 1 corrections. | all |

E6.1, E6.5 and E6.6 can start at once, because they own separate files. E6.2 and E6.3 both change `services/web` and `crates/kubernetes/src/web_release.rs`, so they run in order. E6.4 follows E6.3, because the mount comes from the address scheme. Decision recorded under rule 1 (greenfield): the cutover refuses a rollback to an app Helm revision that predates T11, and keeps no compatibility path for the ConfigMap routes (`crates/env/src/env/operation/rollback.rs`).

## 3. Changes

### Schema

All are I, on the bootstrap SQL only, with no migration.

- `registry.env` gains a prod fact with one prod row per org at most (E6.1).
- `registry.env_app.label`, its check and `UNIQUE (org, label)` go (E6.3).
- The `ProjectId` SQL check refuses the reserved root segments (E6.5).
- A default application setting, if Question 8 rules one (E6.7).

### Wire

- The env document loses `label`, and `crates/env/env.schema.json` is generated again (E6.3).
- Route annotations on the `<app>-http` Service and on the control Service become the route contract between the app chart, the org chart and web (E6.2).
- `config.json` for an application moves to the application mount and keeps its keys (E6.4).
- OpenAPI paths and the generated SDKs do not change if web translates the public path (I, appendix C).
- A chooser needs one authenticated list operation, its OpenAPI and its SDK binding, if Question 8 rules it (E6.7).

### Behaviour

- Every app URL changes from `<label>--<org>.<domain>` to `<org>.<domain>` for prod and `<env>.<org>.<domain>` for other envs, at `/<project>/<application>` (E6.3).
- A route in `<org>--<env>` serves only its own env host. Only the prod env serves the org apex (E6.1, E6.3).
- Two apps in one env share one origin and one session. The session cookies stay `__Host-`, `Path=/`, `SameSite=Strict` (`services/web/src/browser_session.rs:231`), so a prod cookie is not sent to a dev host. A dev page is same-site to prod, so the T9 check must refuse unsafe cookie requests from it (appendix C).
- Each journey that builds an app URL changes: `app_routes_kind`, `data_change`, `host_contracts`, `installed_digest`, `isolation`, `lifecycle`, `nats_outage`, `pooler_renewal`, `poolers`, `request_path`, `smoke`, `waits`, `workflows` and `write_log` (appendices A, B and D). The standing rule of 2026-11-27 applies: the lane that changes the address updates every journey that reads it.

## 4. Network calls and open transactions

No proposed change puts a network call inside an open database transaction (I). The prod Namespace label is applied after the SQL Job commits, as the worker login Secret already is (`crates/platform-install/src/provision_org.rs:295`). The watches run in web and open no transaction. Certificate requests run in `wamn org create`, outside its SQL Job. Appendix D reports two existing cases, the first-owner provisioning call to identity and the Cloud KMS call at signing key open. Root did not check them in this pass, and T16 records both.

## 5. Image rebuilds

The plan says no rebuild for Epic 6 (correction 7). The proposed beads need these, at E6.8 after your go (I):

- `edge-web`, for E6.2, E6.3 and E6.4.
- `control`, for the embedded system SQL and the carried charts (E6.1, E6.3, E6.5, E6.6).
- `host`, because E6.3 deletes code in `wamn-registry` and `wamn-routes` and changes how the workload binds its host (Question 5).
- `identity`, only if Question 8 puts the default application in identity.
- The browser bundles of Platform UI and Receiving, for E6.4 and E6.7.

## 6. Where the plan does not say enough

- T11 does not define "client base paths" (order of work, T11). Question 1.
- T11 and T2 do not give the prod input, whether prod can move, the case of an org with no prod env, or the repair when the SQL commit succeeds and the Namespace label write fails.
- T11 does not give the annotation keys, the control Service representation, duplicate route handling, or what web serves while its first lists load and after a lost watch.
- T11 does not give the upstream Host contract. Today every app in an env would bind its public host to the same handler (`routeHost`), so two apps on one env host need distinct upstream hosts.
- T11 does not give the public form of the native file, flow and health routes, redirect rewriting, trailing slashes, or the syntax of an application segment.
- T11 gives example root paths only. The full reserved list, and whether a reservation covers a whole first segment, are open.
- T11 leaves the landing page, chooser, default application storage, eligibility and active role open ("To design").
- T11 does not give kind CA trust for callers, certificate readiness in `org create`, or the split of DNS and certificate map work with Epic 7.

## 7. Questions for rulings

Each has a recommendation (I).

1. What are client base paths? Recommend: one mount `/<project>/<application>/` for app pages, assets, `config.json` and API, with a trailing slash. Org control and identity stay at the host root.
2. How is prod chosen? Recommend: `wamn env create <org>/<env> --prod`, fixed at first provisioning. An org may have no prod env. Moving prod is not in Epic 6.
3. What may an application be named? Recommend: one URL-safe slug segment, with the project slug rule, in Rust and SQL. Today any nonempty text is accepted.
4. Which root paths are reserved? Today `project_id_is_reserved` refuses `wamn` and `wamn-*` (F, `crates/registry/src/identifiers.rs:84`), and the slug syntax already excludes `wamn_control` and `config.json`. Recommend: also refuse `api`, `password`, `oauth`, `assets`, `clients`, `platform-ui`, `healthz`, `invite`, `recover` and `reset`, by first segment.
5. Which Host reaches the app workload? Recommend: an internal host per Service, `<app>-http.<org>--<env>.svc.cluster.local`, in place of `routeHost`, with web translating the public path. This changes the host binding, so the host image rebuilds.
6. Where does control routing come from after the ConfigMaps go? Recommend: annotations on the existing control Service `c-<org>-http`, read through the same Service watch.
7. What are the public native paths? Recommend: `/<project>/<application>/api/files`, `/api/flow` and `/api/health`, translated by web to the current native paths. Org control routes and the OAuth callback stay on the org origin.
8. Is the landing page in Epic 6? Recommend: no. Epic 6 keeps the current sign-in redirect, and the chooser and default application go to a later design with their own ruling.
9. Who owns the per-org certificate on kind, and does `edge.acme` stay? Recommend: the org Helm release, in `hosts`, from one local CA Issuer. Retire `edge.acme`, because GCP uses Certificate Manager in Epic 7.
10. Is the host image rebuild accepted for Epic 6? Recommend: yes, once at E6.8, with `edge-web` and `control`.
