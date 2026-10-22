# Integrations

The one authority for connector work on wamn. It replaces the candidate list and the batch plan.

## 1. How this document is used

- **One epic is scoped at a time.** Only the epic marked *scoped* below has issues. The implementing agent builds that epic, closes it, and stops. The next epic is scoped into issues after the owner reviews the closed one, because each epic will change what the next one needs.
- An agent never scopes, starts or pre-builds a later epic, never files its issues, and never widens the scoped epic with a later epic's rows "while it is there".
- A fork that changes a ruling or an invariant below stops the agent; the agent asks. An uncovered fork is not decided by the agent.
- Rows in §3 are candidates, not commitments. A row joins an epic when that epic is scoped.
- Measured on main `e8cc026`. Citations are `file::symbol`.

## 2. Current state

### Cloud host

| Fact | Citation |
| --- | --- |
| Tenant imports are a closed exact-version registry of nine rows: `wamn:node`, `wamn:postgres` 0.1/0.4, `wamn:connection` 0.1, `wasmcloud:blobstore` 0.1, `wasi:logging`, `wasi:io`, `wasi:clocks`, `wasi:random`. `wasi:http/outgoing-handler` and `wasi:sockets` are refused for tenants. | `crates/platform/component-policy/src/lib.rs::CAPABILITY_REGISTRY`, `analyze_tenant` |
| Outbound HTTP exists as a plugin: the guest names a requirement; the host owns endpoint, TLS policy, peer pinning, bounds, and injects the credential as static headers from the secret `{"headers":{…}}`. | `crates/platform/runtime/src/plugins/connection_http.rs::ConnectionHttp`, `credential_headers` |
| Only `blobstore` is a provisionable connection type. HTTP bindings exist in tests and the dev loop only. | `crates/catalog/model/src/connection.rs::RequirementType`, `services/ctl/src/component_verbs.rs::RequirementTypeArg`, `crates/schema/generator/kcl/environment/environment.k::Connection.type` |
| Credentials: a JSON file keyed project → handle, mounted from a Kubernetes Secret. The catalog stores the handle, never the secret. | `crates/platform/runtime/src/plugins/wamn_credentials.rs::WamnCredentials`, `crates/control/lib/src/bind_connection.rs::bind` |
| `CredentialType::OAuth2Bearer` is declared and unused. | `crates/platform/runtime/src/connection_generation.rs::CredentialType` |
| Triggers: HTTP route attachments; event registrations (CDC → JetStream → materializer → queued run); manual `workflow start`. `AttachmentType::Cron` exists with no scheduler. | `crates/execution/workflow/src/contract.rs::Workflows`, `crates/catalog/model/src/lib.rs::AttachmentType` |
| Route authentication is one seam. No signature (HMAC) policy exists. | `crates/platform/runtime/src/plugins/route_authentication.rs` |
| Blob store serves `s3` and `gcs` through `object_store`. | `crates/platform/runtime/src/plugins/wamn_blobstore/binding.rs::resolve` |
| Email: Resend is identity-only, no guest path. | `services/identity/src/mail.rs::Mailer` |
| No MQTT, AMQP, Kafka, SFTP, webhook-signature, OAuth-flow, or native-driver code exists. | — |
| Adding one plugin touches: WIT dep and world (`crates/platform/runtime/wit`), `plugins/<name>.rs` + `plugins/mod.rs`, `crates/execution/host/src/operation/native_policy.rs::{NativePolicyResources, world, on_workload_item_bind}`, `operation.rs`, `services/host/src/host.rs`, the registry row and its conformance test, and when connection-backed the sites in the `RequirementType` row above. | as named |

### Edge (`wamn-edge`)

