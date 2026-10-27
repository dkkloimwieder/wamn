# Integrations

The one authority for connector work on wamn. It replaces the candidate list and the batch plan.

## 1. How this document is used

- One epic is scoped at a time. The owner reviews each epic before the next one is scoped.
- An agent never scopes, starts or pre-builds a later epic, never files its issues, and never widens the scoped epic with a later epic's rows "while it is there".
- A fork that changes a ruling or an invariant below stops the agent; the agent asks. An uncovered fork is not decided by the agent.
- Rows in §3 are candidates, not commitments. A row joins an epic when that epic is scoped.

## 2. Current state

### Cloud host

C0, C2, C3, C10, and C20 follow [outbound HTTP](../architecture/capabilities.md#outbound-http).
C4 follows [schedule workflows](../architecture/execution.md#workflows).
C19 follows [object storage](../architecture/capabilities.md#object-storage).
C39 follows [event capture](../architecture/data-access.md#event-capture).
See [capability admission](../architecture/capabilities.md#import-admission) and [org connections](../architecture/data-access.md#org-connections-and-credentials) for their boundaries.

Email through Resend serves identity only. Guest email, MQTT, AMQP, Kafka, SFTP, and native drivers need the candidate work below.

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

## 3. Candidates

Entry types: **plugin** = host plugin + WIT + registry row. **route** = inbound HTTP. **package** = authored package, no platform change. **device** = `wamn-edge` device loop. **sidecar** = native driver container (C1). Size: S ≤ 2 days, M ≤ 1 week, L > 1 week.

### Cloud

| # | Integration | Entry | Needs | Size |
| --- | --- | --- | --- | --- |
| C5 | Secret Manager through External Secrets for connector credentials | platform config | — | S |
| C6 | Scoped machine API keys per connector | identity | — | S |
| C7 | MQTT subscribe | NATS MQTT listener (config) or plugin | decision `wamn-02q` | S or M |
| C8 | MQTT publish | same as C7 | C7 | S |
| C9 | Inbound webhook: signature auth policy + route that calls `start` | route | — | S |
| C11 | Google Pub/Sub | plugin | — | M |
| C12 | AMQP | plugin | — | M |
| C13 | Kafka | plugin | — | M |
| C14 | AWS SQS/SNS | plugin | — | M |
| C15 | NATS exposed to guests | plugin | owner ruling (widens egress) | S |
| C16 | SSE to the browser | route | — | M |
| C17 | CSV import: upload route → validate → one command per row | route + package | inbound body bound check | M |
| C18 | SFTP drop | plugin | — | M |
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
| C33 | OIDC login | identity | external login adapter | L |
| C34 | SAML | identity | C33 | L |
| C35 | OTLP to a vendor | collector config | a vendor | S |
| C36 | Prometheus `/metrics` | host | — | M |
| C37 | Alerting (Slack, PagerDuty) | package over C20 | C4 | S |
| C38 | Cross-package reads: one package reads another package's model | platform | owner ruling | — |
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

Each epic names a goal and an exit condition. The owner reviews the order before each epic starts.

Connections, external systems, and bindings follow [org connections](../architecture/data-access.md#org-connections-and-credentials).

### Cloud

| Epic | Goal | Exit condition |
| --- | --- | --- |
| 6 | C27, C29, C31, C37. Packages over C20. | Each sends once from a run. |
| 7 | C1, C23. Sidecar and ODBC. | A sidecar opens the Oracle ODBC driver, streams one query to a guest, holds no credential. |
| later | C24–C26, C17, C30, C7/C8, C11–C14, C16, C18, C28, C5, C6, C36 | Picked by need. |
| By owner decision | C15, C32, C33, C34, C35 | Owner rulings or a user. |

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

## 5. Epic 5 — NetSuite (C21, C22)

See [outbound HTTP](../architecture/capabilities.md#outbound-http) for the shared nodes.

### Rulings

Owner rulings for Epic 5. Each ruling is recorded verbatim on the epic bead.

| Ruling | Invariant to measure |
| --- | --- |
| Apply creates one service principal for each application that declares a schedule or composes `wamn_integration`. It grants that principal a role that holds the operations that the application's release declares as dependencies. Publish authorizes the role, apply materializes it with `ON CONFLICT`, and no admin grants it by hand. This replaces the Epic 4 ruling of a manual grant. No publish refusal is added (owner ruling B of 2026-10-22). | A schedule run right after apply reaches its consumer command. A second apply writes nothing. |
| The runtime schema of a composed application derives from `base_dependencies`. No integration declares the models of its base again. This narrows the Epic 4 ruling that derived the schema from the application's own models. | `apps/rest_connector/wamn.k` carries no model of `wamn_integration`. |
| `http-page` gains `method`, with GET as the default, and `body-input-pointer`: the input value at that pointer is the JSON body. Offset style gains an optional `has-more` pointer that ends the loop when its value is false. | A POST page carries the body plus `offset` and `limit`. The loop stops on `hasMore: false` without a request for an empty page. |
| NetSuite is one application, `apps/netsuite`, composed with `wamn_integration` on an org-scoped `oauth1` connection. Its operations are `record.get`, `record.create` and `record.update` on `/services/rest/record/v1/<type>[/<id>]`, `suiteql.query` (POST `/services/rest/query/v1/suiteql`, offset paging, `hasMore`, `Prefer: transient`) and `restlet.call` (C22: `script`, `deploy`, body). Record type, script and deploy are node parameters. The account host is in the org connection definition, and the realm is in the oauth1 credential. Neither is a `setting` row (owner ruling C of 2026-10-22). A SuiteQL query orders by a stable key. | Every request that the fixture sees carries a valid OAuth1 signature with the realm. No guest sees a key. |
| One scheduled pull: `suiteql.query` windowed by `last_synced_at`, with `carry-input-pointer`, then `jsonata`, then the fixture consumer command, then `setting.write`. One push: a consumer event, then `jsonata`, then `record.create` as an upsert by external id, `PUT …/<type>/eid:<key>`, where the key is the consumer record's stable key from the event payload (owner ruling C). | `last_synced_at` advances only after the consumer command succeeds. A failing consumer leaves it unchanged. One NetSuite record exists for each consumer record across retries. |
| `http-request` gains `path-input-pointer` and `body-input-pointer`. The method stays a fixed parameter, and the node is unchanged without them (owner ruling D of 2026-10-22). An optional input-pointer, body or method parameter on a platform node (`http-request`, `http-page`, `jsonata`) is the agent's decision, recorded here. Work stops only for a WIT, catalog, authority or credential change. | A record path with an id or an external id is built from the input and stays confined to the connection's origin. |

### Review and vendor acceptance

1. **Review fixes on main.** All findings of the Epic 4 review, with no new behavior: the org of a connection handle derives from the tenant, OAuth1 base strings percent-decode only and include a form body, a schedule with no next occurrence is skipped and refused at publish, the host schedule task reconnects and reports to the probe, application principals follow the ruling above, `cdc_in_place` compares the publication's schema, the serving manifest format returns to 9, the credentials file has one shape `{projects, orgs}`, the runtime schema derives from `base_dependencies`, `require_direct_transport` is deleted, one `wamn_catalog::parse_workflow_schedule` serves every reader, and the tests gain the restart, failing-consumer and `/recorded` cases.
3. **`apps/netsuite`.** The manifest, five operations, two wirings and the settings. The publication test covers the error cases of `assemble_local_release` only. Unit tests cover the window query and the `jsonata` mappings.
4. **Kind case.** A NetSuite-shaped fixture verifies OAuth1 with the realm, serves SuiteQL POST pages with `hasMore`, record get and create with 204 and `Location`, and a RESTlet echo. The pull and the push each run once, and the assertions read `/recorded`. `tools/netsuite/live.sh` is run by the owner against the real account, from a credentials file that the owner supplies. It calls `record.get` and `suiteql.query` once. It is documented and is never a test target.

Node parameter decisions (agent, under ruling D):

- `http-request.path-input-pointer` overrides `path-and-query` for one request, with the same confinement. A missing or non-string target is a terminal error.
- `http-request.body-input-pointer` sends the selected value as the JSON body. Without it, the node sends the input bytes as before.
- `wamn_integration` `setting.read` takes an optional `carry` and returns it unchanged (owner ruling E of 2026-10-22). The NetSuite pull carries the schedule's due time through it.
- `jsonata` takes an optional `parameters` object, bound to `$parameters`, and an optional `port-input-pointer` that sends the selected result on `main` or `items`. Without them, the node is unchanged.
- The generator admits one component with no local operation when the package owns no model and uses only base SQL. NetSuite's setting adapter forwards the base's setting operations and holds no vendor logic.

## 6. Sidecar shape (for Epic 7)

Driver in its own container: cloud, a second container in the host pod; edge, a second process on the box. One protocol over a Unix socket: `open(connection) → handle`, `call(handle, method, params)`, `stream(handle, method, params) → rows`, `close`. The host plugin proxies; the guest sees a WIT import. The sidecar holds no credential (the host passes the secret at `open`) and reaches no database, NATS or registry. `environment.k` declares it as a connection with `driver = <image>`; `env apply` renders the container. Licensed drivers are owner-supplied images; wamn never bundles them.

## 7. Assumptions

| Assumption | Verify | If false |
| --- | --- | --- |
| Inbound route body bound is separate from the 8 MiB outbound bound. | Read `transport.rs` and the route layer when C17 is scoped. | C17 needs a streaming upload route. |
| The edge world can admit one plugin import without pool changes. | `serve.rs::edge_engine` when Epic C is scoped. | Epic C grows. |
| `rust7` / `s7` reads a real S7 PLC. | Simulator or hardware when Epic F is scoped. | E5 moves to snap7 under C1. |
| The NATS MQTT listener meets C7's auth needs. | Cluster NATS config when C7 is scoped. | C7 becomes a plugin. |
