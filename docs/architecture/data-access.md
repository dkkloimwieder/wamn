# Data access

Migration SQL defines an application's schema.
`wamn-schema-introspection` reads PostgreSQL into a normalized schema description.
The compiler selects relations from the manifest and authored SQL before it reads their column types.
`wamn-compiler` owns authored declarations and their checks. It combines them with the schema description and SQL bytes to produce package artifacts. Catalog holds compiled release facts and does not depend on the compiler.
The schema description is derived output, not an editable schema language.
The generator's core transformation performs no filesystem, database, clock, or environment access.
The build describes its complete SQL corpus with SQLx before it emits consumers.
`wamn-scalar` owns the PostgreSQL type names used by the compiler, introspection and runtime.

## Schema ownership

Each package keeps one SQLx migration sequence for authored SQL and generated history SQL.
SQLx records each successful version and checksum.
Apply refuses changed checksums and missing applied versions.
A released migration never changes. A later change adds a migration.
Application migration declarations select qualified schemas.
Guest SQL names relations without schema qualification and uses the host-selected `search_path`.
Guest input cannot choose a database, schema, or database role.

The release pins one runtime schema per SQL-bearing application in each serving component's `runtime-schema` field.
Publication derives this schema from the application's models and internal relations, including models that use a base package's schema.
Declared SQL relations must use that same schema.
Publication refuses an application that spans multiple schemas.
The host binds the pinned schema before it opens the application's transactions, including transactions shared by composed members.
Two applications in one release each resolve unqualified SQL only in their own schema.
For a release without the field, the host uses its process-wide `WAMN_SCHEMA` fallback.

Package migrations run through SQLx under one restricted login for the project database.
PostgreSQL ownership and privileges govern their effects.
Project initialization installs an event trigger that refuses privilege changes by that login.
It covers `GRANT`, `REVOKE`, and `ALTER DEFAULT PRIVILEGES`, including commands inside routines.
The connection logs in directly as the migration role, so `RESET ROLE` keeps the same restricted identity.
Trusted catalog writes and grants use a separate connection.

The catalog reader reads tables and views. It no longer scans schemas to refuse routines, triggers, rules, policies, ACLs, unused types or standalone sequences.
Selected column types and generated properties must still fit the generated action.
A migration can opt out of a transaction with `-- no-transaction`.
Its SQL must support retries after partial execution.
PostgreSQL privileges restrict the schemas that the migration login can change.
The platform alone installs its declared extension list, `btree_gist` and `pg_stat_statements`, before application migration SQL.