| Fact | Citation |
| --- | --- |
| One serial device, newline frames, one call per frame as a fixed principal. | `services/edge/src/serial.rs::read_frames`, `device.rs::run` |
| One device and one application per box. | `services/edge/src/config.rs::EdgeConfig.device` |
| SQLite intents and samples in one file; HTTPS forward with a 0600 PAT file; refusal, backoff and credential stop rules. | `crates/execution/run-state-sqlite/src/lib.rs::SqliteIntentStore`, `services/edge/src/forward.rs::send` |
| A LAN HTTP listener already serves the release routes on `[http].listen`. No TLS. PAT on local routes answers 503. | `services/edge/src/serve.rs::serve`, `authenticator.rs::EdgeAuthenticator` |
| Guests import `wamn:node/types` only. | `services/edge/src/policy.rs::EdgePolicy::world` |
| The edge never links `wamn-runtime`; the dependency test refuses it. | `services/edge/Cargo.toml`, `services/edge/tests/dependency_boundary.rs` |
| A host-driven device (write, poll with handshake) needs a host plugin and a WIT import; ruled, not built. | edge spec ruling §4.2 |
| Release bundle pinned by digest, copied by hand, unsigned. No cloud→edge channel. | `services/edge/src/release.rs::EdgeRelease::load` |
| aarch64 release binary 38.5 MiB; hardware RSS unmeasured. | edge spec §4.9 |

## 3. Candidates

Entry types: **plugin** = host plugin + WIT + registry row. **route** = inbound HTTP. **package** = authored package, no platform change. **device** = `wamn-edge` device loop. **sidecar** = native driver container (C1). Size: S ≤ 2 days, M ≤ 1 week, L > 1 week.

Rows marked *done* or *dropped* stay for the record.

### Cloud

| # | Integration | Entry | Needs | Size |
| --- | --- | --- | --- | --- |
| C0 | HTTP connection provisionable (`Http` requirement type through ctl and `environment.k`) | catalog + ctl | — | S |
| C2 | OAuth2 client-credentials, outbound | credential type in `connection_http` | C0 | S |
| C3 | OAuth 1.0a TBA, outbound | credential type in `connection_http` | C0 | S |
| C4 | Schedule trigger (`workflows.<id>.schedule`) over `Workflows::start` | host task | — | S |
| C5 | Secret Manager through External Secrets for connector credentials | platform config | — | S |
| C6 | Scoped machine API keys per connector | identity | — | S |
| C7 | MQTT subscribe | NATS MQTT listener (config) or plugin | decision `wamn-02q` | S or M |
| C8 | MQTT publish | same as C7 | C7 | S |
| C9 | Inbound webhook: signature auth policy + route that calls `start` | route | — | S |
| C10 | Outbound webhook with HMAC signing | package over HTTP | C0 | S |
| C11 | Google Pub/Sub | plugin | — | M |
| C12 | AMQP | plugin | — | M |
| C13 | Kafka | plugin | — | M |
| C14 | AWS SQS/SNS | plugin | — | M |
| C15 | NATS exposed to guests | plugin | owner ruling (widens egress) | S |
| C16 | SSE to the browser | route | — | M |
| C17 | CSV import: upload route → validate → one command per row | route + package | inbound body bound check | M |
| C18 | SFTP drop | plugin | — | M |
| C19 | S3-compatible object store | *done* (`object_store` s3) | — | — |
| C20 | Generic REST connector package: base URL, auth, pagination, rate limit | package over HTTP | C0, C2/C3 | M |
| C21 | NetSuite REST (SuiteTalk, SuiteQL) | package over C20 | C3 | M |
| C22 | NetSuite RESTlet | package over C20 | C3 | M |
| C23 | NetSuite ODBC (SuiteAnalytics Connect) | sidecar | C1 | L |
| C24 | Dynamics 365 BC (OData v4) | package over C20 | C2 | M |
| C25 | QuickBooks Online | package over C20 | C2 | M |
| C26 | Shopify / WooCommerce | package over C20 + C9 | C2, C9 | M |
| C27 | Email send for guests (Resend) | package over HTTP | C0 | S |
| C28 | Inbound email parse | route | C9 | M |
| C29 | SMS | package over HTTP | C0 | S |
| C30 | PDF render | plugin, unless a wasm crate passes admission | measure first | M |
| C31 | Google Sheets | package over C20 | C2 | S |
| C32 | SAP (OData) | package over C20 | a user | L |
| C33 | OIDC login | identity | deferred `wamn-jpx2` | L |
| C34 | SAML | identity | C33 | L |
| C35 | OTLP to a vendor | collector config | a vendor | S |
| C36 | Prometheus `/metrics` | host | — | M |
| C37 | Alerting (Slack, PagerDuty) | package over C20 | C4 | S |
| C38 | Cross-package reads: one package reads another package's model | platform | owner ruling | — |
| C39 | CDC only for the schemas that an event registration reads | platform | — | S |
| C40 | CDC capture of several registered schemas in one environment | platform | owner ruling | M |
| C1 | Native driver sidecar: `wamn:connection/driver` + declared container | plugin + deploy | — | L |

