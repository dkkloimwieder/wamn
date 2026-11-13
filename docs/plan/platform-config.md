# Platform configuration in applications

## Mechanism

See [platform data authority](../architecture/data-access.md#actors).

Each section carries the dependency it waits on, so it can be ruled and filed as that dependency lands.

| section | depends on |
| --- | --- |
| Mechanism, Tables, Declaration, Numbering, Connections, Roles, Settings, Environment copy | now |
| no network call inside a transaction (the host refusal) | after A5 |
| `file`, `file.*` routes, `file.put` before the operation | now |
| Connector versions; `connectors` in `environment.k`; `integration.form`; `target_application` digests | after loadable connectors |
| Local environments; local issuer; projections | after SQLite platform functions |

- Rows in `platform` are runtime data, edited through platform routes (the UI). They are never declared.
- A package declares what it needs from `platform` in `wamn.k`: relations it reads, sequences it fills, connections it binds. Declarations carry defaults; rows carry values.
- `environment.k` binds a package's declared connection names to connection rows by name, pins the connector digests the application runs, and names the target it runs on. It holds no values.

See [integration identity and bindings](integration-schema.md).

- Connectors deploy per application: a connector is a dependency of the application, pinned per environment like a package, and runs inside that application's isolation on its target.
- Three levels, each already present in the platform: org, project, application per environment. Environments are isolated: nothing in one environment's database reaches another.
- A project database holds only the org rows that project uses: every user and the connections it has enabled.
- No network call inside a database transaction. Once `begin-operation` is open, a storage put, a connection call or anything that waits on the network is a host refusal; inside the transaction there is only SQL. Storage and connections are called before the transaction opens, and their results enter it as values.
See [operation transactions](../architecture/data-access.md#inputs-queries-and-transactions).

## Tables

Every table below lives in `platform` in the project environment's database. Credentials and targets are control-plane data and are never materialized into an application database: the application cannot read what is not there.

See [environment database authority](../architecture/data-access.md#actors).

| level | table | columns | written by | application reads |
| --- | --- | --- | --- | --- |
| org | `user` | id, name, email, type (`user`, `service`, `platform`), active | platform routes | yes |
| application, per env | `setting` | scope\_type, scope\_id, key, value (jsonb), locked | platform routes, per environment | no (UI reads via routes) |

Control plane only, reached through platform routes and the host:

| table | columns | note |
| --- | --- | --- |
| `target` | id, name, type (`local`, `cloud`), status, last\_seen\_at | identity and release of a runtime; named in `environment.k`; the cloud cluster is one row |
| `target_application` | target\_id, application, environment, bundle\_digest, status; PK (target\_id, application, environment) | what each target runs, written by env apply and confirmed at release; null digest on the cloud target, which runs a release |

- `user` is a projection: identity in the system database is authoritative, and the control host writes the projection (as it writes `app_system.users` today) and propagates deactivation on the admin-hierarchy path. It holds every org user, not only users with a role on the project, so `created_by` / `updated_by` FKs never dangle. Deactivation is `active = false`; rows are never deleted.
- Application rows that name a user (`created_by`, `updated_by`, `employee.user_id`) carry a uuid FK to `platform.user`. The host fills `created_by` and `updated_by` from the operation caller; the payload cannot supply them.
- C4 covers `gcs` and `s3`. C10 owns disk stores and eviction.
See [file storage and access](../architecture/data-access.md#files).

- A `file` row is a file. A row nothing references is kept; deleting it is the user's job through the normal delete. No sweep of unreferenced rows: a file uploaded on purpose and linked to nothing is a normal file, and a list that hides unlinked files is a screen filter, not a deletion policy. Uploads use `<environment>/_wamn_files/staging/<uuid>` under the binding prefix. The host commits the row, copies staging to `<environment>/_wamn_files/live/<uuid>`, then deletes staging. An age rule removes staged objects only. Live objects never expire by age. A read repairs a missing live object from staging. If object deletion fails after row deletion commits, the host logs the key without retry. The collector removes aged orphans only under this environment's segment, including stores that other environments share. Clean up pre-change objects outside environment segments by hand once.

- A local `disk` store has `max_bytes` and `max_age` parameters: a box has a disk, so retention is the admin's explicit choice on that row, and an evicted blob takes its `file` row with it — the one place a row is removed by the platform. The box serves its own files; nothing on it is `public`. Forwarding a file to the cloud is an application decision carried over a `wamn` connection (a new row in a cloud store, same `sha256`); the platform never forwards on its own.

- Record provenance is the application's: a record that came from outside carries `external_system_id` (FK to the relation) and `external_id` (the record's identity in that system's namespace), never a connection id, with `UQ (external_system_id, external_id)` on the model. Which connection imported it is a fact about the run, not about the record. For a `wamn` connection the external system is the peer environment, one row per peer, created with the connection; it gives `project/environment/application` one home.

## Declaration in wamn.k

A package declares each platform thing it uses by name. The declaration is the only place a default lives.

The proposed `relations` declaration still needs an owner decision. See [integration and connection schema](integration-schema.md) for connection declarations.


```kcl
# environment.k — one per project environment; names the target, lists applications and their packages,
# binds connection names, pins connectors
environment = {
    target = "box-7"
    applications = {
        wms = {
            packages    = ["wms.station"]
            connections = { plc = "line-3-plc", printer = "printer-7", files = "box7-files", cloud = "wms-cloud" }
            connectors  = { modbus = "sha256:9f3e…", printer = "sha256:…", disk = "host", wamn = "host" }
        }
    }
}
```

- `targets`: the target types a package may run on, `cloud` by default. A package with models is `cloud` only; env apply refuses a package not allowed on its environment's target type.
- `relations`: platform tables the package reads. A relation is read-only by construction; a write statement, command claim or history trigger against it is a build refusal.
See [sequence declarations](../architecture/data-access.md#numbering).
- `connectors` in `environment.k`: the digest of each connector the application's connections need; `host` for a `form = host` capability. Apply refuses a connection whose integration has no pin. A bump is an edit to this line and an apply, the same as a package bump.
- A `wamn` connection's external system is the peer environment; its `parameters` carry `project/environment/application`. The connection adds transport and exchange only.
See [sequence declarations](../architecture/data-access.md#numbering).

## Numbering

See [numbering and counter authority](../architecture/data-access.md#numbering).

- Rendering, preview and validation all go through the plugin's one renderer (preview in a rolled-back transaction). No second parser, in SQL or anywhere. `set_counter` renders nothing.

## Connections

See [integration identity, generations, credentials, and bindings](integration-schema.md).

- The host's connection plugin opens the connection and reads `credential` from the control plane. No application database holds a credential; there is nothing to grant.
- A connector instance belongs to the application that binds the connection and runs on that application's target. A connection bound by two applications is two instances, each inside its own application's isolation. No connector is shared across applications.
- A physical port has one owner: a connection whose integration is exclusive media (`serial`, `modbus` over rtu, a printer port) may be bound by one application per target; a second binding is an apply refusal. The other application gets the data through a `wamn` connection.
- A machine is not a platform concept. The box next to a CNC is a `target`; "line 3 PLC at 10.0.3.7 unit 1" is a `connection` (integration `modbus`); the CNC is the application's `equipment` row, which links the connection that reaches it (`connection_id`, or a link table if one machine has several). One machine with two protocols is two connections.
- A target serving two applications runs two bundles (`target_application` rows): two components, two grants files, two local databases, one org session key. Wasm isolation between them, as in the cloud.
- Application to application is a connection with integration `wamn`: parameters name the target application, and the caller acts as the connection's service principal — nothing stored. The target application's ACL grants that principal a role; it does not know or care where the caller runs. app1 and app2 in the cloud both reach app3 on a box through their own `wamn` connections.
- An operation calls a `wamn` connection like any other connection, before its transaction: `request` waits with a bounded timeout, `event` does not. Approval rules and other logic that decide a record's status run inside the operation as composed packages; a trigger (cron, an event from a box) invokes an operation the same way a request does.
- The hierarchy is org → project → environment → application: an environment belongs to a project, runs on exactly one target, and applications are deployed into it. An application present in a cloud environment and in a local environment of the same project runs a different package in each (`wms.core` in the cloud, `wms.station` on the box); each environment has its own database, settings and counters, and the two talk through a `wamn` connection like any other pair.
- An integration package holds configuration and logging rows only; domain tables belong to the consuming application.

### Local environments

A local target runs a project environment on SQLite with no generated schema. Two target shapes, nothing in between: `cloud` (Postgres, a cluster, hosted or on-prem, models) and `local` (SQLite, one box, model-free). A local environment runs the model-free packages of its applications; a package says which target types it may run on.

- A package declares `targets = ["cloud"]`, `["local"]` or both. A package with models is `cloud` only; build refusal otherwise. Env apply refuses a package not allowed on the environment's target type. `wms.station` is a model-free package of `wms`: code plus connections (serial, modbus, the `wamn` connection to the cloud), composed under the same application name.
- The box's SQLite holds only what the platform writes, in tables the plugin owns with SQL the plugin writes: `number_counter`, `setting`, the role rows from the grants file, an outbox of operation results and events for the cloud, and projections. The plugin-hosted renderer gives the box gapless local numbers with no SQL function.
- Domain data lives in the cloud environment. The box queues events over its `wamn` connection; the cloud environment's operations commit them into real tables. Offline, the box keeps running its operations and filling the outbox.
- It is a full environment: own identity, settings, counters, copy. What it lacks is models, by design.
- Operator screens on the box are the same generated web client over the local package's contract: the commands it exports and reads over what the box stored (ruled: the host stores what an operation returns). Which operations a kiosk gets is the application author's choice. No second frontend.
- Login offline: the box issues its own sessions. It already holds a signing key and the role rows; it verifies a password or badge against a credential projection and signs a session for its environment, online or offline. The projection is hash only (argon2id), only for users holding a role on that environment, revoked by removing the role; a lost box is handled as a role-removal event for every user projected to it. Revocation reaches the box when online. Stated cost.
- Cloud data on the box is a projection: a read-only table the platform creates on the box with a model's contract columns (no triggers, no FKs, json as text), filtered by a required `where`, queried by the local package like a relation and joined in SQLite to the box's own rows for a screen; never written locally. One table shape, two feeds: `feed = "operation"` fills it from an operation of the local package that fetched the rows (now); `feed = "cdc"` fills it from the source environment's change stream over the platform connection and keeps it current while online, stale while offline (later: CDC per projected model, a filter language for `where`, a SQLite column mapping per contract type). A cross-application projection needs what any cross-application read needs: the box environment's principal holds a role in the source environment granting a read of that model.

```kcl
# mes.station/wamn.k
projections = {
    work_order = {
        from  = "plant7/prod/wms.core.work_order"      # project/environment/application.model
        where = "work_center_id = $station.work_center_id"
        feed  = "cdc"                                   # or "operation"
    }
}
```

### Connections as principals

See [integration identity and credentials](integration-schema.md).

- A sync that runs as its own service application needs no inbound grant: its members write their own tables, and the interactive application reads them as relations.
- `wamn` connections store no credential. The calling target signs a short-lived token for the service principal with the key it already holds; the issuer gains service-principal signing. External connections hold the vendor's credential in the control plane for as long as the vendor's policy says; the credential authenticates the integration user, and per-operator attribution is the application's to carry.
- Local device connections (serial, Modbus, OPC UA) authenticate the target, not a user; their secrets live in the target's key file, never the cloud.

### Transport and exchange

A connection has one transport and one exchange shape. Two connections between the same cloud and the same box are normal: one for a request interface, one for events.

| connection | transport | exchange | offline box |
| --- | --- | --- | --- |
| `mes-sync` | http: the box dials out, the cloud calls down the held connection | `request`: the caller waits, bounded timeout, result returned | refusal; the caller retries or routes elsewhere |
| `mes-events` | nats | `event`: send, durable, no wait | queued; delivered when the box returns |

- `transport` is a parameter of the connection, as `tcp` vs `rtu` is for Modbus. The `wamn` integration is designed for nats, http (websocket) and mqtt; the first build is nats leaf only, and http and mqtt are added when a site needs them. A local target always dials out; no transport needs an inbound port on the plant network.
- `exchange = request | event`. The caller gets the shape from the binding it names; awaiting an `event` connection is a host refusal. A new `wamn` connection defaults to `event`; `request` is chosen where the caller accepts the offline refusal.
- The cloud application's connection to a box application and that application's connection back are the two ends of one link; each row names its transport and they must agree. Apply refuses a mismatch.
- Platform services (release, bundle pull, grants, settings and role rows reaching a local target) ride a connection the platform owns: principal `platform`, integration `wamn`, transport nats, exchange `event`, one row per target, created with the target. Same table, same connector.

### Connector versions

The environment pins the connector digest. A connector is its own artifact and a dependency of the application; a bump is an edit to one `environment.k` line and an apply — never a connection row, a platform image or a release. This is the target: today every connector is native code compiled into the host binary and a connector change is a platform release. The runtime work that makes a connector loadable is the first assumption below; nothing in this section is filed before it is verified.

- `integration` is the catalog: one row per connector code, its `form`, and the digests published for it. `connection` carries no digest.
- `form = host | wasm | native`. `host`: compiled into the host and versioned with it — the capabilities every application uses (postgres, nats, http, blobstore, logging) and, on a local target, the raw hardware the box exposes (serial, gpio). `wasm`: a component admitted under a connector class, pulled by digest like an application. `native`: a process beside the host serving the same connector interface over a local socket, for a vendor binary that has no other form.
- One connector interface for `wasm` and `native`; connection, binding and target tables never know the form. A `host` capability is pinned as `host` and carries the host's digest.
- Pinning per environment is the one rule for every form, the same rule packages follow; per org was rejected because it forbids a canary.
- A bump restarts the connector inside the application: bound operations see a read gap and queued writes for the restart. A `native` restart cannot take the host down.

The process, for one of twenty connectors used by many environments:

```
apps/platform/no-std/modbus/                 # one crate per connector in the platform repo
$ wamn integration build modbus      # local build; the digest is a function of the source bytes
$ wamn integration publish modbus    # push to the registry; the catalog row gains the digest; nothing runs it yet
```

```kcl
# environments/plant7-box7.k — canary one environment
environment = {
    target = "box-7"
    applications = {
        wms = { connectors = { modbus = "sha256:9f3e…", printer = "sha256:…", disk = "host", wamn = "host" } }   # modbus was sha256:4a1c…
    }
}
```

```
$ wamn env apply environments/plant-7-dev.k   # the target pulls one artifact and restarts one connector in one application
```

```kcl
# environments/defaults.k — then every environment that inherits the default
EnvironmentDefaults = { connectors.modbus = "sha256:9f3e…" }
```

```
$ wamn env apply environments/   # every environment inheriting the default moves; overrides stay; cloud environments included
```

- Rollback is the reverse edit and the same apply. No verb, no flag, no version number.
- Bundle format, pull and restart mechanics belong to the target bundle design, not here.

## Roles

A role is per application, and an application is the composed unit. A user may hold several roles on an application and has exactly one active per application per session, chosen explicitly; permissions are never rolled up across held roles. This amends what runs today, where a request is validated against every role the token carries; the token's role claim becomes one role per application and the platform UI gains the picker.

Intent, not yet a ruling: a project has at most one interactive application — the one people use, composed from packages. Any other application in the project is a service: a sync job, a local program on a box. Services hold service principals, not user roles. Under that intent "roles per application" and "roles per project" are one set, and the shared database exists so a service writes rows the interactive application reads as relations.

- Packages composed into one application (wms + mes, an overlay) are one application: one database, one ACL, one operation namespace (`wms.receive`, `mes.start_operation`). A "warehouse operator" role is one set of permissions over every prefix. One login, one role.
- An overlay's operations are the application's operations and are granted like any other. The entry's permission suffices for the base steps it calls inside the same transaction; what it reaches is review information, never a second grant.
- The session token carries the active role per application as a map `application → role`. Switching the role in one application reissues the token with that entry changed. An application with no entry refuses until a role is chosen; a user holding one role there gets it without a choice.
- Separate applications (separate lifecycle, reached through a `wamn` connection) each have their own entry. A call into another application runs as the connection's service principal, never as the user.

## Settings

Display and view settings are one table with a scope column, resolved from least to most specific, with a lock to stop overrides. It lives in each project environment's database beside the role rows: environments are isolated, so a setting in `dev` never reaches `prod`. No application statement reads it; the UI reads it through platform routes.

```
setting
  scope_type  text    -- org | project | application | role | user
  scope_id    uuid
  key         text    -- platform-declared set
  value       jsonb
  locked      boolean not null default false
  UQ (scope_type, scope_id, key)
```

- Resolution: application declaration → org → project → application → role → user. Each level overrides the one before. A role is per application, so role rows are per application by construction; with one active role per application there is no tie.
- The base of the hierarchy is not a row: the application's own declaration (default columns, sort, filters in the table definition) and platform defaults in code. A `setting` row only overrides them.
- Lock: a locked row wins over every more specific scope for its key; a write at a more specific scope is refused while the lock holds; existing more specific rows are ignored, not deleted, so unlocking restores them.
- Keys are a closed set declared by the platform: `display.theme`, `display.density`, `format.date`, `format.time`, `format.number`, `locale`, `timezone`, `calendar.first_weekday`, `table.<table id>.layout`, `table.<table id>.view.<name>`, and the offline profile keys `sync.offline_profile`, `sync.pending_max_age`, `sync.clear_on_sign_out` ([wamn-sync, Offline profile](wamn-sync.md#offline-profile)). An undeclared key is a write refusal.
- Views are rows, not a table: `table.<id>.view.<name>` at user scope is a personal view, at role scope a shared default for that role, at application scope a shipped one. A separate `view` table is warranted only if views need what scope cannot express — ownership transfer or a publish step — and that has no consumer yet.
- `org` and `project` scope rows are set per environment: the org and project settings screens carry an environment picker, and a value set in one environment reaches no other. There is no fan-out and no drift detection; a dev environment that looks different from prod is intended. The admin-hierarchy path only grants the org or project admin the right to write those rows in each environment.
- Copying a role to another environment copies its role-scope settings with it; the copy route does that. An environment copy carries the whole table. User-scope rows are per environment like every other: a user's `prod` layout is not their `dev` layout.
- Application data about a user (my open orders, my shift) is an application table, never a setting. A value an application reads in SQL (default facility) is application configuration, never a setting.

## Environment copy

A copy is a copy: the whole database into a fresh one, `number_counter`, `setting` and role rows included. There is no flag. A fresh numbering after a copy is `set_counter` on the new environment.

Moving dev to prod is not a copy. Schema and new tables ride the release. Rows that are part of the application rather than its data — lookup codes, `*_type` rows, a default role — are declared in the package as seed data and upserted by code at env apply, so every environment gets them from the same declaration. Anything else is data and moves by import if it moves at all.

## Invariants

Each rule has one test that runs in the platform test set.

| invariant | test |
| --- | --- |
| application role: `SELECT` on the listed tables (plus `role`, `user_role`, `permission` and `environment`), `EXECUTE` on `bump_counter`, `assign_counter` and `set_counter`, and `platform.write_log`, the engine's claim record, keeps its existing grants, nothing else; no history table grant; no `credential`, `target` or `target_application` table exists in an application database | query `information_schema.role_table_grants` and `routine_privileges` per database; any other row fails; `information_schema.tables` shows none of the three |
| no manifest model names a `platform.*` table | build refusal; fixture with such a model is refused |
| no write statement, command claim or history trigger targets a declared relation | build refusal; fixture with an `UPDATE platform.user` is refused |
| a set below a live number is caught by the model's `UQ` | records to 17; set 10 → allowed; the insert that renders 10 is refused by the `UQ`; `set_counter` reads no application table |
| every bound connection has a pinned connector | `environment.k` binds a modbus connection with no `connectors.modbus` line → apply refuses |
| no network call inside a database transaction | a command opens `begin-operation`, then calls `file.put` or any bound connection → host refusal; the same call before `begin-operation` → allowed |
| a target runs only what was applied to it | a target's release reports a bundle with no `target_application` row → flagged, not served |
| every application user FK resolves | FK to `platform.user`; `user` holds every org user |
| a `file` row exists only for a stored blob | application INSERT on `file` is refused; `file.put` leaves one row whose sha256 matches the blob; an unreferenced row is still there a month later; an aged staging object is gone; a live object stays regardless of age; a committed row with only staging bytes repairs on read; disk eviction belongs to C10 |
| exposure is enforced per file | application: session with `file.get` → 200, anonymous → 401, session without the grant → 403, another application never serves it; private: fetch by id → 403, signed url from the operation → 200, same url after expiry → 403; public: plain url with no session → 200, after unexpose → origin 404; signed lifetime above 15 minutes is capped; public on a store with no cdn\_host → refused |
| one active role per application per session | user holds A and B on one application, B-only operation, active A → 403; switch to B → 200; another application's entry unchanged |
| an overlay entry needs no grant on the base steps it calls | role granted the overlay operation only; the operation commits; the base step called directly → 403 |
| settings resolve least to most specific | org and user rows for one key; user value applies |
| a lock stops overrides without deleting them | org row locked: user write refused, pre-existing user row ignored; unlock: user row applies |
| setting keys are a closed set | write with an undeclared key is refused |
| settings never cross environments | set in `dev`; read in `prod` shows the declaration default |
| a role copy carries its settings | copy role to another environment; its role-scope rows exist there with the new role id |
| an environment copy carries counters | copy an environment with records numbered to 17; the first insert in the copy renders 17, not 1 |
| seed rows come from the package, idempotently | apply twice; seed rows exist once, by code; an edited seed row is overwritten, an added row is kept |
| an environment runs on one target | environment.k names one target; a second target line is refused at apply; the same application on a box and in the cloud shows two environments with two target\_application rows |
| a local target accepts only model-free packages | environment.k with target = local and a package declaring a model → build refusal |
| the application role never writes number\_counter | Postgres: role\_table\_grants shows no UPDATE, and next\_number still advances; SQLite: an application statement writing platform.number\_counter → admission refusal |
| a box issues sessions offline | cloud link down; user with a role on the box logs in on the box → session signed by the box key and accepted by its operations; user without a role → refused |

## Assumptions

| assumption | verify | if false |
| --- | --- | --- |
| the control host already writes rows into every project database on the admin-hierarchy path, and that path can carry `platform.*` rows | the write path is a function the new tables call with no new transport | the `platform` schema needs its own write path before any table lands |
| the host's connection plugin, not the application, opens connections and reads credentials from the control plane | no application statement can compile against a credential; the plugin has a control-plane handle | credentials move to a secret manager; the plugin reads references |
| the issuer can sign a short-lived token for a service principal, not only a logged-in user | the target's connection plugin signs with the target key and the target application verifies it as it verifies a user token | a long-lived service token stored in `credential`, rotated by hand, until the issuer signs for services |
| transaction claims can carry `app.application` beside the existing `app.user_id`, `app.role` and `app.operation` | one more `set_config` in the claim statement; the claim is for logging only, and `next_number` takes the application from host state (wamn-9e4ks) | `next_number` takes the application as a parameter and a shared database trusts its caller; acceptable only while a project database serves one application |
| the `wamn:postgres` plugin can host the renderer and run the counter SQL inside the operation transaction, on Postgres through `bump_counter` and on SQLite directly | `next_number` as a plugin call renders the same text on both engines for the same pattern and counters; the Postgres app role shows no `UPDATE` grant on `number_counter` | the renderer becomes a pgrx extension plus a registered SQLite function: one crate, two loaders, a custom Postgres image, no managed Postgres for that database |
| one `number_counter` row per distinct key is acceptable growth | count rows per sequence after a month of batches and periods | a retention rule on `per=` rows older than their records' retention; nothing else |
| row-lock serialization of creates per counter and key is acceptable | measure creates per second per document type with the Postgres benchmark replay | switch that sequence to a Postgres sequence and accept gaps, per sequence, not globally |
| `platform.user` carrying every org user is acceptable fan-out | count user writes per org per day against project count | materialize on first grant and store `updated_by` as a bare uuid without FK |
| after A5 the host can refuse a blobstore or connection call while an operation transaction is open | the plugins see the operation state `begin-operation` sets, as `run` does | the rule holds by generator refusal of such calls in an operation body instead; weaker, until the host test lands |
| a connector can be a dependency of an application: admission has a connector class that grants a component sockets and refuses postgres, and an application bundle carries its connectors by digest | one wasm connector pinned in `environment.k`, pulled with the application onto a local target, started and restarted with the host running; sockets granted to it and still refused to application components | every connector is `form = host`; the `connectors` line in `environment.k` is always `host`; a connector bump is a platform release |
| a bundle is one application, so a target serving two applications runs two bundles | the bundle names one manifest | `target_application` keys on bundle, not application, and the bundle lists its applications |
| an http transport can hold a box-initiated connection the cloud calls down, with a result back | a request connection round-trips to a named application on a local target within its timeout | only the event exchange reaches a box until it does; request connections stay cloud-to-cloud |
| the session token claim can carry a per-application role map | the token verifier accepts a map claim on cloud and edge | one token per application, shell holds several |
| a project has at most one interactive application | no project needs two applications a person logs into | the token map is exercised within one project and roles stay per application; nothing in the tables changes |
| the box can issue sessions: its key signs session tokens its own operations accept, against a credential projection for users with roles there | offline login on a box succeeds and an operation accepts the token; the same token is refused by the cloud | offline operator use on a box is bounded by the 930 s token lifetime; the operator UI on the box is online-only |
| a projection can be fed from the source environment's change stream over the platform connection into a read-only SQLite table | one projected model on one box stays within a stated lag while online and serves while offline | projections stay `feed = "operation"` only |

## Implementation

### Rules

C3 requires the ruling 18 kind test unless the owner rules otherwise.

See [standing repository rules](../../CLAUDE.md#standing-repository-rules), [platform version procedure](../operations/building.md#platform-version-procedure), and [cloud prerequisites](../operations/gcp.md#cloud-credentials-and-prerequisites). Plus:

- The spec's invariants table is the test set. Each epic lands the tests for the invariants it makes true; a test asserts a fact, never a stored literal.
- Receiving and WMS change only where the spec makes today's shape non-conforming (`number =`, `role = "actor"`, `relations`, connection bindings). Nothing else in them moves.
- One agent per lane that touches `crates/control`. Epics C1–C4 run in sequence in the integrations agent; the UI agent and platform-install agent keep their own epics.
- Platform version bumps follow the [platform version procedure](../operations/building.md#platform-version-procedure). C1 and C2 are host-plugin changes, so they are image bumps; plan the build with the cloud path.

### Architecture references

See [numbering](../architecture/data-access.md#numbering) and [integration and connection schema](integration-schema.md).

The application role reads `setting`, and `scope_id` is text. These differences still need the owner decisions below.

### Epics

Tag = the spec's dependency tag. Owner = which running agent takes it, in its queue order.

| # | Epic | Tag | Owner | Done when |
| --- | --- | --- | --- | --- |
| C3 | **Connections, bindings and targets.** Run the ruling 18 kind test. `wamn` connections, with service-token signing, are `wamn-ym204`. Target application records and the platform connection are `wamn-ztxg2`. | now | integrations agent | The REST connection from `rest_connector` to `rest_consumer_fixture` works in the WMS dev document with no values in `environment.k`. |
| C4 | **Files.** `platform.file`, pinned connection generations, native streaming `file.put/get/delete/expose`, grants, exposure, signed URLs, and staging repair on GCS and S3. Disk stores and eviction belong to C10. | now | integrations agent, after C3.5 | File invariants pass, including origin 404 after unexpose and staging-only age cleanup. |
| C5 | **Admin screens.** Control: project integrations and environment connections with write-only credentials, sequences (`set_counter`), settings (org/project scope with the environment picker). Application: settings at application/role/user scope, views as settings rows. Settings table semantics (resolution, lock, closed key set, copy with role). | now | platform UI agent, after M6 | Settings invariants pass. An admin creates an integration and connection, sets a counter and locks a setting from the browser. The server refuses what the UI hides. |
| C6 | **Active role.** Token claim `application → role` map; the auth test uses the active entry only; role picker in the shell; reissue on switch; a user with one role gets it without a choice. | now (ruled) | platform UI agent, after M4, before C5 if M4 finishes first | Invariant "one active role per application per session" passes; existing M2/M4 flows unchanged for single-role users. |
| C7 | **Environment copy and seed rows.** `environment copy` carries `number_counter`, `setting`, roles; package seed data upserted at env apply. | now | platform-install agent, after E5 | Copy and seed invariants pass on kind. |
| C8 | **Transaction refusal.** Host refuses storage and connection calls while `begin-operation` is open. | after A5 | perf-arch agent (A5 owner) | The network-in-transaction invariant passes. |
| C9 | **Loadable connectors.** Spec assumption 11 spike first (connector class in admission, sockets granted to a connector component and refused to applications, bundle carries connectors). Then `integration.form`, `connectors` pins in `environment.k`, `wamn integration build/publish`, `target_application` digests. | after loadable connectors | integrations agent, after C4 | One wasm connector pinned per environment, bumped by an `environment.k` edit, no platform release. |
| C10 | **Local environments.** SQLite platform tables in the plugin, model-free packages on `local` targets, outbox, offline sessions, `feed = "operation"` projections. CDC projections last. | after SQLite platform functions | edge owner, after the edge plan's next epic | A box runs `wms.station` offline, numbers a record gaplessly, and the cloud commits its events on reconnect. |

Nothing in C1–C4 blocks the UI or install epics; C5 needs C1–C3 merged.

### C2 numbering

The owner ruled that no kind run follows C2.4. Its rulings replace the C2 rulings below where they differ:

- Sequences and records are independent. A package declares a sequence and its default pattern, and the admin owns the pattern after that.
- The build refusals "sequence with no field and no caller" and "a key must be a field of every model using the sequence" are removed (C2.4.2). A model's numbered field must have a field for each key its pattern names (C2.4.3).
- `set_counter` reads and writes counter rows only. It renders no number and scans no application table. The model's `UQ` catches a set that gives a number a record already has (C2.4.4).
- Counter authority, case 1 (C2.4.6, recorded on `wamn-83eth.13`): no application surface runs arbitrary SQL. An application guest runs only the statements its package declares, and admission denies the raw `wamn:postgres/client` interface to applications. Component admission parses each declared statement with `pg_query`, which is Postgres's own grammar. It refuses a counter function call, `DO`, dynamic `EXECUTE`, `query_to_xml*`, `cursor_to_xml*` and `U&` identifiers. `sqlparser` was refused, because a grammar difference is the bypass that the test closes.
- Package backfill SQL is outside the counter test, because it runs as `wamn_db_owner`, which already holds more authority. Who may author backfill SQL is `wamn-o1hwr`.
- Migration 0031 stays rewritten in place, because no wamn-dev database ran it.
- `CounterSetError` and `numbering::Failure` become error structs with a kind (C2.4.18).
- No sleep inside the tokio test, no NATS port race, and the numbering live tests reuse `test_database` (C2.4.21).

Rulings for C2 as filed:

- `next-number(name, keys) -> text` and `peek-number(name, keys) -> text` are two added functions on `wamn:postgres/statements`, with no version bump. `peek` is generated from `peek-number` (owner ruling 2026-10-23). It renders the current value with no lock and no advance. The host refuses both outside an open operation transaction. The renderer lives in the plugin. The three platform functions, `bump_counter`, `assign_counter` and `set_counter`, are the only writers of `number_counter` (owner rulings 2026-10-23).
- `{date:FMT}` uses the environment's `timezone` setting row at application scope when present, else UTC. Settings resolution across scopes is C5 (admin screens).
- `targets` is declared and validated at build: a package with models is `cloud` only. The apply-side target refusal is C10 (local environments).
- C2.4 allows a sequence with no field and no caller. A sequence key need not belong to every model that uses the sequence (owner ruling 2026-10-24).
- `set_counter` is a host route granted per role: a new `HostRouteAuthority` variant in `host_route.rs`, added in place with no `wamn.json` version bump, and `permission.grant` accepts it. It runs in the operation transaction and writes through `platform.set_counter` (SECURITY DEFINER, `EXECUTE` for the application role; the route resolves the sequence by the application it validated against the release and the sequence name) under the counter row lock. The engine write log and the counter row's `updated_by` record the actor (owner ruling 2026-10-23). It never touches a `per=` mapping. C2.4 removed the dry render: the set reads and writes counter rows only, and a model's `UQ` catches a live number (owner ruling 2026-10-24).

Out of C2: settings resolution, the `set_counter` UI, the SQLite renderer, connections, seed rows.

### C3 connections, bindings and targets

The test budget, the stops and the closeout are the same as for C2.

Owner rulings of 2026-10-25 on the C3 scoping questions. They replace the text below where they differ:

1. C3.1 removes `tenant_id` only from `role`, `user_role`, `permission`, `api_key` and `environment`, with their history tables, policies and indexes. The shared control, catalog and run tables keep the environment key (`org--project--env`). `wamn-8yflb` renames that key to `env_key`, including its wire fields. The separate `environment` field keeps the short name.
2. The event field is `env_key`, and credential hashes frame `"env_key"`. No format number changes.
3. See [integration and connection schema](integration-schema.md).
4. See [integration and connection schema](integration-schema.md).
5. See [integration and connection schema](integration-schema.md).
6. Both principal kinds stay, and nothing migrates. The application principal runs schedules and integrations, as ruled earlier. The connection principal is only for inbound calls, and it holds the roles that an admin grants it.
8. A failed auth deletion is logged, and the TTL limits the stale fact. A durable retry is `wamn-3gqi1`.
9. See [integration and connection schema](integration-schema.md).
10. See [integration and connection schema](integration-schema.md).
12. See [integration and connection schema](integration-schema.md).
13. An exclusive medium allows one binding per target, readers included, because a physical port has one owner.
14. See [integration and connection schema](integration-schema.md).
15. C3 creates only the target identity. `target_application` and the target's platform connection are `wamn-ztxg2`.
16. C3 builds inbound authentication only for the principals of external connections. The `wamn` connection records, peer external identity, inbound authentication, and request and event capability belong to `wamn-ym204`. The platform catalog gains a `wamn` type and version when that implementation is available.
18. The final kind test is the REST connection from `rest_connector` to `rest_consumer_fixture` in the WMS dev document.

Owner rulings made while C3 was built:

22. A changed package records its new digest under the same version. C3 bumps no package version.
Rules:

- No table or function that holds or returns a credential is granted to `wamn_app`.
- Every write to a principal or a grant also deletes the matching `auth_changes` key.
- A service principal never holds a user role through the active-role logic, and a test asserts it.

The ruling 18 kind test must prove the REST connection from `rest_connector` to `rest_consumer_fixture` in the WMS dev document, with no values in `environment.k`.

### Open decisions

The docs audit ([docs-audit.md](../history/README.md#docs-audit), 2026-10-24) lists the places where this plan and the built code disagree. Each waits for an owner ruling before the spec text changes.

- Item 3: `relations` in `wamn.k`. The manifest derives relations and has no such key.
- Item 4: the `user` shape. The code has `display_name` and `status`, not `name` and `active`.
- Item 5: `setting` is readable by the application role, and `scope_id` is text.
- Item 6: the counter route is `number_counter.set`. It refuses any `per=` counter, and the write log has no actor column.
- Item 7: `peek-number` returns the last number issued, with no lock. The spec text describes a preview of the next number.
- Item 9: a synced model numbering its key field (wamn-sync rule).
- Items 10 to 14: connector pins against node pins, service principals in workflows, org-wide name uniqueness, the A5 tags and the target refusal, and the terms.
- `wamn-o1hwr`: who may author package backfill SQL.

Deferred lifecycle proposal from the retired deployment plan.

Not now. When continuous drift detection or a pull-based deploy is wanted, add Flux and point a HelmRelease at the `release` chart with the values file committed to a repository; the `Database` CR becomes a manifest under a Flux Kustomization. The environment documents are already files, so B is: commit them, and `apply` commits the rendered values in step 6 instead of running `helm upgrade`. How writes are serialized and how convergence is awaited once Git owns the desired state is deferred with B; a process-local advisory lock cannot order Git writers, so option A's lock is not promised there. Nothing in option A is redone: the documents, the chart, the values and the readers are what B consumes. Two facts shape the choice. The chart version selects the image set (R15), so it needs no cluster read under Flux or Argo CD; Flux keeps standard Helm release storage and Argo CD does not, which matters only to `rollback`'s read of `helm history` (R8). A hand edit of the committed values bypasses the stage and the predicate exactly as a hand `helm upgrade` does today; the rule for B is that only `apply` writes that path, and nothing in the cluster enforces it. If a guard is wanted later, the host refusing to start when the installed stage is behind what its release needs is the place; not Postgres.

The proposal has no owning bead in the retired plan.

`provision-project-env` stays outside `env apply` (R21).
The session-target Secret goes to the identity namespace, which no `env apply` input names.
The gates use local port-forward files for login.
The mint-pat Job, the Rust identity journey, and three live tests call `provision-project-env`.
The work stays open and unscheduled (`wamn-7yse3`, reason on `wamn-r3qqf.3`).

### Out of this plan

- Choosing which connectors to build: [integration history](../history/README.md#integrations) candidate list, owner's pick, after C3.
- The edge plan's bundle format, pull and restart: C9 depends on it, does not define it.
- OIDC, SAML: identity plan.
