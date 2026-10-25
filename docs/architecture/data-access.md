# Data access

Migration SQL defines an application's schema.
`wamn-schema-introspection` reads PostgreSQL into a normalized schema description.
`wamn-schema-generator` combines that description with exact manifest and SQL bytes to produce package artifacts.
The schema description is derived output, not an editable schema language.
The generator's core transformation performs no filesystem, database, clock, or environment access.

## Schema and definition ownership

The effective schema comes from the selected base migrations followed by the selected overlay migrations.
Each package keeps its own ordered migration stream.
A root version names no predecessor. Each later version names the current leaf as its exact predecessor.
The predecessor's paths and checksums form a byte-identical prefix of the candidate stream.
Forks, skipped predecessors, reordering, and changed inherited bytes refuse.
Before first release membership, a candidate package can correct its migration stream.
First membership seals the package coordinate and its cumulative stream in one transaction.
Later changes require a new package version, and retained records are not pruned.
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
Upgrade qualification reads the pinned schema and refuses a candidate that changes it.

The migration policy admits the supported additive table, field, and named-constraint forms.
It also admits plain indexes on tables created earlier in the package's migrations, under the table's owner.
An added column is nullable with a modeled type and no default, or it carries one of the two demanded non-null defaults.
The catalog reader supports ordinary tables, supported columns and defaults, identity properties, constraints, and supported indexes.
It normalizes identity sequences and indexes that belong to constraints instead of counting them as independent definitions.
It excludes PostgreSQL object identifiers and storage details from semantic identity.
Its exact supported forms live in [introspection](../../crates/schema/introspection/src/postgres.rs) and [migration policy](../../crates/schema/introspection/src/migration_policy.rs).

An installed successor with new migrations also requires `qualify-upgrade` evidence before production application.
Its narrower policy permits new ordinary tables and the admitted column additions, but refuses added constraints on existing relations.
Qualification copies the installed database with its data and object ownership.
It reads each SQL-bearing application's runtime schema from the selected release and requires the candidate's declared schema to match.
For a release without a schema pin, qualification uses the package's declared schema.
Predecessor SQL must plan against the candidate schema before grants change and again after the complete candidate root set reconciles.
Candidate SQL must plan under those same final grants. Planning preserves the grants it examines.

A base upgrade requires a successor for every installed overlay that pins that base.
Each overlay successor preserves its migrations and consumed operation contracts, then pins the exact candidate base version and component digest.
Qualification binds the complete predecessor and successor root sets and the candidate component bytes.
Application installs the base first, then every affected overlay, in one transaction with the accepted evidence.
A failure rolls back the complete transition. Nontransactional migration statements refuse with the named operation.