### Edge

| # | Integration | Entry | Needs | Size |
| --- | --- | --- | --- | --- |
| E0 | Device list: `[[device]]`, one loop per device, status per device | edge config + loop | — | S |
| E9 | Serial framing: delimiter, fixed length, STX/ETX | device | — | S |
| E10 | Barcode / RFID reader (serial, keyboard-wedge) | device over E9 | E9 | S |
| E2 | Modbus TCP poll on a schedule (`tokio-modbus`) | device | E0 | M |
| E3 | Modbus RTU | device | E2 | S |
| E11 | Outbound HTTP on the edge | plugin: `connection_http` in its own crate, linked by both hosts; `EdgePolicy::world` widened by grants | — | M |
| E13 | Local printer: IPP over E11; raw 9100 is TCP and needs E22 | package | E11 / E22 | S |
| E22 | Device plugin + WIT for host-driven devices (open/read/write on a bound device) | plugin | — | M |
| E14 | Modbus write | device over E22 | E22, E2 | S |
| E8 | GPIO / 1-Wire | device over E22 | E22 | S |
| E7 | CAN bus (socketcan) | device over E22 | E22 | M |
| E1 | MQTT subscribe (`rumqttc`), samples into the store | plugin | E11 pattern | M |
| E12 | MQTT publish | plugin | E1 | S |
| E4 | OPC UA client (`async-opcua`, pure Rust) | device over E22 | E22 | L |
| E5 | Siemens S7 (`rust7` / `s7`, young; verify on a PLC) | device over E22 | E22 | M |
| E6 | EtherNet/IP CIP (`rseip`, pure Rust) | device over E22 | E22 | M |
| E15 | TLS on the LAN listener; PAT on local routes | edge | — | S |
| E16 | Local web page: the web bundle in the release bundle | edge | E15 | M |
| E17 | Cloud → edge commands (NATS leaf or HTTPS long-poll) | edge + platform | owner decision | L |
| E18 | Release pull and signature | edge + platform | E17 | M |
| E19 | Fleet status route and screen | cloud route + UI | E17 | M |
| E21 | mTLS edge identity | identity | — | M |

None of E4–E6 needs the sidecar: pure-Rust clients exist. C1 serves C23 and other closed drivers only.

## 3a. Integration shape

Owner rulings of 2026-10-21. Each ruling holds for every integration from Epic 4 on.

| Ruling | Invariant |
| --- | --- |
| A sync integration (NetSuite, Dynamics, QuickBooks, Shopify and the like) never serves the request path. It is its own application, with one package lineage for every org, project and environment. The environment's one release names the integration packages beside the consumer, pinned by version, with no consumer rebuild. Updating the integration is one publish and one `env apply` per environment. | No consumer route calls an integration synchronously. A consumer starts the integration's workflows, and the integration pushes into consumer commands by workflow. |
| A compute service returns a value in request time. It is an outbound call inside a consumer operation, made by a thin package composed into the consumer. | The compute package declares no model where it can avoid one. |
| An integration is stateless. It has no domain models and no staging tables. | Every domain table belongs to the consumer. The integration reaches it only through the consumer's commands and events. A consumer never reads from an integration. |
| Standard state is one platform package, `wamn_integration`, composed into every integration application. Model `setting` holds per-environment key and value pairs that an admin edits: account ids, mappings, enabled targets and `last_synced_at`. Model `log` holds one row per run outcome worth keeping beyond `runs`: records pushed and vendor refusals, by record id. It is delivered like the platform nodes. | No new WIT. An integration keeps no state outside `setting`, `log` and the catalog. |
| Connection, credential, schedule, runs and grants stay in the catalog. An org-scoped connection and credential are declared once, and `env apply` materializes them into each enabled environment. | Two environments that name one org connection resolve one secret. |

## 4. Epic order

Each epic names a goal and an exit condition. Only Epic 5 has issues. The order after Epic 5 is expected, not fixed. The owner re-orders at each review.

### Cloud

