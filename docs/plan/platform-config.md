# Platform configuration in applications

## Mechanism

One mechanism for everything the platform provides to an application: the platform owns schema `platform` in each project database (today's `app_system`, renamed), the control host writes it, the application reads it. The application role has `SELECT` on the listed tables (the tables below, plus roles, permissions and `environment`), `EXECUTE` on `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter`, and no other grant; `platform.write_log`, the engine's claim record, keeps its existing grants. No history table and no `schema_migrations` is readable. Platform routes that change platform rows (`file.put`, `set_counter`, settings) are operations in the application's ACL, granted per role like any other.

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
- Four words, one meaning each: an **integration** is connector code; a **connection** is a configured instance of an integration — communication, never provenance; an external system is the persistent identity of a system outside wamn (one NetSuite account, one plant's MES) with the configuration its connections share; a **target** is a runtime wamn deploys an application to — the cloud cluster or a local box. A target is deployment configuration in the control plane, never a row an application reads.
- Connectors deploy per application: a connector is a dependency of the application, pinned per environment like a package, and runs inside that application's isolation on its target.
- Three levels, each already present in the platform: org, project, application per environment. Environments are isolated: nothing in one environment's database reaches another.
- A project database holds only the org rows that project uses: every user and the connections it has enabled.
- No network call inside a database transaction. Once `begin-operation` is open, a storage put, a connection call or anything that waits on the network is a host refusal; inside the transaction there is only SQL. Storage and connections are called before the transaction opens, and their results enter it as values.
- An **operation** is one exported entry point of an application (`get`, `query`, `list`, `command`): the unit the ACL grants and a route calls. A **transaction** is a window a command may open inside its run with `begin-operation` … `commit`; reads usually open none. Code before and after the window may call the network; code inside may not.

## Tables

Every table below lives in `platform` in the project environment's database. Credentials and targets are control-plane data and are never materialized into an application database: the application cannot read what is not there.

One database per (org, project, environment) is the org boundary. No table below has a tenant column, and no row policy or tenant key index separates its rows; each key is the table's own columns, such as `id` or `(scope_type, scope_id, key)`, and a foreign key is one column (owner ruling 2026-10-23).

| level | table | columns | written by | application reads |
| --- | --- | --- | --- | --- |
| org | `user` | id, name, email, type (`user`, `service`, `platform`), active | platform routes | yes |
| org | `external_system` | id, integration (catalog code), name (unique per org), parameters (jsonb: shared account id, realm, base host), status | platform routes | yes |
| org | `connection` | id, integration (catalog code), external\_system\_id (nullable FK to `external_system`), name (unique per org), transport, exchange (`request`, `event`), parameters (jsonb), status | platform routes | yes |
| org | `file` | id, connection\_id, name, media\_type, size, sha256, blob key, exposure (`application`, `private`, `public`), url, width, height, created\_by, created\_at | the host, in the `file.put` route, in its own commit before any application operation | yes; no INSERT |
| org / project / environment | `integration_allow` | scope\_type, scope\_id, integration | platform routes | yes |
| project | `project_connection` | project\_id, connection\_id | platform routes | yes |
| application, per env | `number_sequence` | id, application, name, pattern | created at env apply from the package default; pattern edited via platform routes | yes |
| application, per env | `number_counter` | sequence\_id, counter, key\_value, value; PK (sequence\_id, counter, key\_value) | `next_number` inside the operation transaction; `set_counter` route | yes; written only through `bump_counter`, `assign_counter` and `set_counter` |
| application, per env | `setting` | scope\_type, scope\_id, key, value (jsonb), locked | platform routes, per environment | no (UI reads via routes) |
| application, per env | `role`, `user_role`, `permission` | as today | as today | yes |

Control plane only, reached through platform routes and the host:

| table | columns | note |
| --- | --- | --- |
| `credential` | id, connection\_id, secret or secret-manager reference | read by the host's connection plugin; no application grant because no application copy |
| `target` | id, name, type (`local`, `cloud`), status, last\_seen\_at | identity and check-in of a runtime; named in `environment.k`; the cloud cluster is one row |
| `target_application` | target\_id, application, environment, bundle\_digest, status; PK (target\_id, application, environment) | what each target runs, written by env apply and confirmed at check-in; null digest on the cloud target, which runs a release |

- `user` is a projection: identity in the system database is authoritative, and the control host writes the projection (as it writes `app_system.users` today) and propagates deactivation on the admin-hierarchy path. It holds every org user, not only users with a role on the project, so `created_by` / `updated_by` FKs never dangle. Deactivation is `active = false`; rows are never deleted.
- Application rows that name a user (`created_by`, `updated_by`, `employee.user_id`) carry a uuid FK to `platform.user`. The host fills `created_by` and `updated_by` from the operation caller; the payload cannot supply them.
- `integration` is platform catalog data (code, form, digests), not a `platform.*` table: an application binds a connection, never an integration. `connection.integration` carries the catalog code for display.
- `connection.name` is unique per org and is what `environment.k` binds; renaming a connection is refused while any applied environment binds it. `connection.external_system_id` groups the connections that reach one system; it is null for a connection with no system behind it (a serial scale, a GPIO pin). A connection says nothing about where it runs: its connector runs inside each application that binds it, on that application's target.
- `integration_allow` narrows top down: the org allows a set of integrations, a project a subset, an environment a subset of that. A package declaring a connector not allowed at its environment is an apply refusal. Allowed is about code; enabled (`project_connection`) is about a configured instance; both must pass.
- `file` is the only file table. An image is a file with an image media type; `width` and `height` are nullable. `connection_id` names the store the bytes are in: a `gcs` or `s3` bucket, or a `disk` store on a local target. The application links `file_id` from its own table (which record, which purpose). No generic attachment-by-(table, id) table.
- Storing is two short calls: `file.put(binding, bytes)` streams the bytes to the store the binding names, inserts the row in its own commit and returns `file_id`; the application operation then carries `file_id` like any reference. No transaction waits on the store. A store is a connection with a store integration (`s3`, `gcs`, `disk`), declared and bound like any other with `mode = write`.
- File routes are operations in the application's ACL: `file.put`, `file.get`, `file.delete`, `file.expose`, granted per role. `file.put` also needs a `mode = write` binding to its target store. `file.get` on a `private` file is a 403 whatever the role; only an operation that returns a signed url reaches it.
- A `file` row is a file. A row nothing references is kept; deleting it is the user's job through the normal delete. No sweep of unreferenced rows: a file uploaded on purpose and linked to nothing is a normal file, and a list that hides unlinked files is a screen filter, not a deletion policy. The only garbage is a blob with no row (a put that failed before its commit), and the store's gc removes those by age.
- `exposure` is per file: `application` (default; any user with a role on the application, stable `/api/<app>/files/<id>` behind the session), `private` (only an operation that returns a short-lived signed url; so the roles holding that operation), `public` (anyone with the link, no login). `url` is stored only for `public`, so old links survive a cdn change.
- A cdn is a way to expose files already in a store, not a place to put them: one hostname fronting the store's `public/` prefix, a parameter on the store connection (`cdn_host`). Exposing a file copies the object to `public/<key>` server-side and sets `url`; unexposing deletes the public object and clears it. A store with no `cdn_host` refuses `public`.
- A local `disk` store has `max_bytes` and `max_age` parameters: a box has a disk, so retention is the admin's explicit choice on that row, and an evicted blob takes its `file` row with it — the one place a row is removed by the platform. The box serves its own files; nothing on it is `public`. Forwarding a file to the cloud is an application decision carried over a `wamn` connection (a new row in a cloud store, same `sha256`); the platform never forwards on its own.
- `external_system` holds what every connection to one system shares (account id, realm, base host) once; a connection adds only what differs (endpoint, transport, credential). A connection with an `external_system_id` has the system's `integration`; a mismatch is a route refusal. `external_system.name` is unique per org and stable while any connection or record references it. Credentials stay on connections: one system may have a read-only and a write connection. Enablement stays on connections (`project_connection`); enabling a whole system for a project is a route that enables each of its connections, not a second table.
- Record provenance is the application's: a record that came from outside carries `external_system_id` (FK to the relation) and `external_id` (the record's identity in that system's namespace), never a connection id, with `UQ (external_system_id, external_id)` on the model. Which connection imported it is a fact about the run, not about the record. For a `wamn` connection the external system is the peer environment, one row per peer, created with the connection; it gives `project/environment/application` one home.

## Declaration in wamn.k

A package declares each platform thing it uses by name. The declaration is the only place a default lives.

```kcl
# wamn.k
package = {
    targets   = ["cloud"]
    relations = { user = "platform.user", connection = "platform.connection", external_system = "platform.external_system" }
    sequences = {
        purchase_order = { pattern = "P-{seq:po,width=8}" }
        pick_label     = { pattern = "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}" }
    }
    connections = {
        documents = { integration = "s3",     mode = "write" }
        line3     = { integration = "modbus", mode = "read" }
        mes       = { integration = "wamn",   mode = "write" }
    }
    models = {
        purchase_order = {
            fields = {
                id         = { type = "uuid", role = "key" }
                number     = { type = "text", number = "purchase_order" }
                updated_by = { type = "uuid", role = "actor" }
            }
        }
    }
}
```

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
- `sequences`: name plus default pattern. Env apply creates the `number_sequence` row when absent and never overwrites an existing pattern. A key named by `per=` or `reset=` must be a field of every model that uses the sequence, or a counter in the same pattern, or `date:FMT`; otherwise a build refusal.
- `number = "<sequence>"` on a field: the generator fills it by calling `next_number` on insert with exactly the fields the pattern names; a write statement that supplies the field is a build refusal. On the key field: allowed, with the limits under Numbering.
- `role = "actor"` on a field: the host fills it from the operation caller. The generated insert fills every actor field from `app.user_id`. The generated update fills `updated_by` from `app.user_id` and never touches `created_by`. An update that changes no field leaves `updated_by` as it leaves the revision. `actor` joins `key`, `reference`, `revision` and `value` as a column role.
- `connections`: the names a package binds, each with the integration it expects and `mode = read | write`. A file store is one of them. `environment.k` maps each name to a connection name; apply refuses an unbound name, a connection the project has not enabled, an integration that differs from the declared one, or an integration not allowed at the environment.
- `connectors` in `environment.k`: the digest of each connector the application's connections need; `host` for a `form = host` capability. Apply refuses a connection whose integration has no pin. A bump is an edit to this line and an apply, the same as a package bump.
- A `wamn` connection's external system is the peer environment; its `parameters` carry `project/environment/application`. The connection adds transport and exchange only.
- A sequence declared by a package is also callable from its commands (`sequences::<name>::next`); a sequence with no field and no caller is a build refusal: nothing would use it.

## Numbering

Document numbers are gapless, rendered once in the host, and advanced inside the operation transaction through counters the application cannot write. Every counter is keyed by a value; the key decides when it resets or whether a key gets the same number again.

The renderer is Rust in the `wamn:postgres` plugin, not a SQL function: the plugin already owns the session, admits every statement and will run on SQLite for local targets, so one implementation serves both engines with no second parser and no custom Postgres image. `next_number` is a plugin call inside the operation transaction:

```
next_number(name, record) -> text        // plugin call; application from host state, never from a claim
platform.bump_counter(sequence, counter, key) -> bigint   // Postgres only: SECURITY DEFINER, lock-insert-increment, no pattern, no parsing
platform.assign_counter(sequence, counter, key) -> bigint // Postgres only: SECURITY DEFINER, per= mapping: return it, or allocate from '' and insert it
platform.set_counter(sequence, counter, key, value)       // Postgres only: SECURITY DEFINER, the set_counter route's write, under the row lock
```

On Postgres the three platform functions, `bump_counter`, `assign_counter` and `set_counter`, are the only writers of `number_counter`, so the application role keeps no `UPDATE` on `number_counter`. Each runs on the sequence id alone and refuses an id that is not a sequence. None reads the `app.application` claim, because guest SQL can rewrite it. The plugin knows the operation's application from host state. It resolves the application and the sequence name to the sequence id, and it passes that id to the function. The plugin's statement admission refuses guest SQL that names `bump_counter`, `assign_counter` or `set_counter`, or that writes a `platform.*` relation. So only the plugin reaches the functions. SQLite needs the same rule, because it has no roles. The claim stays set, for logging only. The rejected alternative — the renderer as a Postgres function via `pgrx` plus a registered SQLite function — kept the grant invariant but needed a custom Postgres image and ruled out managed Postgres.

### Pattern

Literal text plus tokens. Options are comma-separated, order free.

| token | meaning | example |
| --- | --- | --- |
| `{seq:<counter>}` | one counter, never resets | `17` |
| `{seq:<counter>,reset=<key>}` | advances every call; starts at 1 for each distinct key | per facility, per year |
| `{seq:<counter>,per=<key>}` | one value per distinct key; assigned on first sight, reused after | one number per batch |
| `width=8` or `08` shorthand | zero-padded to width; longer values are not truncated | `00000017` |
| `pad=_` | pad character other than `0` | `____17` |
| `hex` | render in hex | `11` |
| `{field:<name>}` | a field of the record, as text | `DAY` |
| `{date:FMT}` | the renderer's own set, on the transaction time in the environment's `timezone` setting: `YYYY`, `YY`, `MM`, `DD`, `WW` (ISO week), `Q`; anything else is a pattern refusal | `26`, `2610` |

`<key>` is a field of the record being inserted, another counter's name in the same pattern, or `date:FMT`.

```
P-{seq:po,width=8}                                       P-00000017
{field:facility}-PO-{seq:po,reset=facility,width=6}      DAY-PO-000017, CIN-PO-000003
{date:YY}-{seq:po,reset=date:YY}                         yearly reset
B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}
                                                         batch abc → B0001-001, B0001-002; batch def → B0002-001
```

### Function

1. Tokenize the pattern; resolve each key from `record`, from an already-rendered counter, or from the transaction time.
2. For each counter in lock order: a counter keyed by another counter comes after that counter, and independent counters go in name order. The order is fixed for each pattern, so concurrent callers cannot deadlock.
   - `reset=`: `bump_counter(sequence, counter, key_value)` locks the row, inserts at 1 if absent, returns the value and increments.
   - `per=`: look up `(counter, key_value)` with no lock — mappings are immutable, so a hit is safe to render. On a miss, `assign_counter(sequence, counter, key_value)` takes a per-key `pg_advisory_xact_lock`, looks again, and returns the existing mapping or allocates from the `''` allocator and inserts the key row with that value. Two concurrent misses for one key allocate once, so `per=` has no gaps. The first line of a batch pays the allocator lock; every later line pays only its own counter (owner ruling 2026-10-23).
3. Render and return the text. A hex counter renders in upper case (255 is `FF`).

- Rendering, preview, validation and `set_counter` all go through the plugin's one renderer (preview in a rolled-back transaction). No second parser, in SQL or anywhere.
- Name-order locking within one call means two patterns on one sequence cannot deadlock each other. Two operations that insert into two models in opposite order can still deadlock on two sequences; Postgres detects it and one retries. The plugin guarantees no more than that.
- A null key field is a refusal. `per=` on a model's key field is a build refusal (same key would mean same id).
- Uniqueness of the rendered number is the model's `UQ` on `number`; a pattern with no per-record counter fails there on the second insert, never silently.
- Cost: locks hold until commit, so creates of one document type (or one key) serialize for the SQL of one operation. There are no outbound calls inside a transaction, so that is milliseconds. Acceptable at document rates. A Postgres sequence was rejected: non-blocking but gaps on rollback.

### Host-filled and application-filled

- Host-filled: `number = "<sequence>"` on a field. The generated insert calls `next_number` with exactly the fields the pattern names; the generated insert type has no such field, so supplying it is a compile error in the guest and a build refusal in SQL.
- Application-filled: a command calls the sequence itself and writes the result to a plain `text` field with `UQ`. For a label printed before the record exists, a number handed to an external system first, or a batch counter assigned once and written to many records.
- A field is one or the other, never both.

```rust
// generated from sequences.pick_label
let n = sequences::pick_label::next(PickLabelKey { batch_id })?;   // "B0001-003"
sequences::pick_label::peek(PickLabelKey { batch_id })             // current value, no lock, never for assignment
```

- A fetched number advances the counter in the operation transaction: rolled back → never consumed; committed with no record → gone. Gapless means no gap from failure, not every number on a row.

### Example

```kcl
sequences = {
    purchase_order = { pattern = "P-{seq:po,width=8}" }
    pick_label     = { pattern = "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}" }
}
models.pick.fields = {
    id       = { type = "uuid", role = "key" }
    batch_id = { type = "uuid" }
    number   = { type = "text", number = "pick_label" }
}
```

```sql
-- generated pick.insert: two statements in one transaction
-- 1. plugin call: next_number('pick_label', {"batch_id": $2}) -> $number
-- 2.
insert into pick (id, batch_id, number)
values ($1, $2, $number)
returning id, number;
```

```rust
pub fn release_picks(batch_id: Uuid, lines: Vec<PickLine>) -> Result<Vec<Pick>, Refusal> {
    lines.into_iter()
        .map(|l| pick::insert(PickInsert { id: Uuid::new_v4(), batch_id, ..l.into() }))
        .collect()
}
```

The application and the actor are transaction claims the host sets at `begin-operation` (`wamn:operation`) with `set_config`: `app.application`, `app.user_id`. Generated SQL reads `app.user_id` with `current_setting`; nothing is passed as a parameter. Numbering does not read `app.application`: the plugin takes the application from host state.

### Sequence as the record id

Allowed, not default: `number = "<sequence>"` on the key field; the insert fills `id`. The key's counters may not use `reset=` or `per=` (collision); an `int32` key takes a bare `{seq:<counter>}` pattern only. Build refusal on either. Cost: text or int FKs and no renumbering. A uuid `id` with `number` `UQ` gives the same row key in lists without that cost.

### Administration

One platform route, `set_counter(application, sequence, counter, key_value, value)`:

- advance: set higher (go-live at `P-00100000`, skip a block). The next call renders `value` and increments.
- reset: set 1 on that `(counter, key_value)` row; for `per=` the `''` allocator row.
- downward is allowed, with one check: the route renders the next value in a rolled-back transaction and refuses when that number already exists in the model. A reset to 1 on a new period key passes; a set below live records is refused before an operator ever hits it. No flag.
- `per=` mappings are never edited or deleted: that would rewrite history on records that carry them. Correct the records instead.
- a set goes through `bump_counter`'s lock, so it serializes with concurrent inserts; it is a platform-route write and lands in the write log with the actor.
- not provided: renumbering existing records (an application command if wanted); scheduled resets (`reset=date:…` covers calendars; anything else is a workflow).

## Connections

Everything outside an application — a vendor system, a bucket, a PLC, a printer, another wamn application — is reached the same way: a connection, configured once at the org, enabled per project, bound by name per environment, run by the connector the application carries.

- The host's connection plugin opens the connection and reads `credential` from the control plane. No application database holds a credential; there is nothing to grant.
- A connector instance belongs to the application that binds the connection and runs on that application's target. A connection bound by two applications is two instances, each inside its own application's isolation. No connector is shared across applications.
- A physical port has one owner: a connection whose integration is exclusive media (`serial`, `modbus` over rtu, a printer port) may be bound by one application per target; a second binding is an apply refusal. The other application gets the data through a `wamn` connection.
- A machine is not a platform concept. The box next to a CNC is a `target`; "line 3 PLC at 10.0.3.7 unit 1" is a `connection` (integration `modbus`); the CNC is the application's `equipment` row, which links the connection that reaches it (`connection_id`, or a link table if one machine has several). One machine with two protocols is two connections.
- A target serving two applications runs two bundles (`target_application` rows): two components, two grants files, two local databases, one org session key. Wasm isolation between them, as in the cloud.
- Application to application is a connection with integration `wamn`: parameters name the target application, and the caller acts as the connection's service principal — nothing stored. The target application's ACL grants that principal a role; it does not know or care where the caller runs. app1 and app2 in the cloud both reach app3 on a box through their own `wamn` connections.
- An operation calls a `wamn` connection like any other connection, before its transaction: `request` waits with a bounded timeout, `event` does not. Approval rules and other logic that decide a record's status run inside the operation as composed packages; a trigger (cron, an event from a box) invokes an operation the same way a request does.
- The hierarchy is org → project → environment → application: an environment belongs to a project, runs on exactly one target, and applications are deployed into it. An application present in a cloud environment and in a local environment of the same project runs a different package in each (`wms.core` in the cloud, `wms.station` on the box); each environment has its own database, settings and counters, and the two talk through a `wamn` connection like any other pair.
- An integration package holds configuration and logging rows only; domain tables belong to the consuming application.
- An application sees none of this by default: it sees its bindings (`documents`, `line3`, `mes`). `platform.connection` is a readable relation for a screen; where a connection runs and when a target last checked in are control-plane facts, read through platform routes.

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

Every connection has a principal: a `user` row with `type = service`, created with the connection. Inbound calls arrive as that principal; it holds roles on the target application like any user, in the same ACL, chosen in the same role picker. There is no table-level grant and no separate integration-permission surface.

| direction | what checks | where declared |
| --- | --- | --- |
| inbound: the ERP pushes a purchase order | the connection's principal holds a role granting the operation | ACL rows, like a user |
| outbound: the application writes to S3 or calls the ERP | the binding exists with `mode = write` | `wamn.k` `connections`, `environment.k` binding |

- "Connection A may write purchase orders and nothing else" is a role granted `erp.upsert_purchase_order` only. What that operation touches is its effect profile, shown to the admin at grant time; the profile informs the grant and never is the grant.
- A connection that needs to write a table needs an operation that writes it. If none exists, the application author adds one. The application decides what an external system may do by what it exports.
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
- Platform services (check-in, bundle pull, grants, settings and role rows reaching a local target) ride a connection the platform owns: principal `platform`, integration `wamn`, transport nats, exchange `event`, one row per target, created with the target. Same table, same connector.

### Connector versions

The environment pins the connector digest. A connector is its own artifact and a dependency of the application; a bump is an edit to one `environment.k` line and an apply — never a connection row, a platform image or a release. This is the target: today every connector is native code compiled into the host binary and a connector change is a platform release. The runtime work that makes a connector loadable is the first assumption below; nothing in this section is filed before it is verified.

- `integration` is the catalog: one row per connector code, its `form`, and the digests published for it. `connection` carries no digest.
- `form = host | wasm | native`. `host`: compiled into the host and versioned with it — the capabilities every application uses (postgres, nats, http, blobstore, logging) and, on a local target, the raw hardware the box exposes (serial, gpio). `wasm`: a component admitted under a connector class, pulled by digest like an application. `native`: a process beside the host serving the same connector interface over a local socket, for a vendor binary that has no other form.
- One connector interface for `wasm` and `native`; connection, binding and target tables never know the form. A `host` capability is pinned as `host` and carries the host's digest.
- Pinning per environment is the one rule for every form, the same rule packages follow; per org was rejected because it forbids a canary.
- A bump restarts the connector inside the application: bound operations see a read gap and queued writes for the restart. A `native` restart cannot take the host down.

The process, for one of twenty connectors used by many environments:

```
integrations/modbus/                 # one crate per connector in the platform repo
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

A role is per application, and an application is the composed unit. A user may hold several roles on an application and has exactly one active per application per session, chosen explicitly; permissions are never rolled up across held roles. This amends what runs today, where a request is checked against every role the token carries; the token's role claim becomes one role per application and the platform UI gains the picker.

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
- Keys are a closed set declared by the platform: `display.theme`, `display.density`, `format.date`, `format.time`, `format.number`, `locale`, `timezone`, `calendar.first_weekday`, `table.<table id>.layout`, `table.<table id>.view.<name>`. An undeclared key is a write refusal.
- Views are rows, not a table: `table.<id>.view.<name>` at user scope is a personal view, at role scope a shared default for that role, at application scope a shipped one. A separate `view` table is warranted only if views need what scope cannot express — ownership transfer or a publish step — and that has no consumer yet.
- `org` and `project` scope rows are set per environment: the org and project settings screens carry an environment picker, and a value set in one environment reaches no other. There is no fan-out and no drift detection; a dev environment that looks different from prod is intended. The admin-hierarchy path only grants the org or project admin the right to write those rows in each environment.
- Copying a role to another environment copies its role-scope settings with it; the copy route does that. An environment copy carries the whole table. User-scope rows are per environment like every other: a user's `prod` layout is not their `dev` layout.
- Application data about a user (my open orders, my shift) is an application table, never a setting. A value an application reads in SQL (default facility) is application configuration, never a setting.

## Environment copy

A copy is a copy: the whole database into a fresh one, `number_counter`, `setting` and role rows included. There is no flag. A fresh numbering after a copy is `set_counter` on the new environment.

Moving dev to prod is not a copy. Schema and new tables ride the release. Rows that are part of the application rather than its data — lookup codes, `*_type` rows, a default role — are declared in the package as seed data and upserted by code at env apply, so every environment gets them from the same declaration. Anything else is data and moves by import if it moves at all.

## Invariants

Each rule has one check that runs in the platform test set.

| invariant | check |
| --- | --- |
| application role: `SELECT` on the listed tables (plus `role`, `user_role`, `permission` and `environment`), `EXECUTE` on `bump_counter`, `assign_counter` and `set_counter`, and `platform.write_log`, the engine's claim record, keeps its existing grants, nothing else; no history table or `schema_migrations` grant; no `credential`, `target` or `target_application` table exists in an application database | query `information_schema.role_table_grants` and `routine_privileges` per database; any other row fails; `information_schema.tables` shows none of the three |
| no manifest model names a `platform.*` table | build refusal; fixture with such a model is refused |
| no write statement, command claim or history trigger targets a declared relation | build refusal; fixture with an `UPDATE platform.user` is refused |
| a host-filled `number` field is filled only by the plugin's `next_number` | build refusal on a write that supplies it; the generated insert type has no such field |
| `created_by` / `updated_by` come from the caller | insert with either in the payload is refused; row shows the token's user id |
| numbers are unique and gapless per counter and key | `UQ` on the model's `number`; N concurrent creates on one key yield N consecutive values, none repeated |
| `reset=` starts at 1 per distinct key | two keys interleaved give 1, 2 on each independently |
| `per=` assigns once per key, under concurrency | N concurrent first inserts with one batch id all carry one batch number |
| a key field must be present | insert with the key field null is refused |
| `per=` on a key field, or `reset=`/`per=` on a sequence bound to a key field | build refusal |
| a fetched number is consumed only on commit | `next` then rollback → the next call renders the same value |
| `set_counter` serializes with inserts | set during a concurrent insert → the insert sees the set value or the one before, never a torn one |
| `set_counter` never touches a `per=` mapping | set on a keyed `per=` row → route refuses |
| `set_counter` never sets below a live number | records to 17; set 10 → refused; set 1 on a new period key → allowed |
| `next_number` cannot be aimed at another application | a statement under application A calling `next_number` for a sequence of B → refused; the application in host state decides, not a claim or the SQL text; guest SQL that calls a counter function or writes `platform.*` → refused by statement admission |
| a binding names an enabled connection of the declared integration | apply with a name the project has not enabled, or whose integration differs → refused; disabling a connection while an applied environment binds it → refused |
| a connector not allowed at the environment is refused | `integration_allow` lacks `modbus` for the environment; apply of a package binding a modbus connection → refused; allow it → applied |
| every bound connection has a pinned connector | `environment.k` binds a modbus connection with no `connectors.modbus` line → apply refuses |
| a connection name is stable while bound | rename while an applied environment binds it → route refuses |
| a connection matches its external system | connection with `external_system_id` whose `integration` differs from the system's → route refuses; external system rename while a connection or a record references it → route refuses |
| a port has one owner | two applications on one target bind a serial connection → the second apply refuses |
| no network call inside a database transaction | a command opens `begin-operation`, then calls `file.put` or any bound connection → host refusal; the same call before `begin-operation` → allowed |
| a target runs only what was applied to it | a target's check-in reports a bundle with no `target_application` row → flagged, not served |
| env apply never overwrites an edited pattern | apply twice with a changed row between; the edit survives |
| every application user FK resolves | FK to `platform.user`; `user` holds every org user |
| a `file` row exists only for a stored blob | application INSERT on `file` is refused; `file.put` leaves one row whose sha256 matches the blob; an unreferenced row is still there a month later; a blob with no row older than the gc age is gone; on a disk store an evicted blob's row is gone with the blob |
| exposure is enforced per file | application: fetch by id with a role → 200, without a session → 401; private: fetch by id → 403, signed url from the operation → 200, same url after expiry → 403; public: plain url with no session → 200, after unexpose → 404; public on a store with no cdn\_host → refused |
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
| after A5 the host can refuse a blobstore or connection call while an operation transaction is open | the plugins see the operation state `begin-operation` sets, as `run` does | the rule holds by generator refusal of such calls in an operation body instead; weaker, until the host check lands |
| a connector can be a dependency of an application: admission has a connector class that grants a component sockets and refuses postgres, and an application bundle carries its connectors by digest | one wasm connector pinned in `environment.k`, pulled with the application onto a local target, started and restarted with the host running; sockets granted to it and still refused to application components | every connector is `form = host`; the `connectors` line in `environment.k` is always `host`; a connector bump is a platform release |
| a bundle is one application, so a target serving two applications runs two bundles | the bundle names one manifest | `target_application` keys on bundle, not application, and the bundle lists its applications |
| an http transport can hold a box-initiated connection the cloud calls down, with a result back | a request connection round-trips to a named application on a local target within its timeout | only the event exchange reaches a box until it does; request connections stay cloud-to-cloud |
| the session token claim can carry a per-application role map | the token verifier accepts a map claim on cloud and edge | one token per application, shell holds several |
| a project has at most one interactive application | no project needs two applications a person logs into | the token map is exercised within one project and roles stay per application; nothing in the tables changes |
| the box can issue sessions: its key signs session tokens its own operations accept, against a credential projection for users with roles there | offline login on a box succeeds and an operation accepts the token; the same token is refused by the cloud | offline operator use on a box is bounded by the 930 s token lifetime; the operator UI on the box is online-only |
| a projection can be fed from the source environment's change stream over the platform connection into a read-only SQLite table | one projected model on one box stays within a stated lag while online and serves while offline | projections stay `feed = "operation"` only |

## Implementation

### Rules

Baseline: `origin/main` at `17f777f81`, then C1 done at `3bb4c488e`. Epic numbering: C1 schema (done), C2 numbering (scoped), C3 connections/bindings/targets, C4 files (now, integrations agent after C3), C5 admin screens (UI after M6), C6 active role (UI after M4), C7 environment copy and seed rows (platform-install after E5), C8 transaction refusal (perf-arch after A5), C9 loadable connectors (integrations after C4), C10 local environments (edge owner). Queues: integrations C2 → C3 → C4 → C9; platform UI M3 → M4 → C6 → M5 → M6 → C5; platform-install E4 → E5 → C7; perf-arch M6/M7 → A5 → C8.

The rules of `platform-deploy.md` "How this plan is worked" apply (worktree lane, ≤3 issues per epic, build+clippy+fmt+unit per issue, fast-forward merge after review, report ≤10 lines at merge). Plus:

- The spec's invariants table is the test set. Each epic lands the checks for the invariants it makes true; a check asserts a fact, never a stored literal.
- Receiving and WMS change only where the spec makes today's shape non-conforming (`number =`, `role = "actor"`, `relations`, connection bindings). Nothing else in them moves.
- One agent per lane that touches `crates/control`. Epics C1–C4 run in sequence in the integrations agent; the UI agent and platform-install agent keep their own epics.
- Platform version bumps as rule 6 says. C1 and C2 are host-plugin changes, so they are image bumps; plan the build with the cloud path.

### What exists today (facts, main `17f777f81`)

| spec item | today | gap |
| --- | --- | --- |
| `platform` schema | `app_system` in each project database; `app_system.users` written by the control host on the admin-hierarchy path | rename; grants to one `SELECT` + one `EXECUTE`; new tables (done in C1) |
| transaction claims | `app.user_id`, `app.role`, `app.operation` set with `set_config` at the host-opened transaction | add `app.application` (done in C1) |
| connections | org-scoped connection (`registry.org_connections`, credentials file `orgs`); `wamn_integration` package (`setting`, `log`); one service principal per scheduled or integration application with the declared-dependency role (Epics 4–5, `integrations.md`) | move into `connection`, `external_system`, `credential` and service principals; `environment.k` binds names only; `wamn.k` declares `connections` |
| roles | per application environment; a request is checked against every role the token carries; M4 (roles grid) not started | active-role claim map and picker after M4 |
| numbering, settings, `integration_allow`, `project_connection` | none | new |
| transaction window | the host opens the transaction before the export (`execution.md`); `begin-operation`/`commit` as guest imports are the A5 spike | "after A5" items wait |

### Epics

Tag = the spec's dependency tag. Owner = which running agent takes it, in its queue order.

| # | Epic | Tag | Owner | Done when |
| --- | --- | --- | --- | --- |
| C1 | **Platform schema.** Done at `3bb4c488e`. Rename `app_system` → `platform` (migration, generator, fixtures). Tables: `user` (projection, as today), `external_system`, `connection`, `project_connection`, `integration_allow`, `number_sequence`, `number_counter`, `setting`; `bump_counter` as `SECURITY DEFINER`. Application role: `SELECT` on the listed tables plus roles, permissions and `environment`, `EXECUTE` on `bump_counter`, the existing `write_log` grants, nothing else. `platform.configurations` is deleted (`setting` replaces it). `app.application` claim. Control-host write path for `platform.*` rows (spec assumption 1 verified in issue 3). The integration catalog moved to C3. | now | integrations agent | Done: invariant 1 (grants) and the `app.application` claim check pass on kind; Receiving and WMS apply unchanged on the renamed schema. |
| C2 | **Numbering.** `wamn.k`: `sequences`, `number = "<sequence>"`, `role = "actor"`, `targets`. Generator: build refusals (write supplies a number field or an actor field; sequence with no field and no caller; `per=`/`reset=` key rules; models on a `local`-only package); the two-statement generated insert; `sequences::<name>::next/peek` bindings. Plugin: the renderer in `wamn:postgres` (`next_number` plugin call, pattern tokens incl. the renderer's date set, `per=` lookup-first, name-order locking). Control route `set_counter` with the dry-render check. Env apply creates `number_sequence` rows from package defaults, never overwrites. Receiving gets one `number =` field and `role = "actor"` on `created_by`/`updated_by` (conformance). | now | integrations agent, after C1 | Numbering invariants (gapless, `reset=`, `per=` under concurrency, rollback, `set_counter` rules, cross-application refusal) pass; Receiving creates a numbered record in the local live test. |
| C3 | **Connections, bindings and targets.** The integration catalog as `registry.integrations (code, form, exclusive)` in the system database, seeded by migration. `wamn.k` `connections` (integration, mode). Org `connection` and `external_system` rows and `project_connection` through control routes; `integration_allow` at org/project/environment. `environment.k` becomes `applications.<app>.connections` name bindings. Migrates the Epic 4/5 shape on main (`registry.org_connections` with the `oauth1` credential, `wamn_integration` principals and their auto-granted roles) into `connection`, `external_system`, `credential` and service principals. Env apply refusals: unbound name, not enabled, integration mismatch, not allowed, second `write` on exclusive media. Connection principals: a `user` row `type = service` per connection; inbound calls carry that principal (service-token signing is an issue here if assumption 3 holds, else the long-lived token fallback). `CallableHttp` reads the row, not `environment.k`. Record provenance, when a package wants it, is `external_system_id` + `external_id`, never a connection id (generator work here, none in C1). | now | integrations agent, after C2 | Binding and principal invariants pass; the Epic 4/5 connections on main work through `connection` rows with `environment.k` holding no values; an inbound call as a service principal is granted by an ACL row. |
| C4 | **Files.** `file` table, `file.put/get/delete/expose` routes, exposure, cdn, disk store eviction; `file.put` before the operation. | now | integrations agent, after C3 | File invariants pass. |
| C5 | **Admin screens.** Control: connections (create, credential write-only, enable per project, `integration_allow`), sequences (`set_counter`), settings (org/project scope with the environment picker). Application: settings at application/role/user scope, views as settings rows. Settings table semantics (resolution, lock, closed key set, copy with role). | now | platform UI agent, after M6 | Settings invariants pass; an admin creates a connection, enables it, sets a counter and locks a setting from the browser; the server refuses what the UI hides. |
| C6 | **Active role.** Token claim `application → role` map; the auth check uses the active entry only; role picker in the shell; reissue on switch; a user with one role gets it without a choice. | now (ruled) | platform UI agent, after M4, before C5 if M4 finishes first | Invariant "one active role per application per session" passes; existing M2/M4 flows unchanged for single-role users. |
| C7 | **Environment copy and seed rows.** `environment copy` carries `number_counter`, `setting`, roles; package seed data upserted at env apply. | now | platform-install agent, after E5 | Copy and seed invariants pass on kind. |
| C8 | **Transaction refusal.** Host refuses storage and connection calls while `begin-operation` is open. | after A5 | perf-arch agent (A5 owner) | The network-in-transaction invariant passes. |
| C9 | **Loadable connectors.** Spec assumption 11 spike first (connector class in admission, sockets granted to a connector component and refused to applications, bundle carries connectors). Then `integration.form`, `connectors` pins in `environment.k`, `wamn integration build/publish`, `target_application` digests. | after loadable connectors | integrations agent, after C4 | One wasm connector pinned per environment, bumped by an `environment.k` edit, no platform release. |
| C10 | **Local environments.** SQLite platform tables in the plugin, model-free packages on `local` targets, outbox, offline sessions, `feed = "operation"` projections. CDC projections last. | after SQLite platform functions | edge owner, after the edge plan's next epic | A box runs `wms.station` offline, numbers a record gaplessly, and the cloud commits its events on reconnect. |

Order inside each agent's queue:

- integrations agent: C2 → C3 → C4 → C9.
- platform UI agent: M3 → M4 → C6 → M5 → M6 → C5.
- platform-install agent: E4 → E5 → C7.
- perf-arch agent: M6/M7 → A5 → C8.

Nothing in C1–C4 blocks the UI or install epics; C5 needs C1–C3 merged.

### Issues for C2 (the only epic filed now)

C1 is done: `wamn-aqd0m` merged at `3bb4c488e`, and its epic-close kind run passed.

Rulings for C2:

- `next-number(name, record) -> text` and `peek-number(name, record) -> text` are two added functions on `wamn:postgres/statements`, with no version bump. `peek` is generated from `peek-number` (owner ruling 2026-10-23). It renders the current value with no lock and no advance. The host refuses both outside an open operation transaction. The renderer lives in the plugin. The three platform functions, `bump_counter`, `assign_counter` and `set_counter`, are the only writers of `number_counter` (owner rulings 2026-10-23).
- The application is the composing application that runs the operation (C1.2 ruling). The plugin reads it from host state, not from the `app.application` claim, which guest SQL can rewrite. `next_number` refuses a sequence of another application because the plugin resolves the sequence by that application and the name, never by SQL text (wamn-9e4ks).
- `{date:FMT}` uses the environment's `timezone` setting row at application scope when present, else UTC. Settings resolution across scopes is C5 (admin screens).
- `role = "actor"` fills from `app.user_id`. An insert fills every actor field. An update fills `updated_by` and never touches `created_by` (owner ruling 2026-10-23). A payload that supplies an actor field is a build refusal. `actor` joins `key`, `reference`, `revision` and `value` as a column role.
- `targets` is declared and validated at build: a package with models is `cloud` only. The apply-side target refusal is C10 (local environments).
- Build refusals: a write that supplies a `number` or `actor` field; a sequence with no field and no caller; `per=` on a key field; `reset=` or `per=` on a sequence bound to a key field; an `int32` key with anything but a bare `{seq}`; a key named by `per=` or `reset=` that is not a field of every model using the sequence, another counter, or `date:`.
- `set_counter` is a host route granted per role: a new `HostRouteAuthority` variant in `host_route.rs`, added in place with no `wamn.json` version bump, and `permission.grant` accepts it. It runs in the operation transaction and writes through `platform.set_counter` (SECURITY DEFINER, `EXECUTE` for the application role; the route resolves the sequence by the application it checked against the release and the sequence name) under the counter row lock. The engine write log and the counter row's `updated_by` record the actor (owner ruling 2026-10-23). It refuses a set below a live number by the dry render, and never touches a `per=` mapping.
- Env apply creates `number_sequence` rows from package defaults and never overwrites an existing pattern.
- Receiving conformance only: `purchase_order.create {writable_fields = ["supplier_id"]}` with `purchase_order_number` numbered by `PO-{seq:po,width=6}`, and `role = "actor"` on `created_by` and `updated_by`. Receiving moves to 2.2.6, and Acme moves only its Receiving pin and its `record-receipt` import to 2.2.6. WMS does not change (owner ruling 2026-10-23).
- The kind case checks the `number_sequence` row after apply. A local live test covers the numbered create, because the kind suite has no token issuer.

Issues:

1. `wamn-p3x9n` first (`wamn build` sees platform interface changes), because C2 changes the plugin interface. `wamn.k` `sequences`, `number`, `role = "actor"`, `targets`; the generator refusals; the two-statement generated insert; the `sequences::<name>::next/peek` bindings. A unit test per refusal on fixtures.
2. Plugin: `next-number` on the statements interface; the renderer (tokens, `width`/`pad`/`hex`, `field`, the `date` set); `per=` lookup-first with the `''` allocator; `reset=`; name-order locking; cross-application refusal by the application in host state. Live Postgres tests for the numbering invariants: gapless and consecutive under N concurrent creates on one key, `reset=` per key, `per=` once per key under concurrency, a rollback leaves the number unconsumed, a null key is refused.
3. The `set_counter` route with the dry-render check and its live tests (advance, reset, below-live refused, `per=` row refused, serializes with a concurrent insert); env apply creates sequences and never overwrites an edit; Receiving conformance; kind case: Receiving creates a numbered record.

Out of C2: settings resolution, the `set_counter` UI, the SQLite renderer, connections, seed rows. C3 is filed after the owner's review of C2.

### Open before C1 starts

- `app_system` references outside the generator (operations docs, `wamn-ctl` verbs, tests) — the C1.1 lane lists them in its first report; the owner says whether any stays as an alias.
- The build for C1 and C2 is an image build each (plugin changes). The lane uses `--cloud` if the machine is busy; cap applies.

### Out of this plan

- Choosing which connectors to build: `integrations.md` candidate list, owner's pick, after C3.
- The edge plan's bundle format, pull and restart: C9 depends on it, does not define it.
- OIDC, SAML: identity plan.
