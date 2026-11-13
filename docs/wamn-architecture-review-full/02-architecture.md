# Architecture: current and proposed

Current is the code as the review tabs read it: tabs 01–58 at `553511bbd`, tabs 59–88 at `epic/dep7` (b94833fd). Proposed is that code with every ruling applied. Each row names its tab; (inference) marks what no tab states. Open points are in the Summary tab.

## Runtime at a glance

![current runtime paths · two write drivers](<images/architecture-current.png>)

Today a route call and a workflow node reach PostgreSQL by two drivers: the route guest's write loop and the host loop.

![proposed runtime paths · one write driver](<images/architecture-proposed.png>)

Proposed, a workflow node sends the same HTTP request a browser sends, in process, so the route guest is the only write driver and the host loop goes.

## Boundary

|  | Platform | Application |
| --- | --- | --- |
| Owns | Cluster services (NATS, wasmCloud runtime operator, CloudNativePG, identity, web front); the host image; one host per environment; the project databases, roles and credentials; the event stream and change capture | `wamn.k`, migrations, SQL, component code, web client, its release |
| Lifecycle | Install and upgrade of a cluster and an environment (`wamn-platform-install`, Helm charts) | Build, publish, deploy, schema change (`wamn`, through control routes) |
| Must guarantee | Org isolation; a caller runs only what it was granted; a component reaches only what its release declares; a write commits once | Its own migrations, SQL and code are correct |
| Must not need | Knowledge of how an application was authored | A platform rebuild to change application code or model |

### Where the two sides cross

| Crossing | Current | Proposed | Tab |
| --- | --- | --- | --- |
| One model crate for both sides | `wamn-catalog` holds the authored model and the served model; host, engine, runtime, workflow and control crates compile it | Unchanged; no ruling splits the crate | 74–76 |
| The host reads SQL text at run time | `control_route.rs::release_profile` reads SQL through `sql_lex.rs` | `pg_query` is the one parser and the manifest is checked once, at build; the fact comes from the build (inference) | 74 |
| Platform code inside each application artifact | The route guest is composed into each application's entry at publish | Kept: build-time composition stays, and the composer also generates the route table | 31, 73 |
| Generated code included by path | 33 component files `include!` paths under `target/wamn/` | The generated module is written into the application's source folder | rules row 11 |
| An error file per application | `data/error.rs` repeated in five components | Errors are shared in the guest library | 04 |

## Crates