`apply-package` records accepted evidence in `catalog.package_upgrade_qualifications` within the transaction that installs the successor.
Reconciliation requires evidence that matches the complete installed package set, exact presented roots, and derived privileges.
It selects evidence by those identities, never by timestamp. Earlier qualifications remain immutable history.
Release rollback retains committed migrations and admits only the exact immediate predecessor whose accepted evidence still matches the live database.
The [package upgrade procedure](../operations/deployment.md#package-upgrades) defines prerequisites, command order, and recovery.

Package migrations cannot create roles, grants, extensions, routines, triggers, rules, or row policies.
Managed schemas refuse foreign tables, authored views, materialized views, unsupported types, and unsupported generated properties.
Nontransactional operations and mutations outside the selected schemas refuse.
The platform alone installs its declared extension list, `btree_gist` and `pg_stat_statements`, before application migration SQL.

The platform installs the record history triggers and the history tables, and the catalog reader skips them by name, as [record history](#record-history) describes.

Provisioning creates one `NOLOGIN` role per package ID before package migrations run.
`package_owner_role` in `crates/control/provision/src/name.rs` derives `wamn_package_owner_<40 SHA-256 hex digits>` from that ID.
The role owns the package schemas, tables, sequences, and generated history functions.
An overlay keeps its own role and uses the base relation owner for admitted changes to that relation.
The database owner, `wamn_db_owner`, retains platform objects.
Project migration `0037_package_owners.sql` transfers existing package objects and preserves their grants.
Publication parses backfill SQL with the same statement checks as declared component SQL.

Each managed relation, field, and constraint records its owning package.
An overlay can add a field only where the base permits that extension.
The added field remains overlay-owned even inside a base relation.
Schema ownership does not grant runtime mutation authority.
An overlay reads exposed base fields and mutates its own fields through declared operations.
Base row creation, deletion, and business-field mutation require an explicit base operation or grant.

The platform owns grants and row policies separately from application DDL.
Applications cannot replace or weaken them.
PostgreSQL uses the authenticated database role for application row isolation.
[Connection authority](capabilities.md#invocation-and-connection-authority) states how the host selects the database and the credential of each authority class.
Control-store projections retain their separate tenant-scoped policies.
`wamn-run-state` supplies the host queue worker's actual `CURRENT_USER` membership query.
The native runtime refuses absent membership at the existing guest error boundary.

## Application declarations

An optional `upgrade_stage` in the package manifest declares one online migration stage for that package version.
Its `phase` is `expand`, `backfill`, or `contract`.
Named `preconditions` and `postconditions` carry SQL directly, so the manifest identity covers their exact bytes.
A backfill also declares `sql`, a positive `batch_size`, and an explicit `initial_cursor` under `backfill`.
Batch SQL takes `$1::jsonb` for the cursor and `$2::int4` for the batch size.
It returns one row with `next_cursor jsonb` and `complete bool`.
KCL authors use a non-null cursor because the manifest compiler omits `None` values.
The explicit `whole_row_grants` exception belongs only to an expand stage.
Stage declarations require a predecessor and a package that declares SQL.

Qualification format 12 carries the stage declaration beside the package, root, and privilege identities. Formats 10 (plain) and 11 (overlay) carry none.
Expand stages support the admitted column additions on package-owned relations.
They run named conditions before and after their migration suffix in the same transaction.
Application repeats the conditions against current data and records qualification evidence only after success.
Each stage statement uses a five-second lock timeout. A lock failure aborts the transaction without an automatic retry.
Condition inspection reads relation ownership and scalar column types from the current database.
Conditions admit package-owned column references, bound parameters, comparisons, and `EXISTS`.
Functions and casts use the shared column-default policy. Defaults retain their existing restrictions.
Backfill stages retain the predecessor migration files and carry no new DDL suffix.
Each batch updates one owned relation, ordered and bounded by an immediate, non-null, single-column unique key.
The supported SQL selects a `batch`, updates those keys, then returns the cursor unchanged and whether the batch was empty.
The package predicate must exclude processed rows. Inspection does not establish that the procedure terminates.
Each successful batch commits its data and progress together. The installed version stays at the predecessor until completion and successful postconditions.
Resume uses the recorded cursor and exact qualification. A different unfinished stage or a changed stage manifest is refused.
Preconditions run before the first committed batch. A failed final postcondition preserves earlier batches and leaves the predecessor installed.
Expand migrations also admit one invoker function and its synchronization trigger together in one SQL file.
The trigger runs before each insert or update and assigns approved expressions to explicitly writable package columns.
The function body contains `BEGIN`, `NEW` assignments, `RETURN NEW`, and `END`.
It cannot write identity fields, generated fields, declared server-owned fields, or platform history and tombstone fields.
Application code writes the source column. The trigger writes its destination column without an application grant on that destination.

Immutable ownership records bind the function and trigger to their package and relation.
Before each batch, application compares inherited definitions against the exact artifact, current column types, and stored owners.
Unknown triggers, changed function bodies, elevated privileges, and changed trigger behavior are refused.
Function replacement and reuse of names with ownership history are refused.
An expand stage can declare `exceptions = ["whole_row_grants"]` for nullable column additions without defaults.
The existing data-access owner derives grants from the complete candidate roots inside the package transaction.
The platform then runs postconditions and plans the selected release's serving SQL under those grants.
Schema changes, derived grants, and accepted qualification evidence commit together.
If a condition or statement fails, the transaction rolls back all three.
The exception does not admit package-authored grants or skip statement planning.
Other staged DDL and contract execution are unavailable.

`wamn.k` declares application choices that another source cannot determine exactly.
The generator supplies fixed protocol rules and rejects authored fields for them.
Receiving, Acme, and WMS use the same declaration format.

Every package under `apps/` authors its manifest in `wamn.k`, a KCL file, and `tools/repo-lint` refuses a `wamn.json` at its root. Test fixtures outside `apps/` keep a hand-written `wamn.json`.
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
| `min_lengths` | Application value policy: the fewest characters that a text column holds after a trim. The generated codec refuses a shorter value with `invalid_input` on its field. The author writes `CHECK (char_length(btrim(<column>)) >= <n>)` in a migration, and generation refuses the model without it. That CHECK is the database guard and never the answer, so no operation contract lists it. Retain. |
| `audit_log.columns`, `.retention` | Application history policy. Retain. |
| `operations` keys, `permission`, `writable_fields`, `revision_field`, `result` | Application operation, authority, revision, and result choices. Retain. |
| Create `values` | Application value policy for a new record: the subset of an `enum_fields` list that a create accepts. The generated input contract states it, and states the model list for every other enum field, so a client and the generated route schema offer only those values. Retain. |
| CRUD `error_details` | Generator-owned error vocabulary and details, derived from operation kind and applicable database constraints. Remove from authored input. |
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
| Canonicalization `excluded_fields`, `line_order` | Application command-identity choices. A command that claims its key gets `excluded_fields` `request_id` and `value.idempotency_key` from the schema module, written only to change. Retain `line_order`. |
| Canonicalization `payload`, `uuid`, `timestamptz`, `numeric`, `duplicate_line` | Fixed platform codecs and duplicate-line refusal. Derive. |
| Custom `errors`, business `error_details`, `constraint_errors` | Application error choices and constraint mappings. A business error detail can state the `text` that a screen shows. Retain. Derive fixed details for standard platform errors, `idempotency_conflict` among them. |
| `relations[].schema`, `.table`, `.select_fields`, `.insert_fields`, `.update_fields`, `.delete`, `.lock` | SQL authority. The SQL owns what a statement reads, writes, locks and deletes, so generation derives each relation from the statements over the tables of the package's own schemas and writes it into the compiled manifest. PostgreSQL then plans each statement under the grants it derives. Derive. |
| `relations[].constraints` | The constraints a statement relies on: each one its SQL names in `ON CONFLICT ON CONSTRAINT`, and each one `constraint_errors` maps, on the reached relation that defines it. Derive. |
| `statements.*.path`, `.fetch`, `.parameters`, `.row` | Static SQL selection and accessor contract. PostgreSQL checks types, privileges, and transaction requirements. Retain names and declared shape. |
| Parameter/row `name`, `type`, `nullable` | Accessor names and representation. SQL planning does not determine all application names or nullability contracts. Retain. |
| `connections` aliases | Application database-capability selection. Retain alias names as an array. The PostgreSQL interface is fixed, so remove repeated interface records. |
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

Generated codecs handle JSON entry, typed input validation, primitive conversion, correlation, and declared error details.
Their `export_operation!` macro connects a typed application handler to `run` and `run-json`.
Known component calls use typed values.
HTTP and dynamic entry use the JSON adapter.
Revision inputs, returned revisions, and conflict revisions use decimal strings in JSON.
Database and WIT revision values remain numeric.
The `minimum`, `maximum`, and `observed` refusal details are decimal strings in WIT and JSON, spelled as a numeric value is and never as a float.
Custom fields declare `revision: true`; CRUD operations use their declared revision field.
Other integer fields remain JSON numbers.
The common integer codec preserves exact values above JavaScript’s safe-integer limit.
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
2. `generated/data/error.rs` holds that refusal type. It spells the closed vocabulary of the operation contracts, and its details are the declared detail fields.
3. `generated/component/<model>.rs` holds one module for each operation, with its codec, its handler, and its export.
4. `generated/wit/world.wit` holds the world `<package>:generated/generated`, which exports every operation of the package and imports what those operations call.
5. `generated/publication/attachments.json` holds one route entry for each generated operation, with its definition hash.
6. `generated/publication/component-operations.json` holds one declaration entry for each generated operation, keyed by component.
7. `generated/data/sequences.rs` holds `sequences::<name>::next` and `sequences::<name>::peek` for each sequence that the package declares `callable`. The package has no such file without a callable sequence.

The shared crate `wamn-data-access` holds the keyset cursor, the query page, and the scalar parsers that the data functions use.
A data crate includes `data/mod.rs` of its build output as its public module `generated`, through a path relative to the crate. The generated files include each other by paths relative to themselves.
A component includes `generated/component/mod.rs` in a module that names that data module `data`.
The fixture overlay has no data crate, so its component includes the data functions itself.

The authored world, in `component/wit/authored.wit`, only includes the generated world.
The authored world includes the generated one, and the reverse is not possible.

The authored `publication/attachments.json` and component declaration carry only the custom operations.
Publication, the declaration render, the client generator, and `materialize_package` read the authored file and the generated file together.
An attachment id or an operation in both files refuses.
A copy of an authored document outside its package reads the generated file beside it only when the copy keeps the package layout.

### Operation references

The package version is authored once, as `package.version` in `wamn.k`.
A sealed operation id is `<package>:<interface>/<operation>@<version>`, for example `wamn-receiving:location/list@2.0.0`.
WIT packages, component worlds, contracts, the files under `generated/`, the clients, grants and the statement sets of the host carry the sealed id.

An authored route entry, component declaration or wiring names an operation by its reference, `<package>:<interface>/<operation>`.
A declaration template authors no `scope.package-version`.
A reference to an operation of the package takes `package.version`, and a reference to an operation of a base takes the `version` of that base in `base_dependencies`.
A platform interface, such as `wamn:node/handler@0.1.0`, is not a package operation and keeps its WIT version.

Every reader of an authored file resolves its references with `wamn_schema_generator::operation_reference` before it uses the file.
The readers are the generator, publication, the declaration render, the author-wiring verb, the dev coordinator and the composer.
An authored id that carries a version, or a template that authors `scope.package-version`, refuses.
A web page keys `onOpen` and `onFill` by the reference, so one page shows one version of each package.
A test gets the sealed id and the package version from the compiled `wamn.json` through `wamn_test_infrastructure::operations`.
A new package version is one line in `wamn.k` and a regeneration.

The manifest member `routes` sets how a generated route is published:

- `auth_modes` is the authentication modes of each route. It defaults to `["pat", "session"]`.
- `path_prefix` starts each path, such as `/acme`. It defaults to none.
- `id_prefix` starts each attachment id. It defaults to none, which gives `<model>-<action>-http`.

A query that names an undeclared sort refuses with `invalid_input` on the field `sort`.

Native SQLx tests compile the exact files that guest execution names.
Guests send statement identities and arguments through `wamn:postgres`.
The `statements` interface uses native async calls for execution, transaction creation, commit, and rollback.
Version 0.4.0 includes `run-stream`, `operation-transaction`, `next-number`, `peek-number`, and the separate `operation` interface. It is the one version, and the host serves only it.
The host begins the transaction of one logged operation item, and `operation-transaction` gives it to the guest. Inside that operation, `begin` refuses with SQLSTATE `25001`, and `commit` or `rollback` of the host transaction refuses with `2D000`.
An unnamed `statements` import runs the statement set of the active operation.
In a composed component each member has its own named import, and the host links one instance per name.
The host keys the active statement set by the component scope and the import name, so a member that names another member's statement gets `unknown-statement`, and so does a read (get, query or projection) that names a statement that writes or locks rows.
Every instance lends the same host transaction. Only the statement set differs.
The host resolves those identities to admitted SQL bytes and the operation's allowed statements.
Generation refuses PostgreSQL values that the production `wamn:postgres` type contract cannot represent.
SQLx metadata is compilation data, not the SQL or package contract.

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
Generation compares these declarations with the SQL.
Projections and history tables refuse delete access.

Generation plans every authored and generated statement as `wamn_app` under exactly the grants that the package declaration derives.
PostgreSQL checks column privileges at plan time, so a statement that reads a column outside those grants fails at generate time.
If the grants do not cover every column, the check refuses a whole-row reference such as `to_jsonb(item)`, `RETURNING item`, or `(item).sku`.
The refusal lists the path, SQLSTATE, and PostgreSQL message of each refused statement, for example `42501 permission denied for table item`.
The check runs in one transaction that rolls back, so the generation database keeps its roles and privileges.
The [statement check test](../../crates/schema/generator/tests/statement_check_live.rs) adds a whole-row cast of `widget` to a small platform package, and the check refuses it.

Generation also refuses a statement that takes a different number of parameters than its generated accessor binds.
A statement takes parameters up to the highest `$n` it references outside comments, strings, and quoted identifiers.
The refusal names the statement path, the accessor, and both counts.

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
It has no tenant column and no row policy, because the database is the tenant, and no record history: the rows the work writes carry the actor.
`wamn_app` holds `INSERT`, `SELECT`, `UPDATE (result)` and `DELETE` on it.
Its key is the claim operation without its `@version`, for example `wamn-wms:location/create`, and the idempotency key.
The key is the input field that the route's `idempotency` names: `idempotency_key` for a create, and `value.idempotency_key` for a command item.
An update, a delete, and any other route without a key field key each item by its `request_id`.

The current Receiving path claims through the engine.
It runs the intent rules of [`intent.rs`](../../crates/platform/engine/src/operation/intent.rs) around every create, update, delete and command, in the cloud and on the edge.
No codec, data function or component spells a claim.
The engine calls the component once for each item, with a one-item list.
For an operation with SQL, the host begins one transaction for each item, and the steps of that item run inside it:

1. The host opens the transaction with `BEGIN ISOLATION LEVEL READ COMMITTED`, because the retry below reads the row that another transaction committed.
2. The engine claims the key with an insert that does nothing on conflict. An uncommitted claim of another transaction makes the insert wait until that transaction ends.
3. If the insert claimed nothing, a committed claim holds the key. The engine reads its request and result. The same request answers the stored result, and another request answers `idempotency_conflict` with the detail `{field, intent}`. The host rolls back, so the retry runs no work.
4. If the insert claimed the key, the component does the work in the same transaction. The engine stores the result in the claim, and it commits with the work. A refusal rolls back, and the claim goes with it.

The host pipelines `BEGIN`, local session claims, and the write-log claim before it runs the component.
After the component returns its result, the host queues the finish, causation message when present, and `COMMIT` together.
The guest statement needs its own trip because the finish stores the returned value.
The [execution path](execution.md#native-dispatch) describes the statement and round-trip counts.

The component reaches that transaction with `wamn_postgres_statements::operation_transaction()` of `wamn:postgres` 0.4.0, and its data access takes the `Transaction`.
A `begin` inside such an operation refuses.
An operation without SQL claims in two steps on a host connection of its own: begin before the call, and finish after it.
A begun claim has a null `result` until it finishes.
A claim that never finishes answers `intent-uncertain`, and so does a commit that the host sent without reading its answer.

A refused item leaves no row, so its key is free again, and a later different request under that key does its work.
A retry that answers a stored result commits nothing, so no event publishes a second time.
A new id comes from its insert's `RETURNING`, and nothing mints an id before the work.
No application table implements or references the claim. The stored result names the rows that the work wrote, for example the `transaction_ids` of a WMS command.

The host also provides `wamn:operation/operation@0.1.0` for the composed application entry.
Receiving calls this interface through its composed HTTP entry.
The host binds its workload to a component in the loaded release before serving calls.
Only the composed entry imports `wamn:operation`.
The control-only entry links this interface without operation authority.
Its begin calls return `unknown-operation`, and its commit, rollback, and WAL calls return `not-begun`.
Package members that import `wamn:postgres/statements` need only the PostgreSQL WIT package path.
The `wamn:postgres@0.4.0` package has no dependency on `wamn:flow-http-routing`.

`begin-operation` takes the opaque caller handle from `authenticate`, the sealed operation id, an idempotency key, and request hash bytes.
The caller is optional only for an operation that registers no grant in the loaded release.
An absent caller on a granted operation returns `grant-denied`.
The host checks the handle, release facts, and caller grant, then mints a delivery id.
It returns that id as `begun::new(string)` for the application's operation context.
The application reads the id and never supplies it.
The host pipelines the claims and write-log insert in a new transaction for that operation.
The host stores the hash bytes unchanged and returns stored result text unchanged as `begun::finished` on replay.
The entry returns that text and runs no work. `finished` leaves no operation open, so another begin is allowed immediately.

The host projects each base route's operation id and idempotency key path through `wamn:flow-http-routing/routing@0.1.0`.
The request-hash rule declares excluded paths, line order, and numeric or timestamp spelling.
It excludes `request_id` from the hash and keeps the key.
The guest applies spelling and line order before invocation, then excludes the declared paths only for hashing.
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
`authorize` takes the caller handle and the sealed operation id, checks the caller and its grant, and binds the statement authority.
It opens no transaction and claims no write-log key.
`finish-read` ends the authorized read and records its delivery outcome without SQL.
`begin-operation` on a read operation returns `reads-claim-nothing`.
After `authorize`, `commit`, `rollback`, and `wal-position` return `not-begun`.
A read writes no causation message, so it does not invalidate a list tag.

This interface returns `uncertain`, `resolved(string)`, `conflict`, `unknown-operation`, `grant-denied`, `invalid-caller`, `not-begun`, `already-begun`, `postgres(pg-error)`, or `reads-claim-nothing`.
The `resolved` payload carries the operator basis text for the existing `intent-resolved` response.
The write log supports no operator resolution, so it produces no `resolved`.
An unread commit answer returns `uncertain`.

The request is the SHA-256 of the canonical item without `request_id`, with the key kept.
The route carries the canonical form of its input item (`canonicalization`), which publish reads from the generated input contract.
The engine applies it before it hashes an item and before the component gets it.
It spells each `numeric` leaf as PostgreSQL does, with its scale kept, and each `timestamptz` leaf in UTC with six fractional digits.
It orders the declared lines, and the declared exclusions leave the hash.
The component's normalize keeps only its checks.
The result is the answer body of the item, `{"value": ...}`, without its `request_id`.

Production-claim errors retain PostgreSQL's primary message and constraint name.
They omit `DETAIL` and `HINT`, which can include failing row values.
Run rows contain application JSON.
This rule matches platform identity errors. Primary messages remain verbatim.

Commands keep network effects outside transactions that hold database locks.
Post-commit work uses the [event path](capabilities.md#events).

A generated query returns its page as one result: its rows, and the keyset cursor that continues the read.
For a cursor, SQL compares the sort field and ID as one row, so a matching index starts at that cursor.
`wamn build` requires a `(field, id)` index for every allowed sort field, including the default.
It reads indexes across all package migrations and refuses a missing index with the operation, table, and exact `CREATE INDEX` line.
Its `run-json` entry returns the page as the outcome list of one item, and its `read-json` entry also returns the WAL position.
The generated `query` action declares a `limit` that defaults to 100 and accepts 1 to 100.
The query's codec fills the default and refuses a limit below 1.
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
Arbitrary tenant SQL remains subject to runtime authority and execution limits.
Compile-time checking for arbitrary tenant components remains demand-gated.

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
Date parts use the `timezone` setting of the application scope, and UTC when no setting exists.
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
It never reads the `app.application` claim, because guest SQL can rewrite that claim.
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

The plugin checks every key and, for a pattern that reads the date, the `timezone` setting before the first counter call, so a refused call advances no counter.
The time zone check runs in a savepoint, so its refusal leaves the operation transaction usable.
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
The check reads only the package declarations, so a row with an edited pattern does not refuse.
`wamn dev` reads the `environment.k` beside `dev.json` and creates the same rows.
If the file is missing, `wamn dev` refuses and names the path that it expects.

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
No component derives grants for a fixture as it does for user fields.
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
The [claims lint](../../tests/conformance/src/repo_policy/session_claims.rs) admits those readers by name, and it admits `wamn_history.log_row_change` as the reader of `app.operation`.
It also admits the two function templates of `apply_package/record_history.rs`, which generate the functions of the package relations, and the actor stamps of generated statements.

After the migrations apply, apply-package installs one `wamn_record_history_stamp` trigger on each owned relation whose declaration selects at least one column.
The trigger runs `BEFORE INSERT OR UPDATE` for each row and executes the generated function `wamn_history.<schema>__<relation>_stamp()`.
apply-package generates that function from the selected columns and the other columns of the relation, which it reads from the catalog after the migrations.
The function assigns each selected column, and it tests a change with `NEW.<column>::text IS DISTINCT FROM OLD.<column>::text` for each other column.
It reads no catalog and builds no JSONB. It follows the rules of `wamn_history.stamp_row`, which the platform relations keep.
apply-package replaces a generated function only when its stored source differs, and it refuses a name of 64 bytes or more with `record-history-function-name-too-long`.
A column change that does not run through apply-package leaves the function on the old column list until the next apply.
apply-package installs no trigger for `"columns": []`, and it removes a stamp trigger that the declaration no longer selects.
It then reads each trigger of the owned relations as the text that `pg_get_triggerdef` renders.
It refuses with `record-history-trigger-mismatch` when that text differs from the text that the declarations derive.
A trigger that no declaration derives also refuses.
Development package reconciliation runs the same step.
The catalog reader skips the `wamn_record_history_stamp` and `wamn_record_history_log` triggers by name.
Every other trigger refuses.

### History tables and the log trigger

Each relation whose retention is not `"none"` keeps a log in its own history table.
The history table is `<relation>_history` in the schema of the relation.
`wamn_history.create_history_table` in `record-history.sql` is the one definition of the table shape.
A package relation gets no `tenant_id` column.
`wamn_history.log_row_change` is the `AFTER INSERT OR UPDATE OR DELETE` row trigger function that writes one entry for each row change.
It writes the entry in the same transaction as the change.
It writes with the authority of the writer, so the `current_user` tenant floor also governs the entry.

An entry has these columns:

- `position`: the per-row position, a `bigint` identity value that the function allocates under the row lock. Positions rise for each row, and gaps are allowed.
- `id`: the id of the entry, a random `uuid` that PostgreSQL generates. A read that shows entries in a table keys its rows on it.
- `tenant_id`: the tenant of the row, only in a history table under a tenant floor.
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
Package provisioning grants the same history privileges and schema `CREATE` to each package owner, which owns its generated functions.
[`deploy/sql/record-history-app-grants.sql`](../../deploy/sql/record-history-app-grants.sql) grants the same three privileges to `wamn_app`.
`CATALOG_SCHEMA_SQL` composes it after `record-history.sql`.
Every other tenant or package applier applies it after `record-history.sql`.
The file names `wamn_app`, so an applier without that role fails.
The system database never applies it, so the system database has no `wamn_app` grant.

Generation refuses these declarations:

- A model table, an internal relation table or key, or a custom operation relation whose name ends with `_history`. A custom operation can read a history table, but it cannot declare an insert, an update, or a row lock on one.
- A logged relation whose longest derived history object name has 64 bytes or more. The longest name is `<relation>_history_transaction_id_not_null`, so the name of a logged relation has 31 bytes or fewer. The refusal names the relation.
- A logged relation with no primary key. Each entry keys the row by its primary key columns.

No generated `update` or `delete` exists over a history table, so no application operation can alter an entry.

After the migrations apply, apply-package creates each logged relation’s history table as its package owner.
apply-package never drops a history table.
It records the CDC exclusion of each history table in `wamn_cdc_exclusions`, as it records an internal relation.
The exclusion row names the package that owns the relation, and its relation id is the history table name.
A migration that creates a table whose name ends with `_history` refuses with `history-table-name-reserved`.

apply-package installs one `wamn_record_history_log` trigger on each owned logged relation.
The trigger runs `AFTER INSERT OR UPDATE OR DELETE` for each row and executes the generated function `wamn_history.<schema>__<relation>_log()` with one argument, the retention.
The function names the primary key columns, every column of the relation, and the history table as static SQL, so a write reads no catalog and plans no dynamic statement.
It ignores the argument, which the retention run reads. A platform relation runs `wamn_history.log_row_change` with the same argument.
A retention of `"none"` removes the trigger, and the history table and its entries stay.
apply-package compares the text of each installed log trigger with the declared text, and it replaces a trigger whose retention argument differs.
It then refuses a result that still differs.
Development package reconciliation runs the same steps.

The catalog reader skips every relation whose name ends with `_history`.
It reads none of its columns, constraints, indexes, sequences, triggers, rules, or policies.
The schema state id therefore stays the same when a relation starts or stops logging.

The generator recognizes the history table of each logged model by its name, and it adds no history table to the catalog.
It takes the history columns from `HISTORY_COLUMNS` in the [`wamn-record-history`](../../apps/platform/data/record-history/src/lib.rs) crate.
Authored SQL validation, the SQL lexer, the required schema contract, and grant derivation use that column list.
A custom operation cannot require or map a constraint of a history table.
The generated data-access evidence grants `wamn_app` `INSERT` on every entry column of the history table, and no update or row lock.
A model with a change feed ([execution](execution.md#change-feed)) also grants `wamn_app` `SELECT` on `transaction_id`, `row_key`, `type` and `before`, the columns its changes statement reads.
The changes statement reads entries at or after the cursor transaction.
One snapshot supplies xmin, xmax, and the visibility test.
It includes entries below xmin and visible entries below xmax.
Rows below xmin page first, and visible rows at or above xmin fill the remaining page space.
A page that carries any such row completes the pass and leaves its next cursor at xmin without an id.
`create_history_table` also creates an index on `transaction_id`, and apply-package runs it for a present table too, so an older table gains the index.
The log function is `SECURITY INVOKER`, so the writer needs that grant.
The reconciler reads the history table like any other package relation and keeps that grant.
The generation database creates the history tables before `EXPLAIN` and SQLx prepare, as [running tests](../operations/running-tests.md#application-generation-and-sqlx) describes.

### Retention task

A declared `"P<n>D"` retention is a minimum keep window.
It promises nothing about removal time, backups, holds, or erasure.
The retention task removes an expired entry on its next daily run, so its schedule sets the retention precision.
[`audit-retention.example.yaml`](../../deploy/platform/audit-retention.example.yaml) is an example daily CronJob.

The [`prune-record-history`](../../crates/control/lib/src/prune_record_history.rs) verb of `wamn-ctl-ops` runs the task.
It connects as a scoped generation of the tenant-scoped audit retention family, whose stable role is `wamn_audit_retention`.
It refuses any login that is not an audit retention generation for `--tenant` in the connected database.
The family is not a `wamn_platform` member, because no reader needs the edge.
`catalog-schema-prefix.sql` creates the stable role under the role bootstrap lock, beside `wamn_platform`.
The system database schema creates no audit retention role.

The verb reads the retention of each relation from the `wamn_record_history_log` trigger argument in `pg_trigger`.
It skips an `unlimited` relation.
Each relation runs in its own transaction, which binds `wamn:audit-retention` as the actor and the operation.
The transaction takes the audit retention advisory lock before it reads `pg_trigger`.
apply-package takes the same lock before it reconciles the log triggers, so a retention change waits for a running delete.
The cutoff is `transaction_timestamp()` less n days, in UTC.
The verb deletes an entry older than the cutoff only when no earlier entry of the same row is at or after the cutoff.
It therefore removes a prefix of the history of a row, never an interior entry, and it writes no marker.

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
The [execution page](execution.md#native-dispatch) lists the principal that the host binds for each kind of delivery.
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
The database is the org boundary, so `platform."user"` has no `tenant_id`, no row policy, and no tenant key index. Its key is `id`, and `email` is unique in the database.

The [platform principal owner](../../crates/control/provision/src/platform_principals.rs) renders the platform rows of a project database.
It refuses a platform domain that is not a valid domain name.
Its SQL binds `wamn:provisioning` first, so the `wamn:provisioning` row stamps itself.
The development environment, the verification world, and `tools/identity-jwks-journey-run` apply that SQL.

The [platform configuration writer](../../crates/identity/platform/src/configuration.rs) writes the platform configuration rows of one environment.
The writer runs on the environment's `wamn_administration` login, in one transaction that binds the caller and the operation, so each row write appends its history entry.
The writer keeps a row that already holds the given values, and it removes no row.

Every relation in [`deploy/sql/app-schema.sql`](../../deploy/sql/app-schema.sql) carries the four stamp columns as `NOT NULL` and a `wamn_record_history_stamp` trigger.
These relations are `user`, `role`, `user_role`, `permission`, `api_key`, and `environment`, and the platform configuration tables `external_system`, `connection`, `integration_allow`, `project_connection`, `number_sequence`, `number_counter`, and `setting`.
`environment` holds at most one row, the status of the environment that the host reads. No row means active.
An integration is connector code, and `integration` columns hold its catalog code. A connection is a configured instance of an integration. An external system is the persistent identity that a group of connections reach.
Each relation except `number_counter` also has a `wamn_record_history_log` trigger with the retention `unlimited`, and a history table.
`number_counter` has no history table, because one entry per counter advance would double the writes of every create. The write log is its audit.
The database separates environments. The platform account and configuration tables and their history tables have no `tenant_id`, row policy, or tenant key index.
`role` keys on `name`. `user_role` keys on `(user_id, role_name)`, and `permission` keys on `(role_name, permission)`.
Their foreign keys reference one column. `api_key` keys on `id`, and `key_hash` is unique in the database.
`environment` uses a boolean `id` primary key constrained to `true`. Its default is `true`, so it holds at most one status row.
No status row means active. The application login reads that row from its selected environment database.
Project migration `0038_administration_database_boundary` removes the remaining tenant columns and rebuilds these keys.
The migration preserves rows and history images. It removes `tenant_id` from history keys and gives environment history the key `{"id": true}`.
A duplicate resulting key refuses the whole migration. Historical migrations stay unchanged.
The shared control, catalog, and run tables retain the `org--project--env` key named `tenant`.
Event fields and credential hashes retain their existing tenant representation.
Auth invalidation keys still include the environment key, so equal principal IDs in separate environments keep separate cached grants and status.
`wamn_app` holds `USAGE` on `platform`, `SELECT` on `user`, `role`, `user_role`, `permission`, `environment` and the seven platform configuration tables, `EXECUTE` on `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter`, and the write log grants. It holds nothing else in `platform`: no grant on `api_key`, on a history table, or on `schema_migrations`.
`platform.bump_counter(sequence, counter, key)` is `SECURITY DEFINER`. It inserts the counter row at 1 when the row is absent, returns the value, and increments it, under the row lock. It checks no tenant and reads no claim. It refuses an id that is not a sequence with `undefined_object`. The `wamn:postgres` plugin decides which sequences a caller reaches: it takes the application from host state and resolves the sequence id by that application and the sequence name. Component admission parses each statement a package declares with PostgreSQL's grammar (`pg_query`). It refuses a statement that names a counter function, writes a `platform.*` relation, or holds a `DO` block, a function body, a SQL `EXECUTE`, `SET`, `RESET`, `set_config`, `pg_logical_emit_message`, `query_to_xml*`, `cursor_to_xml*` or a `U&` escape (`crates/control/lib/src/component_admission/declared_sql.rs`). Publish checks the pushed descriptors again. Application guests run only declared statements, so only the plugin calls these functions. The host also runs a text guard (`reject_claim_mutation`) on every statement it runs: on declared statements, which covers a release admitted before the parsed check, and on the raw `wamn:postgres/client` interface, which only platform components import.
`platform.assign_counter(sequence, counter, key)` is `SECURITY DEFINER` and checks the sequence id in the same way. It gives a `per=` counter one value for each key: under a transaction advisory lock on the key, it returns the value of the key row, or takes the next value of the `''` allocator row and inserts the key row with it. It refuses the `''` key with `invalid_parameter_value`.
`platform.set_counter(sequence, counter, key, value)` is `SECURITY DEFINER` and checks the sequence id in the same way. It is the write of the `number_counter.set` host route: under the row lock, it inserts the counter row with the value or sets the value of the row. It refuses a value below 1 with `invalid_parameter_value`.
Project migrations 0024 to 0031 bring an installed database to the schema of `app-schema.sql`.
They rename the schema `app_system` to `platform` and the account tables to their singular names.
They add the platform configuration tables and the three counter functions, and they drop the tenant floor of `user` and the configuration tables.
`upgrade-schema` applies each migration once in one transaction. A fresh install records them as applied.
The administration family role `wamn_administration` holds `SELECT`, `INSERT`, `UPDATE` and `DELETE` on `user`, `role`, `user_role`, `permission` and `environment`, and on the platform configuration tables `external_system`, `connection`, `project_connection`, `integration_allow`, `number_sequence` and `setting`.
It holds no grant on `number_counter`: only `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter` write it.
It holds `INSERT` on the entry columns of their eleven history tables, `USAGE` on `wamn_history`, and `EXECUTE` on `wamn_history.row_image`. No entry column names a tenant, and administration holds no tenant derivation grant.
It holds `USAGE` on `catalog` and `SELECT` on `catalog.releases`.
The CLI role writer checks the target database against the registry and needs the administration grants. It requires no superuser or row-security bypass.
It refuses a tenant key that differs from the registry environment key, so role writes invalidate the selected environment's cached grants.
The session role reader holds `SELECT` only on `user.id`, `user.status`, `user_role.user_id`, and `user_role.role_name`.
Its connection selects the environment database. Its query filters by principal ID and active status.
The grant goes to this family only. `wamn_platform`, the group that every platform family inherits, holds none of it.
One login of the family serves each environment, and two hosts hold it: the application host of the environment and the control host of its org.
The audit retention role holds no grant on these history tables, because their retention is `unlimited`.
A foreign key cascade writes its delete entries with the actor and the operation of the deleting transaction.
The log copies full rows, including `api_key.key_hash`.
A column that is secret at rest does not belong in a stamped relation.
An applier installs `record-history.sql` and then `record-history-app-grants.sql` before `app-schema.sql`.

### System database

The system database owns `registry.integrations`, the connector catalog, and `registry.target`, the deployment target identities.
System migration `0029_integrations_and_target` installs both tables. A fresh install records the same migration as applied.
The catalog seeds `http`, `gcs`, and `s3`, each with `form = host` and `exclusive = false`.
The target table stores `id`, unique `name`, `type` (`local` or `cloud`), `status`, and nullable `last_seen_at`.
It seeds one active target named `cloud`, with no check-in time. Repeated schema upgrades create no additional seeds.
Neither table exists in an application database, and neither grants application access.
The target identity adds no environment assignment, `target_application`, or platform connection.

The system database (`wamn_system`) stamps its identity authority relations.
These relations are `identity.principals`, `identity.org_memberships`, `identity.org_roles`, `identity.project_roles`, `identity.project_env_memberships`, and `identity.pats`.
Each relation carries the four stamp columns as `NOT NULL` with no default, and a static `wamn_record_history_stamp` trigger.
The registry, the session keys, the operations tables, and the control store carry no stamps.
In the system database, the actor is an `identity.principals` id.
The system identity tables record the actor only. A control route binds `app.operation` to its sealed operation id, and no column stores it.
The system database keeps stamps only and no history table, as its [limits](#limits) state.

The `SYSTEM_SCHEMA_SQL` composition in [provisioning](../../crates/control/provision/src/lib.rs) installs `record-history.sql` before [`deploy/sql/system-schema.sql`](../../deploy/sql/system-schema.sql).
`record-history.sql` grants to `wamn_db_owner`, so an applier that runs as `wamn_system` creates that role first.
`SYSTEM_SCHEMA_SQL` does not carry `record-history-app-grants.sql`.

`identity.principals.type` is `user`, `service`, or `platform`.
A `platform` row carries its `wamn:<component>` name in `subject` and in `display_name`, and its derived id.
The `principals_platform_principal_check` constraint pins that subject, display name, and id.
It also refuses a `wamn:` display name on another kind, and the subject pattern of another kind refuses a colon.
Only `wamn:provisioning` writes in the system database, so the schema creates only its row.
The schema binds `wamn:provisioning` for the transaction that creates the row, so the row stamps itself.
A platform principal cannot authenticate.
The `identity.pats` foreign key carries the principal kind, and a CHECK refuses a token for a platform principal.
Every path that turns a stored principal into a caller also refuses the platform kind.

The identity issuer and wamn-ctl bind `wamn:provisioning` in each write transaction.
wamn-identity issues each token in its own transaction.
wamn-ctl creates service principals, assigns project roles, grants and revokes memberships, and revokes tokens in the same way.
A drop tells hosts to discard a cached authorization fact.
Service creation and project-role assignment record identity drops.
Provisioning publishes each drop after its transaction commits, including the role assignment before PAT issuance.
Package apply and local configuration reconciliation record a drop when they create the built-in `admin` role.
Their transaction owners publish the drops after commit through `AuthChanges`.
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

The SQL lexer reads a relation or alias that stands alone as a function argument, such as `wamn_history.row_image(<alias>)` or `to_jsonb(<alias>)`, as a read of every column of that relation.
The read therefore declares every column of that relation, and the generated grant covers the whole-row reference.
The lexer reads no column from any other whole-row reference, such as a cast. The PostgreSQL statement check refuses such a reference when the grants do not cover every column.

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
`env apply` applies `app-schema.sql` to the project database and writes its platform rows.
It writes a service row for each service principal of the project.
The release stage also creates one service principal for each application with a scheduled workflow, scoped to that environment.
The stage writes its tenant service row without assigning a role or permission.
It reads the platform domain from `registry.meta.platform_domain`, and it refuses while that value is unset.
It creates the `platform.number_sequence` row of each sequence that the packages declare, as [Numbering](#numbering) describes.
It writes a user row for each user with a membership in that project environment, and removes a user row whose membership is gone.
It writes the `admin` row of each member who holds `project-admin` in the project.
`wamn-ctl-ops create-user` creates that user in `identity.principals`, and it is the only path that creates one.
The verb ships in the operational binary, and only a platform operator runs it.
One email address names one principal for the whole platform.
A user who works in two organizations holds one principal and two memberships.
A user row carries the email of that user from `identity.principals`.
The control host writes the user row and the `admin` row when its route grants them, and removes them when its route revokes them, with the administration login, bound to the caller and the route's sealed operation id.
The application host writes roles, permissions and user roles through its [host routes](execution.md#host-run-routes) with the same login and the same binding.
A member that `wamn-ctl grant-project-env-membership` or `wamn-ctl invite` adds gains the row at the next `wamn-ctl env apply`, because those verbs hold no tenant connection.
A principal with no user row cannot write in the tenant.
The invocation bind refuses it.
`WamnPostgres::bind_session_claims` refuses a principal that owns no `platform."user"` row in the project database.
The caller sees `permission-denied`.
The host's [auth memory](execution.md#auth-memory) answers the check, and a miss reads the row.
A removed row stays accepted until a change message clears the memory or 60 seconds pass.
There is still no foreign key on a stamp column.
Modified application SQL can forge actor attribution and `app.operation`.
Record history gives no tamper resistance against modified application code or administrative SQL.

A very large `"P<n>D"` day count stops the retention verb at that relation, and the verb prunes no later relation of that database.

The statement check of one package runs under the grants of that package alone.
If an overlay adds a column that no installed operation reads, a whole-row read of that relation fails at run time with permission denied.
No current statement reaches that case.

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
Project migration `0021_remove_model_versions.sql` removes `wamn_cache`, its counter table, its functions, and their dependent application triggers.
Fresh databases install none of these objects, and apply-package installs only the record history triggers.

## Canonical values and SQL names

`wamn_execution_contract::canonical_json_bytes` owns durable command, cursor, and package-record JSON bytes.
Input canonicalization precedes idempotency hashing.
It normalizes representation without changing values.
Timestamp spelling, UUID case, and key order normalize, while PostgreSQL numeric scale remains significant.
A changed numeric scale remains a changed command body.
A generated update compares each supplied field with its current value as PostgreSQL text.
A numeric scale-only change, such as `1.0` to `1.00`, is therefore a change that increases the revision.

Keyset pagination uses `id` as the total-order tie-breaker in the primary sort direction.
Descending order reverses the compound order.
Cursor timestamps use UTC RFC 3339 with exactly six fractional digits.
Cursor numerics preserve their PostgreSQL scale.

Constraint and ordinary-index names follow this pattern unless an explicit shorter name is required:

```text
<table>_<column_1>[_<column_n>]_<kind>
```

Columns appear in table-definition order.
Kinds are `pkey`, `key`, `fkey`, `check`, and `idx` for primary, unique, foreign, check, and ordinary-index objects.
Names must contain fewer than 64 bytes.
The author supplies an explicit shorter name when needed. Tools do not silently abbreviate or accept PostgreSQL truncation.

For database preparation and SQLx commands, use the [operations pages](../operations/README.md).
Test database isolation has one definition in [running tests](../operations/running-tests.md#test-database-isolation).