Generated history migrations install record history triggers and tables.
The catalog reader skips them by name, as [record history](#record-history) describes.

Provisioning gives each project database one restricted package migration login.
That login owns application schemas, relations, and generated history functions.
The `NOLOGIN` database owner, `wamn_db_owner`, retains platform objects.
A separate managed platform migration login inherits its privileges.

Each entity map records its relation OID, package, and entity.
Package apply refuses rebinding an existing relation OID to another package or entity.
Schema ownership does not grant runtime mutation authority.
An overlay reads exposed base fields and mutates its own fields through declared operations.
Base row creation, deletion, and business-field mutation require an explicit base operation or grant.

The platform owns grants and row policies separately from application DDL.
Applications cannot replace or weaken them.
PostgreSQL uses the authenticated database role for application row isolation.
[Connection authority](capabilities.md#invocation-and-connection-authority) states how the host selects the database and the credential of each authority class.
Control-store projections retain their separate project-targeted policies.
`wamn-run-state` supplies the host queue worker's actual `CURRENT_USER` membership query.
The native runtime refuses absent membership at the existing guest error boundary.

## Application declarations

`wamn.k` declares application choices that another source cannot determine exactly.
The generator supplies fixed protocol rules and rejects authored fields for them.
Receiving, Acme, and WMS use the same declaration format.

Every package under `apps/` authors its manifest in `wamn.k`, a KCL file, and `cargo xtask repo-lint` refuses a `wamn.json` at its root. Test fixtures outside `apps/` keep a hand-written `wamn.json`.
`wamn build` compiles it to `wamn.json` in the package's build output, and every reader reads that compiled file.
The author states a fact once, in the place that owns it.
The schema module fills authoring defaults, and generation adds each member that the table below marks derived, so the compiled file states what an authored `wamn.json` states.
The schema module and generation refuse a derived member that `wamn.k` states.
An optional key that the author does not write stays absent. To leave out a key that has a default, such as `connection`, the author sets it to `None`.

The following map covers the Receiving manifest fields and their nested declarations.
A schema fact describes the database. A permission or writable-field list selects authority over that database.
The generator compares those choices with the schema and SQL. It does not replace them with broader inferred authority.

| Declaration | Authoritative source and treatment |
| --- | --- |
| `package.id`, `package.version` | WAMN package identity. Cargo names an artifact and does not determine this coordinate. Retain. |
| `required_platform_policy_contract.id`, `.state` | WAMN policy requirement. Retain independently of migration shape. |
| `models.*.schema`, `.table`, `.owner` | Application relation selection and package ownership. PostgreSQL supplies the selected relation's schema. Retain selection and ownership. |
| `server_owned_fields`, `client_field_extensible` | Application exposure and extension policies. Database defaults do not determine these choices. Retain, except the stamp columns `created_at`, `created_by`, `updated_at` and `updated_by`: generation makes each one the table carries server-owned, and refuses a list that names one. |
| `enum_fields` | Application value policy of a text column. A CHECK that carries a quoted value takes the form `CHECK (<column> IN ('a', ...))`, which PostgreSQL stores as `(<column> = ANY (ARRAY['a'::text, ...]))`, and generation reads the column's value set from it into the compiled manifest. A CHECK of any other form with a quoted value refuses with its name, and a CHECK without one is not a value set. The author states a list only to narrow a CHECK's set or to name a set that no CHECK carries; a list equal to the CHECK's set, or one that widens it, refuses. Derive. |
| `min_lengths` | Application value policy: the fewest characters that a text column holds after a trim. The generated typed export refuses a shorter value with `invalid_input` on its field. The author writes `CHECK (char_length(btrim(<column>)) >= <n>)` in a migration, and generation refuses the model without it. That CHECK is the database guard and never the answer, so no operation contract lists it. Retain. |
| `audit_log.columns`, `.retention` | Application history policy. Retain. |
| `operations` keys, `permission`, `writable_fields`, `revision_field`, `result` | Application operation, authority, revision, and result choices. Retain. |
| Create `values` | Application value policy for a new record: the subset of an `enum_fields` list that a create accepts. The generated input contract states it, and states the model list for every other enum field, so a client and the generated route schema offer only those values. Retain. |
| CRUD `error_details` | Generator-owned error vocabulary and details, derived from operation type and applicable database constraints. Remove from authored input. |
| `authored_sql.default`, `.variants[].field`, `.direction`, `.path` | Application selection of exact SQL for each supported ordering of a query that generation cannot write: one that joins, aggregates with `GROUP BY`, or windows with `OVER`. SQL files own statement bytes. Generation writes every variant of a query that its model's table alone answers from the declared filters, search and sort, and refuses `authored_sql` whose every variant reads only that table. Retain mappings for the others. |
| `filters[].field`, `sort.fields`, `.directions`, `pagination.default_sort`, `.tie_breaker`, `limit.default`, `.minimum`, `.maximum` | Application query bounds and selection. Retain within supported generator rules. |
| `filters[].match` | Application match mode of one filter. `exact` is the default and the contract leaves it unstated. `contains` and `prefix` match a part or the start of a text field with no `enum_fields` list. `range` takes one object with an optional inclusive `min` and `max`, on an int32, float64, numeric or timestamptz field. `is_null` takes one boolean on a nullable field: true keeps the empty rows. Generation refuses a mode on any other field. The generated query SQL implements the mode, and authored query SQL must implement the mode that its filter declares. Retain. |
| `filters[].required`, `filters[].default.last_days` | Application band of a query. Only a `range` filter on a timestamptz field can be required, and only a required filter has a default of at least one day. A read without the band reads the last `last_days` days, as of the statement. A read that sends the band states its `min`, and a band with no default is always sent. Retain. |
| `search.fields` | Application text fields of a query's server search. The request member `search` is one non-empty string, and a row matches if any field contains it in any case. Generation refuses a field that is not text, a repeated field, and an empty list. The search binds after the filters and before the cursor. Retain. |
| Filter `binding` (`json_array` for a list, `json_range` for a range, `json_boolean` for is-null), sort `max_fields`, pagination `type`, `.cursor`, and limit `invalid` | Fixed query protocol, including cursor version, canonical JSON, encoding, opacity, and refusal. Derive in generated contracts. |
| `internal_relations.*.schema`, `.table`, `.cdc` | Application relation selection and event-publication policy. PostgreSQL owns relation shape. Retain. |
| Custom operation `type`, `visibility`, `permission`, `connection` | Application operation exposure and authority selection. Retain. |
| `transaction`, `automatic_retry`, `idempotent_by` | Application transaction and replay choices. `idempotent_by: claim` is the whole declaration of a claim: the engine claims the key in the [write log](#the-write-log). Validation refuses a `claim` object. Retain. |
| `input.raw_body_maximum`, `.envelope.minimum`, `.maximum` | Application input bounds. A command that claims its key takes the public envelope, and the schema module fills a raw body of at most 1048576 bytes and 1 to 100 inputs. Written only to change. |
| `input.line.minimum`, `.maximum` | Application input bounds. Retain. |
| Input `item_semantics` and count-bound `invalid` | Fixed per-item outcomes and input refusal. Derive. |
| Input/result `fields[].path`, `.type`, `.nullable`, `.values`, `.revision`, and result `class` | Application boundary contract. Generated WIT consumes these fields, so it cannot replace their source. Retain. |
| Custom `errors`, business `error_details`, `constraint_errors` | Application error choices and constraint mappings. A business error detail can state the `text` that a screen shows. Retain. Derive fixed details for standard platform errors, `idempotency_conflict` among them. |
| `relations[].schema`, `.table`, `.select_fields`, `.insert_fields`, `.update_fields`, `.delete`, `.lock` | SQL authority. The SQL owns what a statement reads, writes, locks and deletes, so generation derives each relation from the statements over the tables of the package's own schemas and writes it into the compiled manifest. The compiler describes the statement with SQLx and classifies its transaction needs. Derive. |
| `relations[].constraints` | The constraints a statement relies on: each one its SQL names in `ON CONFLICT ON CONSTRAINT`, and each one `constraint_errors` maps, on the reached relation that defines it. Derive. |
| `statements.*.path`, `.fetch`, `.parameters`, `.row` | Static SQL selection and accessor contract. PostgreSQL tests types, privileges, and transaction requirements. Retain names and declared shape. |
| Parameter/row `name`, `type`, `nullable` | Accessor names and representation. SQL planning does not determine all application names or nullability contracts. Retain. |
| `connections` | Map each application alias to an integration code and a `read` or `write` mode. PostgreSQL stays outside this map. |
| `components` keys and `.connections` | Component grouping and declared capability selection before compilation. Retain. Cargo artifact names do not determine operation grouping. |
| `targets` | The target types a package runs on: `cloud` or `local`. The default is `cloud`. Generation refuses a package with models that lists `local`. Retain. |
| `sequences.*.pattern`, `.callable` | Application numbering choices: the default pattern of a number sequence, and whether the package's commands call it. Generation parses each pattern with the one pattern parser. Retain. |
| `models.*.fields.*.number`, `.role` | Application choice of what fills a column. `number` names the sequence that numbers the field on insert. `role = "actor"` fills the field from the operation caller. Generated SQL fills both, so a write that supplies the field refuses. Retain. |

Compiled component metadata owns actual WIT imports and exports for runtime admission.
Those bytes exist after generation, so reading them to generate the same component creates a circular dependency.
The manifest does not duplicate native deployment replicas, credentials, database targets, or workload bindings.
Those facts remain with the existing deployment and binding owners.
Generation writes one world for the generated operations of a package, and an authored world includes it, as [generated operations](#generated-operations) states.
It does not change deployment representation.

## Generated SQL and contracts

Each package combines generated CRUD, lock, and mutation SQL with authored static query and projection SQL.
Package-owned typed accessors and registered operations expose that fixed set.
There is no universal runtime controller with unrestricted operation-ID authority over the database.
Command source uses generated accessors and named queries instead of adding unreviewed SQL strings.

The generator derives native-async WIT contracts and Rust adapters from supported CRUD and custom-operation declarations.
Each package owns its contracts under `wit/` in its build output.
Names, fields, revision inputs, nested values, results, and error details follow the declarations.
Separate operations over one table retain separate writable fields and permissions.

Generated exports validate typed input before calling a handler or an override.
Their `export_operation!` macro connects each application handler to typed `run`.
The host converts HTTP JSON from the WIT export type.
Known component calls keep typed values.
Revision inputs, returned revisions, and conflict revisions follow their declared integer type in JSON.
An `int32` uses a JSON number, and an `int64` uses a decimal string.
Database and WIT revision values remain numeric.
The `minimum`, `maximum`, and `observed` refusal details are decimal strings in WIT and JSON, spelled as a numeric value is and never as a float.
Custom fields declare `revision: true`; CRUD operations use their declared revision field.
Other integer fields follow the same rule.
The host converter preserves integers above JavaScript’s safe-integer limit.
Application handlers of custom operations retain business rules, transaction sequencing, and deliberate result transformations.

## Generated operations

A generated CRUD operation needs no authored Rust, WIT, route entry, or declaration entry.

UUID keys require a client id on create.
Integer keys (`int4` or `int8`) must be `GENERATED AS IDENTITY` or have a sequence `DEFAULT`.
The build refuses a plain integer key.
The database generates the integer id, and create returns it.
A request that supplies a database-generated id is refused.
Only database-generated keys can appear in `server_owned_fields`.

Offline creates with integer keys replay by the write log’s `request_id`, not by `id`, and the client learns the id from the reply.

Generation writes these files for each package. A path below is a logical path: the file lies at the same path, without `generated/`, in the package's build output `apps/target/wamn/<package>`, which Git does not hold.

1. `generated/data/<model>.rs` holds one data function for each operation of the model. The function parses the scalars, calls the generated accessor, and returns the one generated refusal type.
2. `wamn-data-access::error` owns the shared refusal type and statement error translation. Each export translates it into its WIT error.
3. `generated/component/<model>.rs` holds one module for each operation. Its `Action` trait has a default `run` method. The application implements the trait on its component. It can override `run` and call `default_handle` from the override. The export validates input before it calls the override.
4. `generated/wit/world.wit` holds the world `<package>:generated/generated`, which exports every operation of the package and imports what those operations call.
5. `generated/publication/attachments.json` holds the final routes, inline input schemas, and computed definition hashes.
6. `generated/publication/components.json` holds the final component declarations with inline input and result schemas.
7. `generated/data/sequences.rs` holds `sequences::<name>::next` and `sequences::<name>::peek` for each sequence that the package declares `callable`. The package has no such file without a callable sequence.

The shared crate `wamn-data-access` holds the keyset cursor, the query page, scalar parsers, and statement error translation.
The compiled `wamn.json` carries `compiled_operations`, including described statements and their transaction facts.
Readers borrow each operation through `wamn_catalog::operation_contract`. The compiler emits no contract files, source maps, or native verifier rows.
`generated/openapi.json` describes the published HTTP interface with OpenAPI 3.1. GET parameters carry separate JSON values.
Every list schema has a maximum. Generated exports enforce page and feed limits before they call application code.
The TypeScript client retains its camelCase field mappings. It reads the compiled operation descriptions in memory.
A data crate includes `data/mod.rs` of its build output as its public module `generated`, through a path relative to the crate. The generated files include each other by paths relative to themselves.
A component includes `generated/component/mod.rs` in a module that names that data module `data`.
The fixture overlay has no data crate, so its component includes the data functions itself.

The authored world, in `component/wit/authored.wit`, only includes the generated world.
The authored world includes the generated one, and the reverse is not possible.

The package declares custom HTTP exposure in `routes.operations`, keyed by its local operation name.
Each entry names an attachment id and path.
The compiler derives its schemas, sealed operation identity and definition hash.
Component declarations and attachments come only from the compiled output.
Package-local publication JSON is not an input.

### Operation references

The package version is authored once, as `package.version` in `wamn.k`.
A sealed operation id is `<package>:<interface>/<operation>@<version>`, for example `wamn-receiving:location/list@2.0.0`.
WIT packages, component worlds, contracts, the files under `generated/`, the clients, grants and the statement sets of the host carry the sealed id.

The compiler derives sealed identities from the package and base declarations.
Publication, admission and clients read those compiled facts.
Workflows belong to the environment and name sealed operations from its serving release.
A web page keys `onOpen` and `onFill` by the operation reference.
A test reads sealed identities through `wamn_test_infrastructure::operations`.

The manifest member `routes` sets how a generated route is published:

- `auth_modes` is the authentication modes of each route. It defaults to `["pat", "session"]`.
- `path_prefix` starts each path, such as `/acme`. It defaults to none.
- `id_prefix` starts each attachment id. It defaults to none, which gives `<model>-<action>-http`.

A query that names an undeclared sort refuses with `invalid_input` on the field `sort`.

Guests send statement identities and arguments through `wamn:postgres`.
The `statements` interface uses native async calls for execution, transaction creation, commit, and rollback.
Version 0.4.0 includes `run-stream`, `operation-transaction`, `next-number`, `peek-number`, and the separate `operation` interface. It is the one version, and the host serves only it.
The host begins the transaction of one logged operation item, and `operation-transaction` gives it to the guest. Inside that operation, `begin` refuses with SQLSTATE `25001`, and `commit` or `rollback` of the host transaction refuses with `2D000`.
An unnamed `statements` import runs the statement set of the active operation.
In a composed component each member has its own named import, and the host links one instance per name.
The host keys the active statement set by the invocation scope and the import name, so a member that names another member's statement gets `unknown-statement`, and so does a read (get, query or projection) that names a statement that writes or locks rows.
Every instance lends the same host transaction. Only the statement set differs.
The host resolves those identities to admitted SQL bytes and the operation's allowed statements.
Generation refuses PostgreSQL values that the production `wamn:postgres` type contract cannot represent.
The compiler stores described statement facts in the compiled operation description.

The generator default for a base package command is one SQL statement that validates and writes.
Review authored commands against the same rule. SQL returns the outcome and its details, and the guest maps them without repeating database predicates.

Two exceptions keep statements separate. A parent lock precedes a fresh read, as in Receiving `record_receipt`.
An operation that an overlay composes with a base operation also stays separate.
A CTE is a named query within a statement.
Under READ COMMITTED, a locking CTE retains the snapshot from before a lock wait. The next statement reads fresh data.

Dependent writes within one statement use `RETURNING` dependencies, because textual order does not order their execution.
This is a review rule and a generator default. It adds no build refusal.

A model that removes rows declares `delete_mode`, and only the package that owns the relation declares it.
The `delete` operation is a route, and it needs `delete_mode`. A model can declare `delete_mode` with no `delete` operation, when only its command SQL deletes rows.
A `hard` delete removes the row, and the record history keeps its contents in the `before` image of the delete entry.
A `tombstone` delete keeps the row and sets `deleted_at` and `deleted_by`.
Every generated read, every generated update, and a second delete then hide that row, so it behaves as `not_found`.
A tombstone model carries both reserved columns, and every other model carries neither.
Neither column is writable, because the delete statement sets both.
A hard delete of a row that another row still references refuses with `foreign_key_violation`, which names the inbound constraint.
No application foreign key declares `ON DELETE CASCADE`.

Authored command SQL can use `DELETE FROM` only on a model that declares `delete_mode: hard`.
The operation declares `delete: true` on that relation and names the columns that `WHERE` and `RETURNING` read in `select_fields`.
Generation derives these relation facts from the SQL once.
Projections and history tables refuse delete access.

The compiler generates SQL once, then SQLx describes every statement before consumer emission.
This includes authored SQL, each fixed list sort, and feed statements.
The build checks parameter counts and types, selected column names and types, and compatible nullability.
PostgreSQL plans determine whether a statement writes or locks rows.
A call to an authored routine counts as a write even when its plan has no write node.
Deployment support for routines is still incomplete.
The compiler generates no application grants and does not plan under derived grants.

Generated metadata binds schema requirements, platform-policy requirements, SQL identity, and generator provenance.
Publication and deployment record source commit attribution outside tracked generated bytes.
An unchanged overlay can remain compatible with an additive base when those requirements still hold.
A matching schema alone does not establish operation or business compatibility.

## Inputs, queries, and transactions

Create input omits server-owned identity, generated, revision, and audit fields.
Omission lets the declared database default apply, while explicit null requires a nullable writable field.
Update input preserves three states: absent means unchanged, null means SQL NULL, and a supplied value replaces the field.
A revision-controlled update or delete requires the expected revision.
An absent row returns `not_found`, and a changed revision returns `concurrency_conflict`.
A generated update increases the revision only when a supplied field differs from its current value.
A successful update returns the current revision, so a true no-op returns outcome `updated` with its unchanged revision.

Each outer item of a write carries its `request_id`. A read carries none, and its outcomes match its items by position.
For `per_input` commands, each item owns one transaction on one PostgreSQL connection.
The transaction resource never crosses a wiring edge, and separate outer items have no shared atomicity.
A composed write followed by a projection is not one transaction.
An overlay command can call a base command within its transaction.
The overlay operation imports the base operation, and the overlay package pins the base by digest in `base_dependencies`.
The overlay build composes the overlay and its pinned base into one component, so the call is a component-model call with no host between.
The host requires the original caller to hold the overlay entry only. The base operation runs under the entry's authority.
The generated contracts carry ordinary typed values. Application code owns the transaction sequence and conditional business rules.

The released operation carries its own statements only.
The host begins the item's transaction when the admitted operation declares one: a command with `transaction = explicit_per_input`, or a generated create, update or delete.
A command without SQL of its own can declare `transaction = explicit_per_input`, so an overlay command that only calls a base command still runs in one host transaction.
The base reaches that transaction through `operation-transaction`. Each member binds its own statements through its own `statements` import.

Acme's `acme.record_receipt` calls Receiving's `record_receipt` and then records its inspection in the same host transaction, under the existing purchase-order lock.
Acme claims the key under its own operation.
No inspection requirement produces no inspection row. An approved requirement permits the write. An unmet requirement rolls back the entire item.
Permitted direct Receiving calls retain base behavior.

The host owns begin, commit, rollback, and connection cleanup.
An error, trap, cancellation, or deadline destroys unfinished transaction state before another request can acquire the connection.
Queue lease renewal keeps a connection guard from the start of its transaction through commit or rollback.
Shutdown cancellation destroys that connection, so the pool cannot retain its open transaction or row locks.
The shared `finish_queue_transaction` function also guards the connection while commit or rollback awaits PostgreSQL.
Serialization and deadlock errors reach the caller without an automatic transaction retry.

### The write log

`platform.write_log` holds the idempotency record of every create, update, delete and command in one project database.
`deploy/sql/app-schema.sql` installs it, so no application migration names it.
It has no project column and no row policy, because the database is the project, and no record history: the rows the work writes carry the actor.
`wamn_app` holds `INSERT`, `SELECT`, `UPDATE (result)` and `DELETE` on it.
Its key is the claim operation without its `@version`, for example `wamn-wms:location/create`, and the idempotency key.
The key is the input field that the route's `idempotency` names: `idempotency_key` for a create, and `value.idempotency_key` for a command item.
An update, a delete, and any other route without a key field key each item by its `request_id`.

The concrete operation owner handles claims for each create, update, delete and command.
The PostgreSQL owner serves HTTP calls and workflow nodes. The edge owns its SQLx SQLite claims.
Generated application exports contain no claim implementation.
The operation boundary invokes each new item alone and returns stored results for completed keys.
For an operation with SQL, the host begins one transaction for each item, and the steps of that item run inside it:

1. The host opens the transaction with `BEGIN ISOLATION LEVEL READ COMMITTED`, because the retry below reads the row that another transaction committed.
2. The operation owner claims the key with an insert that does nothing on conflict. An uncommitted claim of another transaction makes the insert wait until that transaction ends.
3. If the insert claimed nothing, a committed claim holds the key. The operation owner reads its request and result. The same request answers the stored result, and another request answers `idempotency_conflict` with the detail `{field, intent}`. The host rolls back, so the retry runs no work.
4. If the insert claimed the key, the component does the work in the same transaction. The operation owner stores the result in the claim, and it commits with the work. A refusal rolls back, and the claim goes with it.

The host runs `BEGIN`, local session identity, and the write-log claim in that order before it runs the component.
After the component returns, the host runs the finish, the causation message when present, and `COMMIT` in that order.
SQLx runs each statement on the same connection.
The [execution path](execution.md#native-dispatch) describes the transaction.

Counter set and generated delete know their complete success result before the work.
Their transactional claim inserts that result and skips the finish update.
The delete result is `{"value":{"outcome":"deleted"}}`.
A refusal still rolls back the work and the claim.

The component reaches that transaction with `wamn_postgres_statements::operation_transaction()` of `wamn:postgres` 0.4.0, and its data access takes the `Transaction`.
A `begin` inside such an operation refuses.
An operation without SQL claims in two steps on a host connection of its own: begin before the call, and finish after it.
An external-only claim command omits `transaction: explicit_per_input`.
The compiler refuses an operation whose capabilities combine SQL work with an external effect. Split that work into separate operations.
A begun claim has a null `result` until it finishes.
An unfinished claim answers `in-progress` until its age exceeds `MAX_HOST_CALL_DURATION`, which is 30 seconds.
After that boundary it answers `outcome-unknown`. A late completion can still record its result.
A commit whose answer is lost also leaves its outcome unknown.

A refused item leaves no row, so its key is free again, and a later different request under that key does its work.
A retry that answers a stored result commits nothing, so no event publishes a second time.
A new id comes from its insert's `RETURNING`, and nothing mints an id before the work.
No application table implements or references the claim. The stored result names the rows that the work wrote, for example the `transaction_ids` of a WMS command.
The result row stays stored, so each retry returns the original success answer in full.
The existing `prune-record-history` job deletes all claims older than 30 days, using `created_at`.
Its expiry statement lives in `crates/control/provision/src/audit_retention.rs` and runs from `crates/control/environment/src/prune_record_history.rs`.
This retention period exceeds the longest call by a wide margin.
A result can finish late within that period without a separate claim identifier.
The write log provides idempotency protection, which prevents repeated work for the same key.
Record history carries the actor and provides the audit.

The host also provides `wamn:operation/operation@0.1.0` for the separate route component.
The route component calls this interface before it invokes a Receiving operation.
The host binds its workload to a component in the loaded release before serving calls.
Only the route component imports `wamn:operation`.
The control-only entry links this interface without operation authority.
Its begin calls return `unknown-operation`, and its commit, rollback, and WAL calls return `not-begun`.
Package members that import `wamn:postgres/statements` need only the PostgreSQL WIT package path.
The `wamn:postgres@0.4.0` package has no dependency on `wamn:flow-http-routing`.

`begin-operation` takes the opaque caller handle from `authenticate`, the sealed operation id, an idempotency key, and request hash bytes.
The caller is optional only for an operation that registers no grant in the loaded release.
An absent caller on a granted operation returns `grant-denied`.
The host makes sure of the handle, release facts, and caller grant, then mints a delivery id.
It returns that id as `begun::new(string)` for the application's operation context.
The application reads the id and never supplies it.
The host binds the identity before it inserts the write-log claim in a new transaction for that operation.
The host stores the hash bytes unchanged and returns stored result text unchanged as `begun::finished` on replay.
The entry returns that text and runs no work. `finished` leaves no operation open, so another begin is allowed immediately.

The host projects each base route's operation id and idempotency key path through `wamn:flow-http-routing/routing@0.1.0`.
The server hashes the parsed request element with sorted object keys.
It leaves out `request_id` and the route's idempotency key field.
It preserves scalar spelling and array order before hashing and invocation.
The hash bytes retain the `sha256:` prefix of the canonical JSON digest.
Host routes and wiring attachments carry no base-operation rules.
The route also carries the host operation deadline in milliseconds.
All items share that budget, and an unreached item claims no key.

`commit(result)` stores the application's JSON text without parsing it, emits the causation message, and commits the transaction.
The async `rollback()` call rolls back the open transaction, drops its claim, and stores no result.
The key is unclaimed again, so a retry runs the operation.
Rollback clears the open operation on success or refusal. Rollback without an open operation returns `not-begun`.
Dropping the Store rolls back unfinished work and clears its authority.
Only one operation can be open per Store.
A begin while one is open returns `already-begun` and leaves that operation open.
Commit clears the open operation on success or refusal. Any other refused begin leaves nothing open.
The next begin on the same Store is allowed, with its own transaction and delivery id.
Commit without a new open operation returns `not-begun`.
Member statement handles borrow the same transaction.
`statements.run`, `statements.run-stream`, and guest transaction creation remain refused inside it.

The async `wal-position()` call returns `pg_current_wal_lsn()::text` on the open operation's connection.
It leaves the operation open and returns `not-begun` when no operation is open.
The call uses no separate connection or transaction.
A read operation calls `authorize` in place of `begin-operation`.
`authorize` takes the caller handle and the sealed operation id, makes sure of the caller and its grant, and binds the statement authority.
It opens no transaction and claims no write-log key.
`finish-read` ends the authorized read and records its delivery outcome without SQL.
`begin-operation` on a read operation returns `reads-claim-nothing`.
After `authorize`, `commit`, `rollback`, and `wal-position` return `not-begun`.
A read writes no causation message, so it does not invalidate a list tag.

The interface returns typed refusals for conflicts, unknown operations, denied grants, invalid callers and invalid lifecycle calls.
PostgreSQL failures carry `postgres(pg-error)`.
Unfinished claims return `in-progress` until the maximum call duration, then `outcome-unknown`.
An unread commit answer also returns `outcome-unknown`.
The interface has no operator resolution state.

The request hash covers parsed JSON with sorted object keys.
It leaves out `request_id` and the route's key field.
The server preserves scalar spelling and array order before hashing and invocation.
A repeat with a different spelling or line order is an idempotency conflict.
The result is the answer body of the item, `{"value": ...}`, without its `request_id`.

Production-lease errors retain PostgreSQL's primary message and constraint name.
They omit `DETAIL` and `HINT`, which can include failing row values.
Run rows contain application JSON.
This rule matches platform identity errors. Primary messages remain verbatim.

Commands keep network effects outside transactions that hold database locks.
Post-commit work uses the [event path](capabilities.md#events).

A generated query returns its page as one result: its rows, and the keyset cursor that continues the read.
For a cursor, SQL compares the sort field and ID as one row, so a matching index starts at that cursor.
`wamn build` requires a `(field, id)` index for every allowed sort field, including the default.
It reads indexes across all package migrations and refuses a missing index with the operation, table, and exact `CREATE INDEX` line.
Its typed `run` export returns the page. The host converts it to a JSON outcome and retains the WAL position.
The generated `query` action declares a `limit` that defaults to 100 and accepts 1 to 100.
The query's typed export fills the default and refuses a limit below 1.
The entry refuses a limit above the declared maximum or the platform ceiling before the query runs.
An application query reads through `statements.run` of `wamn:postgres` 0.4.0, with the WAL position on the same connection when the entry asks for it.
The statement reads one row past the limit, and that row says whether a next read has rows.
The project row limit applies to that read.
`statements.run-stream` stays in `wamn:postgres` for a workflow node that declares it, and no base read calls it.
A custom projection declares its result class and typed result fields, and no row or byte limit.
Its authored SQL decides how many rows it returns.
A custom projection or command admits the result classes `one`, `optional_one`, and `bounded_list`.
Generation refuses `page` on a custom operation, because `page` belongs to the generated `query` action.
The host refuses a statement that returns more rows than its row limit.
The row limit is `WAMN_PG_ROW_LIMIT`, default 100,000, or the `row_limit` of the project.
The limit applies to each statement, and a projection runs one statement for each envelope item, up to 100 items.
The host truncates nothing, and the refused item returns `internal_error`.
No limit bounds the bytes of a result.
Each guest linear memory is capped at 256 MiB, and the statement timeout bounds the duration of a statement.
Unbounded JSON materialization is not a streaming or export interface.
Arbitrary project SQL remains subject to runtime authority and execution limits.
Compile-time validation for arbitrary project components remains enabled on demand.

1. A member's instance binds to the union of its own operations' declared statements. This is wider than today's per-entry binding (`operation.rs:661`): Receiving's record\_receipt code could name void\_receipt's statement, and a Receiving read could too. Accepted: a package trusts its own code, as a crate does; a read that writes is its author's bug, and the profile screen shows it. Composition never widens a set to another package.
2. Mutating base-owned business state goes through the base's exports (`data-access.md:61-62`). Reading or locking base relations within a member's own admitted statements is allowed; Acme already locks `purchase_order` for its overlay-owned fields.
3. Exports test again their preconditions from the database. A sibling in the same store can forge any record it passes (separate memories, shared data), so typestate from a sibling is not proof.
4. Members never share host resources. Each obtains its own from its own import instance; none crosses an export (`components.md:44`).
5. Authorization is the ACL row for the entry, on both sides: required = {entry}, held = the role's rows. The call graph never adds to or subtracts from either. What the entry reaches is the author's responsibility and the admin's information.

Two ways to do it wrong: define the linker per request instead of per digest; or let members import different `wamn:postgres` versions, which would mean N ABIs. One version per release is the rule.

wamn has one way to compose packages inside an application: the caller sequences the callee's exported operations, and a callee joins the invocation's host-owned transaction by asking the host for it with operation-transaction(). Existing read exports that open their own guest transaction do not join; inside a command their begin refuses, as today. Converting them is listed in [Deferred](../plan/trx-pkg-plan.md#deferred--build-when-a-consumer-exists). No transaction handle crosses a component boundary. No platform participant or pre-commit mechanism exists; base-declared WIT imports remain as ordinary dependencies.

Idempotency (ruled 2026-10-12): a key identifies one request to one operation. Acme's `record_receipt` has SQL of its own (the inspection statement), so it claims under its own operation, `acme.record_receipt`, as an ordinary `idempotent_by = claim` command with `transaction = explicit_per_input`. It no longer inherits Receiving's claim: `idempotent_by = {inherited = …}` (wamn.k:55,104) existed for the participant-era SQL-free orchestrator, and dies with it (Cleanup). Consequences, all accepted: (1) a key recorded through `receiving.record_receipt` and resubmitted through `acme.record_receipt` is a second request to a second operation — it runs, Acme's rules apply, and Receiving's row rules decide whether a second receipt exists; the base write entry in a client application is therefore not a routed target (authoring rule; ACL after C). (2) Participant-era keys stored under `receiving.record_receipt` are not replayed by Acme's entry at all; a retry through Acme runs again. Receiving and Acme are portfolio applications on wamn-dev with no production data. The request hash covers the canonical item without its request ID and caller key. `write_log` stays keyed (operation, idempotency\_key), operation versionless (app-schema.sql:630-632), so a retry across a release of the same operation answers the stored result. Cached replay across two entry points with different business rules is exactly what own-operation claims rule out.

The database is the project, so the log carries no `project_id` and no row policy. `wamn_app` gets `INSERT`, `SELECT` and `UPDATE (result)`, nothing else. No `user_id`: record history stamps the rows the work writes.

A second caller waits at step 2 for the whole of the first caller's work. A lock or statement timeout there ends the second caller's transaction; its item outcome is the platform's timeout refusal, and the client retries.

- `begin-operation` and `commit` exist only on the entry's import instance. No package member imports them: "read access but no transaction access" applied to the entry/member split. Publish refuses a member that imports them, and refuses an application that supplies its own `wasi:http/handler` export.
- The transaction is keyed by Store, never by member instance. Base and overlay share one invocation, one connection, one commit, and the overlay still sequences base-exported steps before, between and after its own as component-model calls. A package with its own database (analytics) gets its own connection and transaction, as today. A cross-application call is host-mediated through the router with no shared transaction, as today.
- `run` / `run-stream` stay refused inside a host transaction.
- Authorization stays at publish; the only runtime test is the entry's grant, once, at `begin-operation` or at `authenticate`.
- The composed application's digest depends on its source bytes. The separate route component has its own digest pin.
- The guest supplies no trust data (ruled 2026-10-20 from the spike, `wamn-cbqzl`). The host mints the delivery id at `begin-operation`. `authenticate` returns an opaque caller handle, and `begin-operation` makes sure of it. The request hash and the result are the application's idempotency data, which the host stores without interpreting.

8. Resolved 2026-10-20: under A5 the component handles a batched request. The entry loops over the items and calls `begin-operation` and `commit` per item; the host sees single operations and never knows a batch exists. Per-item claims and transactions stay as today (an item failing does not roll back the others; a retry replays only unfinished keys). Whole-batch atomicity, if an operation wants it, is one begin with one key, declared by that operation's contract. Owner: batch semantics and error handling belong to the application, not the host; the response shapes (per-item errors in a 200, partial completion on a run failure) stay as the entry builds them today, and validate-all-before-run is kept.

The [write log](#the-write-log) section defines unfinished claim responses, late completion and retention.

## Numbering

A document number is a rendered text, such as `P-00000017`.
The `wamn:postgres` plugin renders each number in the operation transaction of the application.
A rolled-back number is not consumed, so a failure leaves no gap.

### Declaration

The package declares numbering in `wamn.k`:

- `sequences` names each number sequence and gives its default `pattern`. `callable = True` gives the package's commands the generated `sequences::<name>::next` and `peek`.
- `number = "<sequence>"` on a model field makes the generated insert fill that field with the next number.
- `role = "actor"` on a model field makes the generated insert fill it from `app.user_id`. The generated update fills a field named `updated_by` in the same way and never changes another actor field.

An overlay is a model whose `owner` is another package. It inherits the owner's `number` and actor declarations, even when it declares no `fields`. The build reads them, and the sequence patterns, from the compiled manifest of the base build. The generated overlay insert numbers and stamps the inherited fields, and the generated overlay update fills an inherited `updated_by`.

Generation refuses these declarations:

- A pattern that does not parse, and a sequence name that is not an identifier.
- A field that names an undeclared sequence, a field that has both `number` and `role`, and a model with two numbered fields.
- A numbered field that is not `text`, `int32` or `int64`. An integer numbered field takes a bare `{seq:<counter>}` pattern only.
- An actor field that is not a `uuid` field named `created_by` or `updated_by`.
- A pattern key that is not a field of the numbered model. The field cannot be `bytes` or `json`, and the `create` operation must write it.
- A numbered key field, whether the client or the database supplies the id.
- A counter with `per=` on the key field of its model.
- A write that supplies a `number` or `actor` field, inherited or not, in a generated operation or in a custom operation statement.
- In an overlay, a numbered field, an actor field or a pattern key that the relation owner does not own, and a declaration or sequence pattern that differs from the owner's.
- A package with models that lists `local` in `targets`.

### Pattern

A pattern is literal text and tokens:

| Token | Meaning |
| --- | --- |
| `{seq:<counter>}` | One counter that never resets. |
| `{seq:<counter>,reset=<key>}` | The counter starts at 1 for each distinct key. |
| `{seq:<counter>,per=<key>}` | One value for each distinct key. The first call assigns it and later calls reuse it. |
| `width=N` or `0N` | The counter is padded to the width. A longer value is not cut. |
| `pad=C` | The pad character. The default is `0`. |
| `hex` | The counter renders in upper-case hex. |
| `{field:<name>}` | A field of the record, as text. |
| `{date:FMT}` | The transaction time as `YYYY`, `YY`, `MM`, `DD`, `WW` (ISO week), or `Q`. |

A `<key>` is a field of the record, another counter of the same pattern, or `date:FMT`.
Date parts use the `timezone` setting of the application target, and UTC when no setting exists.
When a pattern holds `WW`, its `YYYY` and `YY` parts, and its `reset=date:YYYY` and `reset=date:YY` keys, use the ISO year: the year that holds the ISO week.
For example, 2027-01-01 is in ISO week 53 of 2026, so it renders `2026` and `53`.
For example, `B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}` gives `B0001-001` and `B0001-002` for the first batch.

### Rendering

The guest calls `next-number(name, keys)` or `peek-number(name, keys)` of `wamn:postgres` 0.4.0.
`keys` maps each value that the pattern names (`per=`, `reset=` and `{field:}`) to its text, or to none for a null.
The keys are not a record, so any command can call the function with values of its choice.
The application is the name that the `applications` map of `environment.k` gives the package of the operation.
The host binds that name to `app.application` for each operation, and it refuses a package that belongs to no application or to two.
The plugin reads the application from host state and resolves the sequence by that application and the name.
It never reads the `app.application` identity, because guest SQL can rewrite that identity.
Both calls run only in an open operation transaction, and they return `permission-denied` outside one.

`next-number` advances each counter of the pattern and renders the new values.
The plugin takes the counters in name order, and a counter keyed by another counter comes after it.
A `reset=` or unkeyed counter calls `platform.bump_counter`.
A `per=` counter first reads its key row with no lock.
If the row is absent, the plugin calls `platform.assign_counter`.
`peek-number` renders the last values issued, with no lock and no write. It never serves an assignment.

The calls refuse with a `query-error` and these SQLSTATE values:

- `42704` for a sequence that the application does not own.
- `22023` for an empty key, a missing key, a key given twice, a stored pattern that does not parse, and a `timezone` setting that is not a JSON string or names no time zone that Postgres knows.
- `22004` for a null key.
- `P0002` for a `peek-number` of a counter that issued no number.

The plugin makes sure of every key and, for a pattern that reads the date, the `timezone` setting before the first counter call, so a refused call advances no counter.
The time zone test runs in a savepoint, so its refusal leaves the operation transaction usable.
The generated `sequences::<name>::next` and `peek` take a key struct that holds exactly the fields the pattern names.

### Counters

`platform.number_sequence` holds one row for each application and sequence name, with its pattern.
`platform.number_counter` holds one row for each sequence, counter, and key value.
Its `value` is the next value to render, and the key value `''` is the allocator row of a `per=` counter.
The application role has no write on `number_counter`.
Only the three counter functions of the platform schema write it.
They are `SECURITY DEFINER`, and each takes a sequence id.
The plugin resolves that id from host state.
Its statement admission refuses guest SQL that names a counter function or writes a `platform.*` relation.

Env apply creates the `platform.number_sequence` row of each sequence that a package declares.
The row belongs to the application that the `applications` map of `environment.k` names for the package.
Two packages of one application that declare one sequence with one default pattern share one row.
A row that exists keeps its pattern, so an edited pattern survives every apply.
Two packages of one application can declare one sequence name only with one default pattern.
If the patterns differ, env apply refuses before it writes anything, and the refusal names the application, the sequence, and the two packages.
The `applications` map of `environment.k` decides which packages form one application.
The test reads only the package declarations, so a row with an edited pattern does not refuse.
`wamn dev --config environment.k` deploys through the same control routes.
Environment apply creates the number sequence rows.

### Setting a counter

The application host route `number_counter.set` sets the next value of one counter.
A holder of `admin` can call it, and so can a role that holds the operation as a permission row.
The request names the application, the sequence, the counter, the key, and the value, and it carries a request id.
The application must be a name in the `applications` map, and the route answers `invalid_input` for another name.
The value is 1 or more.
The route runs in one operation transaction under the caller, claims the request id in the write log, and writes through `platform.set_counter` under the row lock.
A retry of a committed set answers the stored `next_number`.

Before it commits, the route renders the number that the next call would give.
It refuses when a record of the application already carries that number.
The records are the single-column unique text columns of the application's schemas.
It also refuses a `per=` counter, whose values never change.
The route answers `invalid_input`, `sequence_not_found`, `counter_not_found`, `counter_assigned_per_key`, `number_live`, or `idempotency_conflict`.

On Postgres the three platform functions, `bump_counter`, `assign_counter` and `set_counter`, are the only writers of `number_counter`, so the application role keeps no `UPDATE` on `number_counter`. Each runs on the sequence id alone and refuses an id that is not a sequence. None reads the `app.application` identity, because guest SQL can rewrite it. The plugin knows the operation's application from host state. It resolves the application and the sequence name to the sequence id, and it passes that id to the function. An application guest runs only the statements its package declares: the raw `wamn:postgres/client` interface is denied to applications. So component admission is the guard. It parses each declared statement with PostgreSQL's own grammar (`pg_query`) and walks the whole parse tree. It refuses a statement that names `bump_counter`, `assign_counter` or `set_counter` (in any quoting, case or schema), that writes a `platform.*` relation, that holds a `DO` block, a function body, a SQL `EXECUTE`, `SET`, `RESET`, `set_config`, `pg_logical_emit_message`, `query_to_xml*`, `cursor_to_xml*` or a `U&` identifier or string, or that does not parse (owner ruling 2026-10-24). Publish runs the same test on the statements of each pushed descriptor. The compiler owns the parsed SQL policy. The host and component admission use that policy in `wamn-compiler::declared_sql`. The host applies it to both declared and raw SQL. So only the plugin reaches the functions. SQLite needs the same rule, because it has no roles. The identity stays set, for logging only. The rejected alternative — the renderer as a Postgres function via `pgrx` plus a registered SQLite function — kept the grant invariant but needed a custom Postgres image and ruled out managed Postgres.

- `per=`: look up `(counter, key_value)` with no lock — mappings are immutable, so a hit is safe to render. On a miss, `assign_counter(sequence, counter, key_value)` takes a per-key `pg_advisory_xact_lock`, looks again, and returns the existing mapping or allocates from the `''` allocator and inserts the key row with that value. Two concurrent misses for one key allocate once, so `per=` has no gaps. The first line of a batch pays the allocator lock; every later line pays only its own counter (owner ruling 2026-10-23).

- Name-order locking within one call means two patterns on one sequence cannot deadlock each other. Two operations that insert into two models in opposite order can still deadlock on two sequences; Postgres detects it and one retries. The plugin guarantees no more than that.

- `sequences`: name plus default pattern. Env apply creates the `number_sequence` row when absent and never overwrites an existing pattern; after that the admin owns the pattern. A sequence is independent of the records: it needs no model field and no declared caller, and its keys need not be fields of any model (owner ruling 2026-10-24).

- Every sequence a package declares is callable from any of its commands (`sequences::<name>::next` and `peek`). No build refusal depends on whether a field or a command uses it.

## Record history

Record history stamps who created a row, who last changed it, and when.
A relation that keeps a log also has a history table, and a log trigger writes an entry to it for each row change.
[Naming](naming.md#reserved-names) reserves the four stamp column names, the platform principal names, and the `_history` suffix.

### Platform fixtures

The system fields and system tables of record history are platform fixtures, not application schema.
They are the four stamp columns, the `wamn_record_history_stamp` and `wamn_record_history_log` triggers, and each `<relation>_history` table.
Each fixture has one definition in [`deploy/sql`](../../deploy/sql).
The applier installs a fixture by name, and every other component recognizes it by name.
No component models a fixture, introspects it as a declaration, or projects it into the schema description.
The compiler generates no grant contribution for fixture or application fields.
Verification of an installed fixture is a string comparison against `pg_catalog`, not a typed round trip.

### Declaration

Every relation-owning model declares `audit_log`:

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "none"
}
```

`columns` selects some of the four reserved names, and `[]` turns stamping off.
`retention` is `"none"`, `"unlimited"`, or `"P<n>D"`.
`"P<n>D"` keeps entries for n whole days, and n is a positive integer with no leading zero.
`"none"` keeps no log.
`columns` and `retention` are independent, so a relation with `"columns": []` can keep a log.
An overlay model inherits the declaration of the relation owner and does not declare its own.

A 30-day log with no stamp columns:

```json
"audit_log": {
  "columns": [],
  "retention": "P30D"
}
```

The [manifest validation](../../crates/schema/generator/src/manifest.rs) refuses a missing key, a key on an overlay model, and a repeated name.
It also refuses an actor without its time: `created_by` requires `created_at`, and `updated_by` requires `updated_at`.
It refuses every other retention, including `P0D`, weeks, months, years, time parts, fractions, and signs.
The [retention task](#retention-task) removes expired entries.

[Generation validation](../../crates/schema/generator/src/generate/validation.rs) compares the declaration with the schema.
It refuses a selected column that is absent or nullable, and a logged relation with no primary key.
A selected time column must be `timestamptz`, and a selected actor column must be `uuid`, the type of `platform."user".id`.
Generation also refuses a column with a reserved name that the declaration does not select.
Generation makes each column with a reserved stamp name server-owned by its name.
It therefore refuses a writable declaration of that column and omits it from generated input.
A stamp column needs no default, because the trigger sets every selected value.

### Stamp trigger

[`deploy/sql/record-history.sql`](../../deploy/sql/record-history.sql) creates schema `wamn_history` and the trigger function `wamn_history.stamp_row`.
The `CATALOG_SCHEMA_SQL` composition carries the file, and the file applies again without error.
The function reads the actor from `app.user_id` and the time from `transaction_timestamp()`.
Every stamp in one transaction carries the same instant, and that instant is not the commit time.

- An insert sets every selected column.
- An update keeps the created pair.
- If a column other than the selected stamps changes, an update sets the updated pair. A true no-op keeps every stamp.
- The function replaces any value that a statement supplies for a selected column.
- A delete gets no stamp.
- A write with no bound actor raises SQLSTATE `55000` with the message `actor-required`, for every column selection.

The function reads no user row, and no stamp column has a foreign key.
The platform trigger functions read `app.user_id`.
Generated statements read it too: a create fills each actor field, an update fills the `updated_by` actor field, and a tombstone delete sets `deleted_by`.
No authorization or row policy reads it.
`wamn_history.log_row_change` reads `app.operation` to record attribution.

The compiler emits each selected stamp function and trigger in an immutable migration under `history/`.
The trigger runs before each inserted or updated row and calls `wamn_history.<schema>__<relation>_stamp()`.
The compiler reads columns from its verification database and writes the function once for that migration version.
A later column or stamp declaration change appends another migration.
That migration replaces the selected functions and removes triggers that the declaration no longer needs.
SQLx executes authored and generated migrations in one version sequence under the restricted migration login.
Apply does not read or compare installed trigger text and does not repair drift on replay.
The catalog reader reads the relations selected by the compiler.

### History tables and the log trigger

Each relation whose retention is not `"none"` keeps a log in its own history table.
The history table is `<relation>_history` in the schema of the relation.
`wamn_history.create_history_table` in `record-history.sql` is the one definition of the table shape.
A package relation gets no `project_id` column.
`wamn_history.log_row_change` is the `AFTER INSERT OR UPDATE OR DELETE` row trigger function that writes one entry for each row change.
It writes the entry in the same transaction as the change.
It writes with the authority of the writer, so the `current_user` project base grants also governs the entry.

An entry has these columns:

- `position`: the per-row position, a `bigint` identity value that the function allocates under the row lock. Positions rise for each row, and gaps are allowed.
- `id`: the id of the entry, a random `uuid` that PostgreSQL generates. A read that shows entries in a table keys its rows on it.
- `project_id`: the project of the row, only in a legacy history table under a project base grants.
- `row_key`: a JSONB object of the primary key columns.
- `type`: `insert`, `update`, or `delete`.
- `operation`: the source of the write, from `app.operation`.
- `changed_by`: the actor, from `app.user_id`.
- `changed_at`: the one time of the entry, from `transaction_timestamp()`. Retention expires entries by this time.
- `transaction_id`: the 64-bit value of `pg_current_xact_id()`, stored as `bigint`.
- `before` and `after`: the JSONB images. A package relation keeps the `to_jsonb` spelling, and a platform relation keeps the spelling of `wamn_history.row_image`.

Every column is `NOT NULL`, and the primary key is `(row_key, position)`.
A CHECK on `operation` accepts three shapes only: an operation token, `wamn:<component>`, and `admin:<kebab-purpose>`.
The CHECK matches each shape by its grammar pattern.
The platform binding owns the closed component list, so a new component needs no change to a history table.

The function refuses a write that it cannot log:

- A write with no bound `app.user_id` raises SQLSTATE `55000` with the message `actor-required`, as the stamp function does.
- A write with no bound `app.operation` raises SQLSTATE `55000` with the message `operation-required`.
- A write to a relation with no primary key raises SQLSTATE `55000` with the message `history-key-required`.

Entry contents:

- An insert records the complete resulting row, including defaults, in `after`, and its `before` is `{}`. That row is the starting state for reconstruction.
- An update records the prior and resulting values of the changed columns, including changes to the stamps and the revision column.
- A delete records the full row in `before`, and its `after` is `{}`.
- An update that changes a primary key column writes a delete entry under the old key and an insert entry under the new key. The shared transaction id links the two entries.
- On a shared relation, the trigger reads `OLD` and `NEW`, which hold the effective base and overlay row. An overlay change therefore captures the complete row without a change to the public input or result of the operation.
- An overlay change to a base relation writes to the history table of that relation, and the overlay declares nothing.
- Every entry records an actor and an operation. The `columns` selection controls row metadata only, not log contents.
- The per-row position orders entries. The transaction timestamp does not order concurrent changes and repeats within one transaction.
- A rolled-back change leaves no entry. A failure while the function writes the entry rolls back the change. An idempotent replay that returns the original result appends nothing.

The event envelope `txid` holds the low 32 bits of the entry transaction id.
The join from an event to its entries is therefore exact within one transaction id epoch.
Causation stays on the event, and the entry does not record it.

`wamn_history.row_image(record)` renders both images of an entry and the current row of a [history read](#history-read).
It sets `TimeZone` to UTC inside the function.
It spells each `timestamptz` column as the platform canonicalizer spells it: UTC RFC 3339 with exactly six fractional digits and a `Z`.
Every other value keeps the `to_jsonb` spelling.
The log function and the stamp function compare row values as JSONB text, so a change of numeric scale alone is a change.
The generated functions of a package relation compare each column as text, with the same result.
Their images keep the `to_jsonb` spelling, so a `timestamptz` value carries the session offset.
Earlier entries can hold the canonical spelling, and the [history read](#history-read) canonicalizes both.
A primary key column of type `timestamptz` keeps the canonical spelling in `row_key`, so every entry of one row has one key.
Such a change moves the stamps and writes an entry. A true no-op moves no stamp and writes no entry.

The log function calls `row_image` with the authority of the writer.
`record-history.sql` therefore grants `USAGE` on `wamn_history` and `EXECUTE` on `row_image` and `timestamptz_image` to `wamn_db_owner`.
Package provisioning grants the same history privileges and schema `CREATE` to the restricted migration login, which owns its generated functions.
[`deploy/sql/record-history-app-grants.sql`](../../deploy/sql/record-history-app-grants.sql) grants the same three privileges to `wamn_app`.
`CATALOG_SCHEMA_SQL` composes it after `record-history.sql`.
Project initialization installs these shared helpers and grants before package migrations run.
Package apply uses the installed helpers.
The system database never applies it, so the system database has no `wamn_app` grant.

Generation refuses these declarations:

- A model table, an internal relation table or key, or a custom operation relation whose name ends with `_history`. A custom operation can read a history table, but it cannot declare an insert, an update, or a row lock on one.
- A logged relation whose longest derived history object name has 64 bytes or more. The longest name is `<relation>_history_transaction_id_not_null`, so the name of a logged relation has 31 bytes or fewer. The refusal names the relation.
- A logged relation with no primary key. Each entry keys the row by its primary key columns.

No generated `update` or `delete` exists over a history table, so no application operation can alter an entry.

The generated history migration creates each logged relation’s history table under the restricted migration login.
Generated history changes retain existing history tables and entries.
It records the CDC exclusion of each history table in `wamn_cdc_exclusions`, as it records an internal relation.
The exclusion row names the package that owns the relation, and its relation id is the history table name.

The generated migration installs one `wamn_record_history_log` trigger on each owned logged relation.
The trigger runs `AFTER INSERT OR UPDATE OR DELETE` for each row and executes the generated function `wamn_history.<schema>__<relation>_log()` with one argument, the retention.
The function names the primary key columns, every column of the relation, and the history table as static SQL, so a write reads no catalog and plans no dynamic statement.
It ignores the argument, which the retention run reads. A platform relation runs `wamn_history.log_row_change` with the same argument.
A retention of `"none"` removes the trigger, and the history table and its entries stay.
A retention change produces a new generated migration that replaces the log trigger.
Development and deployment execute the same immutable migration files.

The catalog reader skips every relation whose name ends with `_history`.
It reads none of its columns, constraints, indexes, sequences, triggers, rules, or policies.
The schema state id therefore stays the same when a relation starts or stops logging.

The generator recognizes the history table of each logged model by its name, and it adds no history table to the catalog.
It takes the history columns from `HISTORY_COLUMNS` in the [`wamn-record-history`](../../apps/platform/data/record-history/src/lib.rs) crate.
Authored SQL analysis and the required schema contract use that column list.
A custom operation cannot require or map a constraint of a history table.
The compiler writes history DDL as immutable files under `history/`.
Authored `migrations/` and generated `history/` share SQLx versions and checksums.
Platform initialization installs the shared helpers from `deploy/sql/record-history.sql`.
Apply executes the emitted files through SQLx and records each successful version.
The changes statement reads entries at or after the cursor transaction.
One snapshot supplies xmin, xmax, and the visibility test.
It includes entries below xmin and visible entries below xmax.
Rows below xmin page first, and visible rows at or above xmin fill the remaining page space.
A page that carries any such row completes the pass and leaves its next cursor at xmin without an id.
`create_history_table` creates an index on `transaction_id` when the generated migration runs.
The log function is `SECURITY INVOKER`, so the writer needs that grant.
The reconciler reads the history table like any other package relation and keeps that grant.
The generation database creates the history tables before statement description, as [running tests](../operations/running-tests.md#application-generation-and-sqlx) describes.

### Retention task

A declared `"P<n>D"` retention is a minimum keep window.
It promises nothing about removal time, backups, holds, or erasure.
The retention task removes an expired entry on its next daily run, so its schedule sets the retention precision.

The [`record retention owner`](../../crates/control/environment/src/prune_record_history.rs) runs the task.
It uses the project-specific audit retention login. Its shared role is `wamn_audit_retention`.
It refuses any login that is not an audit retention login for the derived environment project in the connected database.
The family is not a `wamn_platform` member, because no reader needs the edge.
`catalog-schema-prefix.sql` creates the stable role under the role bootstrap lock, beside `wamn_platform`.
The system database schema creates no audit retention role.

The verb reads the retention of each relation from the `wamn_record_history_log` trigger argument in `pg_trigger`.
It skips an `unlimited` relation.
Each relation runs in its own transaction, which binds `wamn:audit-retention` as the actor and the operation.
The transaction takes the audit retention advisory lock before it reads `pg_trigger`.
Package apply holds the same lock while SQLx runs generated history changes, so a retention change waits for a running delete.
The cutoff is `transaction_timestamp()` less n days, in UTC.
The verb deletes an entry older than the cutoff only when no earlier entry of the same row is at or after the cutoff.
It therefore removes a prefix of the history of a row, never an interior entry, and it writes no marker in the history table.
In the same statement, it raises the row of the history table in `wamn_history.pruning_watermarks` to the highest transaction id that it removed.
The change feed reads that watermark to find an expired cursor.

apply-package owns the grants of the role.
It grants them in the transaction that reconciles the log triggers.
The role holds schema `USAGE`, `DELETE`, and `SELECT (row_key, position, changed_at)` on each history table whose log trigger carries `"P<n>D"`.
It revokes every other privilege of the role, so a change to `"unlimited"` or `"none"` revokes the grants.
It changes nothing in a database that has no role.
The provisioning grant verifier reads `pg_trigger` in each database with the query that the verb runs.
It refuses any other grant, a grant on an `unlimited` history table, and a grant in a reserved schema or `platform`.

### Integration state

The [integration base](../../apps/platform/wamn_integration/README.md) owns `setting` and `log` in the `wamn_integration` schema.
Its private operations own their SQL and generated application grants.
An integration composes those operations and keeps domain records in the consumer application.
The [REST connector](../../apps/rest_connector/README.md) calls the consumer's registered `item.record` command through a pinned workflow dependency.

Its schedule uses the application principal and requires the consumer command permission.
Its event workflow reads consumer item inserts and records delivery outcomes in the base log.
Only the consumer schema requires CDC capture.

### Actors

The actor is the `platform."user"` id of the executing principal, never the credential.
The [execution page](execution.md#native-dispatch) lists the principal that the host binds for each type of delivery.
A writer outside the host binds `app.user_id` with `set_config` in its own transaction.
Provisioning binds `wamn:provisioning`.
apply-package binds `wamn:apply-package` for package writes and operation grants.
The retention task binds `wamn:audit-retention`.
The binding of a platform component also sets `app.operation` to the same `wamn:<component>` name.
Administrative SQL must bind the operator's user row, and test fixtures must bind a provisioned test principal.
Administrative SQL and test fixtures also bind `app.operation` as `admin:<kebab-purpose>`, including a transaction that only deletes rows.

`platform."user".type` is `user`, `service`, or `platform`, and it has no default.
Users have `user` rows, stations and integrations have `service` rows, and platform components have `platform` rows.
The type does not change how a principal authenticates.
A `platform` row carries its `wamn:<component>` name in `display_name` and its derived id.
Its email is `<component>@<platform-domain>`.
The `user_platform_principal_check` constraint pins each name and id pair, and it refuses a `wamn:` name on another type.
`wamn_app` has only `SELECT` on `platform."user"`.
The database is the org boundary, so `platform."user"` has no `project_id`, no row policy, and no project key index. Its key is `id`, and `email` is unique in the database.

The [platform principal owner](../../crates/control/provision/src/platform_principals.rs) renders the platform rows of a project database.
It refuses a platform domain that is not a valid domain name.
Its SQL binds `wamn:provisioning` first, so the `wamn:provisioning` row stamps itself.
The development environment, the verification world, and the Rust identity journey apply that SQL.

The [platform configuration writer](../../crates/identity/platform/src/configuration.rs) writes the platform configuration rows of one environment.
The writer runs on the environment's `wamn_administration` login, in one transaction that binds the caller and the operation, so each row write appends its history entry.
The writer keeps a row that already holds the given values, and it removes no row.

The stamped relations in [`deploy/sql/app-schema.sql`](../../deploy/sql/app-schema.sql) carry four stamp columns as `NOT NULL` and a `wamn_record_history_stamp` trigger.
The account relations are `user`, `role`, `user_role`, `permission`, `api_key`, and `environment`.
The configuration relations are `file`, `number_sequence`, `number_counter`, and `setting`.
`environment` holds at most one row, the status of the environment that the host reads. No row means active.
An integration selects a published type and authentication scheme for one project.
A connection holds that integration's configuration for one environment.
An external system identifies the external account that records come from.
Each relation except `number_counter` also has a `wamn_record_history_log` trigger with the retention `unlimited`, and a history table.
`number_counter` has no history table, because one entry per counter advance doubles the writes of every create.
The database separates environments. The platform account and configuration tables and their history tables have no `project_id`, row policy, or project key index.
`role` keys on `name`. `user_role` keys on `(user_id, role_name)`, and `permission` keys on `(role_name, permission)`.
Their foreign keys reference one column. `api_key` keys on `id`, and `key_hash` is unique in the database.
`environment` uses a boolean `id` primary key constrained to `true`. Its default is `true`, so it holds at most one status row.
No status row means active. The application login reads that row from its selected environment database.
History keys contain no `project_id`. Environment history uses the key `{"id": true}`.
The shared control, catalog, and run tables store the composed `org--env--project` key in `project_env_key`.
The separate `environment` field stores the short name, such as `dev`.
Target JSON, catalog identities, run keys, and manifest targets use `project_env_key`.
Event fields and workload identity use `project_env_key`. Credential hashes frame that name.
The management variable is `WAMN_MANAGEMENT_PROJECT_ENV_KEY`, and `flow-http.json` carries `wamn.project_env_key`.
The event subject keeps its separate organization, project, and short environment tokens.
Auth invalidation keys still include the environment key, so equal principal IDs in separate environments keep separate cached grants and status.
`wamn_app` holds `USAGE` on `platform`, `SELECT` on `user`, `role`, `user_role`, `permission`, `environment` and the seven platform configuration tables, `EXECUTE` on `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter`, and the write log grants. It holds nothing else in `platform`: no grant on `api_key` or on a history table.
`platform.bump_counter(sequence, counter, key)` is `SECURITY DEFINER`. It inserts the counter row at 1 when the row is absent, returns the value, and increments it, under the row lock. It tests no project and reads no claim. It refuses an id that is not a sequence with `undefined_object`. The `wamn:postgres` plugin decides which sequences a caller reaches: it takes the application from host state and resolves the sequence id by that application and the sequence name. Component admission parses each statement a package declares with PostgreSQL's grammar (`pg_query`). It refuses a statement that names a counter function, writes a `platform.*` relation, or holds a `DO` block, a function body, a SQL `EXECUTE`, `SET`, `RESET`, `set_config`, `pg_logical_emit_message`, `query_to_xml*`, `cursor_to_xml*` or a `U&` escape (`crates/schema/generator/src/sql_analysis/admission.rs`). Publish makes sure of the pushed descriptors again. Application guests run only declared statements, so only the plugin calls these functions. The host applies the same parsed policy to declared statements and the raw `wamn:postgres/client` interface.
`platform.assign_counter(sequence, counter, key)` is `SECURITY DEFINER` and makes sure of the sequence id in the same way. It gives a `per=` counter one value for each key: under a transaction advisory lock on the key, it returns the value of the key row, or takes the next value of the `''` allocator row and inserts the key row with it. It refuses the `''` key with `invalid_parameter_value`.
`platform.set_counter(sequence, counter, key, value)` is `SECURITY DEFINER` and makes sure of the sequence id in the same way. It is the write of the `number_counter.set` host route: under the row lock, it inserts the counter row with the value or sets the value of the row. It refuses a value below 1 with `invalid_parameter_value`.
The administration family role `wamn_administration` holds `SELECT`, `INSERT`, `UPDATE`, and `DELETE` on `user`, `role`, `user_role`, `permission`, and `environment`.
It holds the same grants on `external_system`, `integration`, `connection`, `file`, `number_sequence`, and `setting`.
It holds no grant on `number_counter`: only `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter` write it.
It holds `INSERT` on the entry columns of their eight history tables, `USAGE` on `wamn_history`, and `EXECUTE` on `wamn_history.row_image`.
The `external_system`, `integration`, and `connection` projections have no history tables.
No entry column names a project, and administration holds no project derivation grant.
It holds `USAGE` on `catalog` and `SELECT` on `catalog.releases`.
The CLI role writer makes sure of the target database against the registry and needs the administration grants. It requires no superuser or row-security bypass.
It refuses an `project_env_key` that differs from the registry environment key, so role writes invalidate the selected environment's cached grants.
The session role reader holds `SELECT` only on `user.id`, `user.status`, `user_role.user_id`, and `user_role.role_name`.
Its connection selects the environment database. Its query filters by principal ID and active status.
The grant goes to this family only. `wamn_platform`, the group that every platform family inherits, holds none of it.
One login of the family serves each environment, and two hosts hold it: the application host of the environment and the control host of its org.
The audit retention role holds no grant on these history tables, because their retention is `unlimited`.
A foreign key cascade writes its delete entries with the actor and the operation of the deleting transaction.
The log copies full rows, including `api_key.key_hash`.
A column that is secret at rest does not belong in a stamped relation.
Project initialization installs `record-history.sql` and then `record-history-app-grants.sql` before `app-schema.sql`.
`platform.write_log` is the one exception: it has no stamps and no trigger.
It is the idempotency record, and the rows that the work writes carry the actor ([app-schema.sql](../../deploy/sql/app-schema.sql#L412)).

The run plane stamps `run_queue`.
The three effect tables still have no production append writer.
Their stamps apply to administrative writes and future writers that bind an actor.

### Catalog stamps

The project catalog stamps `packages` and `releases`.
Each separate SQLx migration history also carries the actor stamps.
It also stamps `wiring_tombstones`.
Each table carries the four stamp columns as `NOT NULL` with no default and a `wamn_record_history_stamp` trigger.
These tables keep stamps only, with no history table.
`release_connection` and `connection_binding` carry stamps and record history for changes within a release.
`connection_requirement` stores each package's declared names, interfaces, and modes without stamps.
The system database owns immutable `connection_generation` rows with parameters and credential references.
Package application binds `wamn:apply-package` for migrations and catalog writes.
Environment staging, connection administration, local development bindings, and minimum recording bind `wamn:provisioning`, as the platform configuration projection does.
The `platform.connection` projection and `workflows` keep their existing metadata and carry no record history trigger.
Fresh databases create neither `wirings` nor `event_registrations`.

### Files

`platform.file` records one owning application and the connection generation that stored its bytes.
Application tables can reference `platform.file(id)`, as they reference `platform.external_system(id)`.
Connection disablement preserves these references. A foreign key refuses deletion of a referenced file.
Application roles read file metadata but cannot insert, update, or delete it.
File rows use the standard stamps and unlimited history.

The package declares its store binding. `environment.k` binds that declaration to an immutable connection name.

The registry owns application bindings. Release hosts receive no binding map in their target.
Calls that name a binding require its registry row for the org, project, environment, application, and alias.
The host also requires a component in the application's packages to declare that blobstore alias.
Binding changes do not restart release hosts.

C4 covers GCS and S3. C10 owns disk stores and eviction.
Each mutation requires a write binding. Native host code transfers bytes, and guest operations carry only `file_id`.
No file byte transfer changes WIT.

Uploads stream to `<environment>/_wamn_files/staging/<uuid>` under the binding prefix.
The host commits metadata, copies staging to `<environment>/_wamn_files/live/<uuid>`, and deletes staging.
Reads repair a missing live object from staging.
The store age rule covers staging only. Unlinked metadata and live objects never expire by age.
Raw blobstore writes, deletes, and clears refuse file-managed keys and their public copies.

Release hosts serve file transfers on a separate HTTP/1.1 listener.
The release chart sets `--files-addr=0.0.0.0:8082` and exposes it through the `<release>-files` Service.
Outside the chart, `--files-addr` or `WAMN_FILES_ADDR` enables this listener.
A control host refuses this configuration.

The edge sends `/api/<application>/files` and its child paths to this Service before the general `/api/` route.
It keeps the full path and streams uploads without request buffering.
The Rust web front streams upload bodies to the file service.
The storage layer still refuses sizes outside the metadata integer range.

File transfers share the route authenticator and its authentication cache for sessions and PATs.
Every file route requires its operation grant.
The application URL requires a session with `file.get` on the owning application.
Anonymous callers receive 401. A caller without the grant receives 403.
Private files return 403 through that URL, including for administrators.
Another application never serves the file.

Reads by `file_id` require no current binding and use the recorded connection generation.
Removing a binding preserves these reads and application references to the file.
Disabling the connection stops reads. The registry requires an active connection and project enablement.

A package marks a UUID result field with `result.signed_reads` in its public KCL operation declaration. Each entry may set `lifetime_seconds`; omission selects 900 seconds. The compiler refuses zero and values above 900.
The host replaces that file ID with a signed URL after the operation returns. The guest uses the existing WIT contract.
The roles that hold the operation can read the private file through that result.
The optional `lifetime_seconds` accepts 1 through 900 and defaults to 900. Signed URLs last at most 15 minutes.
GCS uses IAM `signBlob` with the pod identity and stores no signing key.
S3 reads its credential through `wamn_kubernetes::Store`. Kubernetes Secrets hold the values. Each write names the environment coordinate. The database accepts only `k8s` references.
Reads and deletes use the bucket and prefix of the recorded generation.

Public exposure requires the optional `cdn_host` store parameter, without a descriptor version change.
The host copies bytes to `<environment>/public/_wamn_files/live/<uuid>` under the binding prefix and stores `url`.
Public copies carry `Cache-Control: max-age=3600`.
Unexpose deletes the origin object at once and clears the URL, without a cache purge.
Expose and unexpose are idempotent.

If an object delete fails after the row deletion commits, the host logs the object key and does not retry.
Collector follow-up belongs to `wamn-2wdsf.4`.

The full schema files own the platform schemas and build fresh installations.

- `file` is the only file table. An image is a file with an image media type; `width` and `height` are nullable. `connection_id` names the store. Each row pins its application and the connection generation that stored its bytes. Reads and deletes use that generation's bucket and prefix. The application links `file_id` from its own table (which record, which purpose). No generic attachment-by-(table, id) table.

- Storing is two short calls: `file.put(binding, bytes)` streams the bytes to the store the binding names, inserts the row in its own commit and returns `file_id`; the application operation then carries `file_id` like any reference. No transaction waits on the store. A store is a connection with a store integration (`s3` or `gcs`), declared by the package and bound by name in `environment.k` with `mode = write`.

- `exposure` is per file: `application` (default; the session holds `file.get` on the owning application, at `/api/<app>/files/<id>`), `private` (only an operation that returns a short-lived signed url; so the roles holding that operation), `public` (anyone with the link, no login). `url` is stored only for `public`, so old links survive a CDN change. Another application never serves the row.

### System database

The system database owns the integration catalog, `registry.integration_type` and `registry.integration_type_version`.
It also owns `registry.target`, the deployment target identities.
The full system schema files install these tables.
The catalog seeds `http`, `gcs`, and `s3`, each with `runtime_type = host` and `exclusive = false`.
The target table stores `id`, unique `name`, `type` (`local` or `cloud`), `status`, and nullable `last_seen_at`.
It seeds one active target named `cloud`, with no release time. Repeated schema upgrades create no additional seeds.
These catalog and target tables exist only in the system database and grant no application access.
An environment stores its target identity on first apply. The target cannot change. `target_application` and the target platform connection remain separate work.

The system database (`wamn_system`) stamps its identity authority relations.
These relations are `identity.principals`, `identity.org_memberships`, `identity.org_roles`, `identity.project_roles`, `identity.project_env_memberships`, and `identity.pats`.
Each relation carries the four stamp columns as `NOT NULL` with no default, and a static `wamn_record_history_stamp` trigger.
The full system schema also stamps `registry.meta`, `orgs`, `env_policies`, `environments`, `projects`, `environment_projects`, `retired_project_envs`, `capture_gap`,.
It stamps `identity.session_keys`, `session_signing_state`, and `password_attempts` too.
The full control schema stamps registry declarations and `wamn_authority.author_login_projects`.
The system operations tables carry no stamps.
In the system database, the actor is an `identity.principals` id.
The system identity tables record the actor only. A control route binds `app.operation` to its sealed operation id, and no column stores it.
The system database keeps stamps only and no history table, as its [limits](#limits) state.

The `SYSTEM_SCHEMA_SQL` composition in [provisioning](../../crates/control/provision/src/lib.rs) installs `record-history.sql` before [`deploy/sql/system-schema.sql`](../../deploy/sql/system-schema.sql).
Initialization creates the `NOLOGIN` owner roles before installing these helpers.
A separate managed migration login inherits the selected owner role and runs the platform migration sequence.
`SYSTEM_SCHEMA_SQL` does not carry `record-history-app-grants.sql`.

`identity.principals.type` is `user`, `service`, or `platform`.
A `platform` row carries its `wamn:<component>` name in `subject` and in `display_name`, and its derived id.
The `principals_platform_principal_check` constraint pins that subject, display name, and id.
It also refuses a `wamn:` display name on another type, and the subject pattern of another type refuses a colon.
Only `wamn:provisioning` writes in the system database, so the schema creates only its row.
The schema binds `wamn:provisioning` for the transaction that creates the row, so the row stamps itself.
A platform principal cannot authenticate.
The `identity.pats` foreign key carries the principal type, and a CHECK refuses a token for a platform principal.
Every path that turns a stored principal into a caller also refuses the platform type.

The identity issuer and wamn-ctl bind `wamn:provisioning` in each write transaction.
wamn-identity issues each token in its own transaction.
wamn-ctl creates service principals, assigns project roles, grants and revokes memberships, and revokes tokens in the same way.
A drop tells hosts to discard a cached authorization fact.
Service creation and project-role assignment record identity drops.
Provisioning publishes each drop after its transaction commits, including the role assignment before PAT issuance.
Package apply and local configuration reconciliation record a drop when they create the built-in `admin` role.
Their transaction owners publish the drops after commit through `wamn_broker::Publisher`.
Publication keeps the existing behavior when NATS is absent or a deletion fails.
`wamn-ctl invite` and `provision-org --admin` write org memberships and org roles in the same way.
A control route of the control serving root binds the calling org admin, as the [execution page](execution.md#host-run-routes) states.
Test fixtures bind `wamn:provisioning` for platform setup, or a principal row that they insert.
The trigger keeps `created_at`, so an expired-token fixture moves `expires_at` to just after `created_at`.

### History read

A history read is an ordinary public custom projection with its own operation token and grant.
Its authored fields decide which prior data it shows.
The log carries copied business values, so a grant of a history read is a new read surface.
The `receiving.load_purchase_order_history` operation of [Receiving](../../apps/wamn_receiving/wamn.k) shows the pattern.

The read is one flat `bounded_list` over the history table of one relation:

- Typed key inputs build `row_key` with `jsonb_build_object`, because the client refuses a JSON input.
- The inputs `after_position` and `limit` select one page in ascending position order.
- An authored `LIMIT` caps the `limit` input, and the host row limit also applies.
- Each result row carries one entry, the current row image, and the head position of the row. No field is nullable.
- `before`, `after`, and the current row are text fields that hold JSONB text, so the host and the client keep their spelling.
- The current row comes from `wamn_history.row_image` with the alias of the relation, or from the declared columns on a relation that an overlay extends. A deleted row has the current image `{}`.
- A row with no retained entries returns an empty page.

An overlay can add columns to a shared relation, and the images of the log carry them.
A history read on such a relation therefore shows only the columns that it declares:

- Its SQL builds the current row with `jsonb_build_object` from the declared columns, so the read needs no grant on an overlay column.
- `wamn_history.timestamptz_image` spells each `timestamptz` column as `wamn_history.row_image` spells it. `record-history.sql` grants `EXECUTE` on it to `wamn_db_owner`, and `record-history-app-grants.sql` grants it to `wamn_app`.
- The guest keeps only the declared columns of `before`, `after`, and the current row with `retain_columns` from `wamn-record-history`. Each kept member keeps its raw text, except a `timestamptz` column that the generated `TIMESTAMPTZ_COLUMNS` of the model names. That value takes the canonical spelling from either spelling.

[Receiving](../../apps/wamn_receiving/query/load_purchase_order_history.sql) reads `purchase_order` this way, and its read shows no Acme column.
An overlay history read uses the same rule with its own columns.

The compiler uses `pg_query` to read PostgreSQL syntax.
A whole-row reference reads every column of its relation.
Release profiles read compiled relation facts and recorded view dependencies.

The [`wamn-record-history`](../../apps/platform/data/record-history/src/lib.rs) crate holds the one fold, and both workspaces register it for guests, the client, and tests.
`state_at` takes every row of every page and returns `Present`, `Absent`, or `Unavailable` at a per-row position.
It folds backward from the current row, or from the `before` image of a final delete.
It keeps each column value as raw JSON text and replaces whole values, so numeric scale and every other spelling survive.
Every position before the oldest retained entry is unavailable.
The fold refuses rows with different head positions, positions that do not rise, and a read that ends before the head position.

The guarantee is bounded historical state:

- If a complete chain and a starting state remain, the fold reconstructs the state at a retained per-row position.
- A live row whose insert entry expired still reconstructs across its retained diffs.
- After the `before` image of a deleted row expires, the contents of that row are unavailable.
- If the oldest retained entry of a row is its insert, the history of that row is complete. Otherwise, the history of that row is incomplete.
- No wall-clock "as of" read exists, because transaction timestamps do not order concurrent changes.
- An intermediate state inside one transaction was never visible to another caller.

### Limits

Stamps and log entries are correct for writes through the supported platform paths.
On those paths, the triggers exist and every writer binds its executing principal and its operation.
`environment::apply` applies `app-schema.sql` to the project database and writes its platform rows.
It writes a service row for each service principal of the project.
The release stage also creates one service principal for each application with a scheduled workflow, targeted to that environment.
The stage writes its project service row without assigning a role or permission.
It reads the platform domain from `registry.meta.platform_domain`, and it refuses while that value is unset.
It creates the `platform.number_sequence` row of each sequence that the packages declare, as [Numbering](#numbering) describes.
It writes a user row for each user with a membership in that project environment, and removes a user row whose membership is gone.
Authorization derives a user’s administration from the current org, environment and project grants. The application database stores no copy of these inherited grants.
The identity owner creates that user in `identity.principals`.
One email address names one principal for the whole platform.
A user who works in two organizations holds one principal and two memberships.
A user row carries the email of that user from `identity.principals`.
The control host writes the user row and the `admin` row when its route grants them, and removes them when its route revokes them, with the administration login, bound to the caller and the route's sealed operation id.
The application host writes roles, permissions and user roles through its [host routes](execution.md#host-run-routes) with the same login and the same binding.
The bootstrap invitation writes system membership. The environment lifecycle projects it when the application database is initialized.
A principal with no user row cannot write in the project.
The invocation bind refuses it.
`WamnPostgres::bind_session_identity` refuses a principal that owns no `platform."user"` row in the project database.
The caller sees `permission-denied`.
The host's [auth memory](execution.md#auth-memory) answers the test, and a miss reads the row.
A removed row stays accepted until a change message clears the memory or 60 seconds pass.
There is still no foreign key on a stamp column.
Modified application SQL can forge actor attribution and `app.operation`.
Record history gives no tamper resistance against modified application code or administrative SQL.

A very large `"P<n>D"` day count stops the retention verb at that relation, and the verb prunes no later relation of that database.

The compiler checks statement value shapes against the migrated build database.
Runtime privileges remain the database's responsibility.

The history read has two more limits:

- The fold does not compare the current row with the newest entry. It cannot see a change that the log did not record, such as a change while the retention was `"none"`.
- The fold does not follow a column that a migration adds or drops between entries.

The system database has two more limits:

- The issuer and the revoker of a token bind `wamn:provisioning`.
- The system database keeps stamps only. A delete gets no stamp, so role, membership, and PAT removals stay unattributed.

## Event capture

An environment release can hold applications whose models use different schemas.
Apply resolves each event registration through its `source_package` and model key `entity`.
It selects only the schemas of those source models, for both workflow and handler registrations.

CDC is change data capture.
No registration means no CDC setup.
One registered schema enables capture of that schema.
Two registered schemas refuse with `one environment captures one schema`.
A schema whose models have no registration is not captured.

## Model versions

A model version uses PostgreSQL's current WAL position, the position in its write-ahead log.
The host reads `pg_current_wal_lsn()::text` before it opens the query snapshot.
It uses that position for every declared relation in the weak ETag, together with the release digest and relation names.
Committed writes from every host advance the WAL position without a counter row or a table-level lock.
Unrelated writes and rolled-back work can also advance it, so they can invalidate a tag without a change to the queried rows.
The position belongs to the database server, not to one relation.

An admitted read-only call emits no causation message, so it does not invalidate its own tag.
The read uses the callable-HTTP connection and needs no application-table grant.
The list reads this position before it runs the query, as [execution](execution.md) describes.
A failed position read leaves the response without an ETag.
Fresh databases install none of these objects. The compiler emits record history triggers as immutable migrations.

## Canonical values and SQL names

`wamn-digest` owns canonical JSON bytes and SHA-256 identities.
`wamn-scalar` owns number and timestamp text formats and the SQL type enum.
Scalar formats serve SQL input and cursor parsing. Request hashes preserve the supplied spelling.
A generated update compares each supplied field with its current value as PostgreSQL text.
A numeric scale-only change, such as `1.0` to `1.00`, is therefore a change that increases the revision.

Keyset pagination uses `id` as the total-order tie-breaker in the primary sort direction.
Descending order reverses the compound order.
Cursor timestamps use UTC RFC 3339 with exactly six fractional digits.
Cursor numerics preserve their PostgreSQL scale.

Constraint and ordinary-index names follow this pattern unless an explicit shorter name is required:

```text
<table>_<column_1>[_<column_n>]_<type>
```

Columns appear in table-definition order.
Types are `pkey`, `key`, `fkey`, `check`, and `idx` for primary keys, unique constraints, foreign keys, SQL `CHECK` constraints, and ordinary indexes.
Names must contain fewer than 64 bytes.
The author supplies an explicit shorter name when needed. Tools do not silently abbreviate or accept PostgreSQL truncation.

For database preparation and SQLx commands, use the [operations pages](../operations/README.md).
Test database isolation has one definition in [running tests](../operations/running-tests.md#test-database-isolation).

### Project integrations and credentials

See [integration and connection schema](../plan/integration-schema.md).

The control role keeps its existing table grants. Connection administration uses `registry.configure_connection` and the targeted `registry.connection_*` read functions.
These functions return only non-secret authority. The write function creates a service principal only with its connection and returns its id for `AuthDrops`.
The upgrade import reads retained bindings through `catalog.connection_upgrade_bindings(text,text)`, without reading the catalog tables.
Catalog bootstrap installs this function after its tables. Application schema installation does not require the catalog schema.
Provisioning grants execution to `wamn_administration`.
Each function is `SECURITY DEFINER`, is owned by `wamn_system`, and pins `search_path` to `pg_catalog`.
PUBLIC holds no execute grant. Only the control or administration role receives the execute grant that it needs.
Control logins can read hierarchy rows and write their declared identity and lifecycle fields. Postgres row security limits these operations to the authenticated login’s org. Connection writes still use the restricted functions.

### Application connection bindings

See [integration and connection schema](../plan/integration-schema.md).

Environment apply reads connection projections through targeted system functions.
They do not add table grants to the control or administration roles.

### Record provenance

An imported application record carries `external_system_id` and `external_id`.
The first column references `platform.external_system.id`. The pair has a unique constraint.
These columns identify the source system and its record, independently of the connection that imported it.
The REST consumer fixture records this pair alongside its local key and name.

Package verification supplies an empty relation with the public provenance key.
It reads no registry. Introspection permits this UUID reference outside the package schemas and still refuses other external table references.
Package migration grants schema access and REFERENCES only while the owner creates this foreign key, then removes those temporary grants.

Database connection configuration contains no password. One native basic-auth Secret stores each role's username and password. SQLx resolves that Secret and the cluster CA in the org namespace. Operator connections also require verified TLS.

Dynamic SQL uses a separate login that inherits `wamn_query`. This role has SELECT privileges and no application writer membership. Each dynamic call runs inside a read-only transaction. Prepared application statements retain their separate authority.