| Crate or group | Current | Proposed | Tab |
| --- | --- | --- | --- |
| `wamn-execution-contract` | Shared types and helpers on six topics; 26 crates use it; about a third has no caller | Deleted. Port names move to `wamn-catalog`, the digest to `wamn-digest` | 01 |
| `wamn-control-registry` | Environment identity plus an unused in-memory copy of the registry; levels ordered org, project, environment; an environment spelled five ways | `wamn-registry`: org, environment, project; registry SQL in one place; one name module | 02, 82, 88 |
| `wamn-event-wire`, `wamn-postgres-statements`, `wamn-number-pattern` | One purpose each | Kept. Serializers derived and one spelling for field names; the SQLx fixture goes | 03, 04, 08 |
| `wamn-session` | Hand-written JWT | `jsonwebtoken` 11.1.0 with EdDSA | 05 |
| `wamn-pg-core` | Quoting helpers; its `Sql` type has no caller | Deleted; callers use `postgres-protocol` | 06 |
| `wamn-component-policy` | Import policy, a build script and a dead path; raw SQL refused to applications | One table of allowed imports; raw SQL allowed, held read-only by its login | 07, 11 |
| `wamn-run-state` | The run queue plus five other things; about 1,130 lines with no production caller | The run queue only. `CredentialGeneration`, `flow_id`, `flow_version` and the uncertain-intent path go | 09, 88 |
| `wamn-runtime` | Postgres plugin on `tokio_postgres`, claims and pools; SQL calls and write log; numbering; run admission; HTTP connection and OAuth plugins; route authentication; events and logging; blob store; one map per plugin; `reqwest` in three files | One scope registry; SQLx with TLS; a SELECT-only login for dynamic SQL; `oauth2` and `jsonwebtoken`; `moka` token cache; `hyper` only; the connection policy refuses loopback and unspecified addresses | 10–20, 25, 88 |
| `wamn-engine` | Route plugin, delivery, operation call, engine build, component admission; integrations called through one store per connection behind a mutex | Integrations on the native wash-runtime dispatch and pool; the provider check goes | 21–24 |
| `wamn-execution-host` | Operation host, file routes, application host routes, control route; a second HTTP server | `sign-result` on `wamn:operation`; one HTTP server; file routes at `/api/<application>/file[/<id>]`; a `moka` pool per environment | 25–27 |
| `wamn-router`, `wamn-workflow` | The router keeps its own wiring types and invariants; the workflow keeps claim flags, a durable caller mapping and `wiring_lowering.rs` | The router reads catalog wiring types; those parts go; park, release and list become control routes | 28–30 |
| http-route guest | The host holds the application route table | The route table is in the guest, generated by the composer; host routes become native ingress service routes | 31 |
| `wamn-schema-generator` | 25,624 non-test lines; contract files, source maps and a SQLx verifier | SQLx describes at build; overridable CRUD; contract files, source maps and the verifier go; an OpenAPI document | 32–53 |
| Identity crates and service | Hand-written JWT; user PATs | `jsonwebtoken`; no user PATs; Kubernetes client on `hyper` | 54–57 |
| `wamn-edge` | Two programs, the web front and the edge device runtime, share only `main.rs`; a device limiter | Separate crates; the limiter goes; one `wamn-kubernetes` crate | 58 |
| `wamn-control-provision` | Names, roles and SQL text; callers hold SQL text; Kubernetes manifests in Rust | Runs its own statements over SQLx; manifests become Helm charts | 59, 65 |
| `wamn-package-control` | Hand-written migration runner; package owner roles; definition ownership table; apply-time allowlist | SQLx `Migrator` with one migration role per project database; the ownership table and the allowlist go | 60, 77 |
| `wamn-environment-control` | Rust renderers, a ConfigMap, SQL Jobs, fingerprints, input hashing, Cloud Build offload | Helm charts and Helm history; the registry holds configuration; `Migrator` checksums; reset by `DROP OWNED BY`; BuildKit cache; split into `wamn-environment` and `wamn-platform-install` | 62–68 |
| `wamn-dev-control`, `wamn-host` | An Introspect stage, template machinery and local host modes | Six stages; Introspect, templates and local modes go; control is a native HTTP service; local development on `kind` | 69, 70, 72 |
| `wamn-ctl` | `wamn`, `wamn-ctl` and `wamn-ctl-ops`; nested SQL Jobs | `wamn` plus a hidden worker `wamn-ctl`; every user verb through a control route | 71 |
| Composer and virtualizer | Build-time composition; WASI imports rewritten | Composition kept; the virtualizer is deleted | 73 |
| `wamn-catalog` | `sql_lex`, an effect projection, a retired-key scan, `schemars` derives | `pg_query`; the manifest checked once at build; those parts go | 74–76 |
| Schema control, introspection, package artifact | Support for the hand-written runner; the allowlist | `wamn-schema-control` stops existing; route normalization once at build; an event trigger | 77 |
| Postgres client | No connection uses TLS | TLS on every connection | 78 |
| Materializer | A guest admits events | The `wamn_jetstream` plugin admits events natively | 79 |
| Apps | Authored `publication/*.json` beside `wamn.k` | `wamn.k` the one authored source; palette components built once with the platform | 80 |
| Scripts and tooling | Shell and Python with tests that match their text | One xtask crate plus a justfile; tools with no caller deleted | 81 |
| CDC reader | Shape and stream re-checks; a system database login | Registration from the config file; the re-checks go | 83 |
| Scenario worker, authoring model | Present, with three roles and three tables | Deleted | 84 |
| `wamn-client` | Client core | Deleted; dev test support uses the CLI's route client | 85 |
| Test trees | `wamn-gates` image, `deploy/gates`, deployed checks | Tiers stated on the test; those go | 86 |
| Guest data libraries, `pg_walstream` | The fork is vendored | A git dependency at the patch commit | 87 |

## Paths

### Route call

| Step | Current | Proposed | Tab |
| --- | --- | --- | --- |
| Entry | Host ingress; the host holds the application route table | Native ingress service routes; the route table is in the composed guest | 31 |
| Caller | The guest asks the host `authenticate`; the host decides from the headers | Same | 31 |
| Grant and write | Grant check in `begin-operation`; the write loop runs in the guest | Same | 31 |
| SQL | Postgres plugin on `tokio_postgres`, no TLS | SQLx with TLS; dynamic SQL on a SELECT-only login | 11, 34, 78 |

### Workflow node

| Step | Current | Proposed | Tab |
| --- | --- | --- | --- |
| Claim | A host claims a queued run whose release digest equals its own (at `553511bbd`) | A host claims runs of any release it serves (inference from one host per environment) | 14 |
| Node call | `invoke_node` calls the operation through the host loop (`invoke_logged`), with a two-step claim in the write log | The driver builds the HTTP request and hands it to the entry in process; the host binds the run's caller to the store; the host loop and the two-step claim go | 01, 31, 64 |
| Timeout | `IntentStore::uncertain` and an operator resolve step | `in-progress`, then a terminal `outcome-unknown`; a late finish still writes; no operator step | 88 |
| Order | `stream_seq` per record | No ordering guarantee | 88 |

### Event