| Epic | Goal | Exit condition |
| --- | --- | --- |
| 1 (done at `05f7659d`, `wamn-jstjf`) | C0. An HTTP connection is bound in a deployed environment and a guest sends through it. | Met. |
| 2 (done at `030ce57c`, `wamn-5i3v9`) | C9, C4, C10. Webhook in, schedule, webhook out. | Met. |
| 3 (done at `afba25a3f`, `wamn-7e39m`) | C2, C3, C20. OAuth credential types; the REST connector package. | Met. |
| 4 (done at `fde0b0cd5`, `wamn-lcaxg`) | Org-scoped connection, C39, the integration principal, `wamn_integration`. The §3a shape without a vendor. | Met. |
| **5 (scoped)** | C21, C22. NetSuite REST and RESTlet in the §3a shape. | §5. |
| 6 | C27, C29, C31, C37. Packages over C20. | Each sends once from a run. |
| 7 | C1, C23. Sidecar and ODBC. | A sidecar opens the Oracle ODBC driver, streams one query to a guest, holds no credential. |
| later | C24–C26, C17, C30, C7/C8, C11–C14, C16, C18, C28, C5, C6, C36 | Picked by need. |
| deferred | C15, C32, C33, C34, C35 | Owner rulings or a user. |

### Edge

| Epic | Goal | Exit condition |
| --- | --- | --- |
| A | E0, E9, E10. Device list and general framing. No WIT. | Two serial devices on one box each produce samples; a barcode frame reaches a route. |
| B | E2, E3. Modbus poll. | Registers polled on a schedule become samples and forward. |
| C | E11, E13 (IPP). First guest capability on the box. | A box guest sends to a LAN printer through a bound HTTP connection; the dependency test still refuses `wamn-runtime`. |
| D | E22, E14, E8. Device plugin with Modbus write first. | A route sets a register and toggles a GPIO pin through the plugin. |
| E | E1, E12. MQTT. | Broker messages become samples; a route publishes a command. |
| F | E4, E5, E6. PLC clients as device types. | Each reads one tag from a simulator or PLC. |
| G | E15, E16. TLS and local page. | A user signs in at the box with no cloud. |
| H | E17, E18, E19, E21. Channel, pull, fleet. | After the owner decides the channel. |

Edge Epic A may run in parallel with cloud Epic 1: they share no files.

## 5. Epic 5 — NetSuite (C21, C22)

### Goal

NetSuite is one integration application, `apps/netsuite`, composed with `wamn_integration` on an org-scoped `oauth1` connection. Its operations are wirings over `http-request` and `http-page`, with no vendor code. One scheduled pull brings SuiteQL rows into a consumer command, and one push turns a consumer event into a NetSuite record. Issue 1 first fixes the Epic 4 review findings on main. Branch `work/integrations` from `35130b0b9`.

Out of this epic: no UI, no Secret Manager, no ODBC (C23), no capture of several schemas (C40), no other vendors, no new WIT, and no OAuth2 JWT assertion. NetSuite machine-to-machine access uses `oauth1` only in this epic. C40 stays out, because NetSuite needs one registered schema. If the kind case proves otherwise, the work stops for the owner.

### Rulings

Owner rulings for Epic 5. Each ruling is recorded verbatim on the epic bead.

| Ruling | Invariant to measure |
| --- | --- |
| Apply creates one service principal for each application that composes `wamn_integration`, not for each schedule. It grants that principal a role that holds the operations that the application's release declares as dependencies. Publish authorizes the role, apply materializes it with `ON CONFLICT`, and no admin grants it by hand. This replaces the Epic 4 ruling of a manual grant. | A schedule run right after apply reaches its consumer command. A second apply writes nothing. |
| The runtime schema of a composed application derives from `base_dependencies`. No integration declares the models of its base again. This narrows the Epic 4 ruling that derived the schema from the application's own models. | `apps/rest_connector/wamn.k` carries no model of `wamn_integration`. |
| `http-page` gains `method`, with GET as the default, and `body-input-pointer`: the input value at that pointer is the JSON body. Offset style gains an optional `has-more` pointer that ends the loop when its value is false. | A POST page carries the body plus `offset` and `limit`. The loop stops on `hasMore: false` without a request for an empty page. |
| NetSuite is one application, `apps/netsuite`, composed with `wamn_integration` on an org-scoped `oauth1` connection. Its operations are `record.get`, `record.create` and `record.update` on `/services/rest/record/v1/<type>[/<id>]`, `suiteql.query` (POST `/services/rest/query/v1/suiteql`, offset paging, `hasMore`, `Prefer: transient`) and `restlet.call` (C22: `script`, `deploy`, body). Record type, script and deploy are node parameters. Account id and realm are `setting` rows. A SuiteQL query orders by a stable key. | Every request that the fixture sees carries a valid OAuth1 signature with the realm. No guest sees a key. |
| One scheduled pull: `suiteql.query` windowed by `last_synced_at`, with `carry-input-pointer`, then `jsonata`, then the fixture consumer command, then `setting.write`. One push: a consumer event, then `jsonata`, then `record.create`. | `last_synced_at` advances only after the consumer command succeeds. A failing consumer leaves it unchanged. The push creates one record for each event, by delivery id. |