| Step | Current | Proposed | Tab |
| --- | --- | --- | --- |
| Capture | The CDC reader on a vendored `pg_walstream`; shape and stream re-checks; a system database login | The fork as a git dependency; registration from the config file; the re-checks go | 83, 87 |
| Publish | NATS JetStream | Same; the message id is open (Summary) | 03, 83 |
| Admit | The materializer guest; run key `event:<sequence>:<source_event_id>` | The `wamn_jetstream` plugin admits natively; the run key is open (Summary) | 79 |
| Run | A queued run | Same; `wamn-run-state` is the run queue only | 09 |

### Edge device

| Step | Current | Proposed | Tab |
| --- | --- | --- | --- |
| Program | `wamn-edge` holds the web front and the edge device runtime | Separate crates | 58 |
| Serve | One release bundle behind an HTTP ingress; the three operation calls over a SQLite run-state file | Same; ingress on loopback by default, with a documented TLS front | 58, 88 |
| Samples and delivery | Call the host loop | The same request path as the cloud (inference from the one write path) | 58 |
| Limits | A device limiter | The pool bound only | 58 |

### Deploy

| Step | Current | Proposed | Tab |
| --- | --- | --- | --- |
| CLI | `wamn`, `wamn-ctl`, `wamn-ctl-ops`; nested SQL Jobs | `wamn` and a hidden `wamn-ctl`; every user verb through a control route | 71 |
| Install | Rust renderers for Kubernetes manifests; a ConfigMap | Helm charts for environment and org; Helm history; the registry holds configuration | 62, 65, 66 |
| Credentials | Two login generations | One login per role, as CloudNativePG managed roles; a changed Secret restarts the host | 09, 65 |
| Migrations | Hand-written runner; package owner roles; definition ownership table; allowlist read by a hand lexer | SQLx `Migrator`; one migration role per project database; an event trigger refuses GRANT and REVOKE; overlay check at build and dev | 60, 77 |
| Reset | Template machinery and fingerprints | `DROP OWNED BY` the migration role and rerun; `Migrator` checksums | 64, 69 |
| Build | Input hashing; Cloud Build offload | BuildKit cache | 67 |
| Release to host | Pod arguments; one release per pod | A workload start on the environment's host; the pod stays up (inference from one host per environment) | 72 |

## Data and credentials

- Databases: one per project of an environment of an org. Today the same set is keyed org, project, environment (tab 82).
- Roles: one migration role per project database (tab 60); an application login; a SELECT-only login for dynamic SQL (tab 11); schemas owned by the platform owner role (tab 68).
- Credentials: CloudNativePG managed roles; one Secret per role, mounted as a file; a changed Secret restarts the host (tabs 09, 65).
- Transport: TLS on every PostgreSQL connection (tab 78).
- Platform rows: each project database holds its copy, because a foreign key cannot cross databases; the identity crate writes the project user rows (tab 68).

## Rules for applications

Row numbers are kept; other tabs cite them.

| # | Rule | Current | Proposed | Tab |
| --- | --- | --- | --- | --- |
| 1 | Migrations may not use qualified names | A hand lexer refuses them | Joins to `platform` are allowed; no hand parsing; the privileges of the migration role bound a migration | 60, 77 |
| 2 | Migrations limited to a closed DDL subset | An allowlist at apply, read by a hand lexer | The allowlist goes; an event trigger refuses GRANT and REVOKE by the migration role | 77 |
| 3 | List reads are fixed statements, one file per sort and direction | Same | Open (Summary) | — |
| 4 | Manifest vocabulary and naming rules refuse the build | Same | Open (Summary); reserved platform model names are ruled | 26 |
| 5 | KCL is the only authoring form | `wamn.k` plus authored publication JSON | The built JSON manifest is the contract; `wamn.k` is the one authored source in the apps | 80 |
| 6 | Guests must avoid the standard library or be rewritten | WASI imports rewritten | Standard WASI on the import list, an empty host context, no rewriting | 07, 73 |
| 7 | Two login generations; a privilege check on every connection | Both | One login per role; the connection check is open (Summary) | 09, 65 |
| 8 | One role per workload family; per-column grants derived from statements | Grants derived from SQL text | The families stay; an application reads and writes its own schema; grants are not derived from SQL text | ruled 2026-11-10 |
| 9 | Exact version for an overlay base | A tag that holds another digest is refused | The digest is the identity; a version is a tag that resolves to the latest digest pushed under it | ruled 2026-11-10 |
| 10 | Wiring compatibility and qualification | Both checked | The wiring check stays; qualification is deleted (gone at dep7) | ruled 2026-11-10; code in 76 |
| 11 | Generated code included by path; an error file per application | Both | The generated module goes into the application's source folder; errors are shared in the guest library | 04 |
| 12 | Signed-file routes kept off the guest path | Off the guest path | The guest asks the host to sign (`sign-result`); workflows call routed operations over HTTP | 25, 31 |
| 13 | Application components may not import the raw query interface | Refused | Allowed; held read-only by a SELECT-only login | 07, 11 |