### Issues

1. **Review fixes on main.** All findings of the Epic 4 review, with no new behavior: the org of a connection handle derives from the tenant, OAuth1 base strings percent-decode only and include a form body, a schedule with no next occurrence is skipped and refused at publish, the host schedule task reconnects and reports to the probe, application principals follow the ruling above, `cdc_in_place` compares the publication's schema, the serving manifest format returns to 9, the credentials file has one shape `{projects, orgs}`, the runtime schema derives from `base_dependencies`, `require_direct_transport` is deleted, one `wamn_catalog::parse_workflow_schedule` serves every reader, and the tests gain the restart, failing-consumer and `/recorded` cases.
2. **`http-page` method, body and `has-more`.** Unit tests cover a POST with a body, the `hasMore` stop and unchanged GET paths.
3. **`apps/netsuite`.** The manifest, five operations, two wirings and the settings. The publication test covers the error cases of `assemble_local_release` only. Unit tests cover the window query and the `jsonata` mappings.
4. **Kind case.** A NetSuite-shaped fixture verifies OAuth1 with the realm, serves SuiteQL POST pages with `hasMore`, record get and create with 204 and `Location`, and a RESTlet echo. The pull and the push each run once, and the assertions read `/recorded`. `tools/netsuite/live.sh` is run by the owner against the real account, from a credentials file that the owner supplies. It calls `record.get` and `suiteql.query` once. It is documented and is never a test target.

Per-issue budget: build, clippy and unit tests. The kind case runs once at epic close. Before the merge, the live targets that touch the changed files run. A stage silent past 30 minutes is killed. A seam of the Epic 1 class is fixed in place and recorded in the close reason.

### Closeout

Rebase, regenerate, no diff, fast-forward to main, close the epic bead with the test log path, delete the remote branch. Then stop. Epic 6 follows the owner's review.

## 6. Sidecar shape (for Epic 6, not before)

Driver in its own container: cloud, a second container in the host pod; edge, a second process on the box. One protocol over a Unix socket: `open(connection) → handle`, `call(handle, method, params)`, `stream(handle, method, params) → rows`, `close`. The host plugin proxies; the guest sees a WIT import. The sidecar holds no credential (the host passes the secret at `open`) and reaches no database, NATS or registry. `environment.k` declares it as a connection with `driver = <image>`; `env apply` renders the container. Licensed drivers are owner-supplied images; wamn never bundles them.

## 7. Assumptions

| Assumption | Verify | If false |
| --- | --- | --- |
| `ConnectionTypeDescriptor::http_v1` already matches what `ConnectionHttp` checks at resolve (`authorize_snapshot`). | Issue 1 reads both; issue 3 proves it. | Epic 1 grows to M; the descriptor is corrected, not the plugin. |
| `expand.rs::credential_handle` and the credentials file need no change for a second connection type. | Issue 2 and 3. | A handle rule per type is added in issue 2. |
| Inbound route body bound is separate from the 8 MiB outbound bound. | Read `transport.rs` and the route layer when C17 is scoped. | C17 needs a streaming upload route. |
| The edge world can admit one plugin import without pool changes. | `serve.rs::edge_engine` when Epic C is scoped. | Epic C grows. |
| `rust7` / `s7` reads a real S7 PLC. | Simulator or hardware when Epic F is scoped. | E5 moves to snap7 under C1. |
| The NATS MQTT listener meets C7's auth needs. | Cluster NATS config when C7 is scoped. | C7 becomes a plugin. |
