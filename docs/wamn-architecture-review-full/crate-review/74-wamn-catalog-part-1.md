# 74 wamn-catalog, part 1

Scope: `crates/catalog/model` at `epic/dep7` (b94833fd), part 1 of 3: `lib.rs` (948), `manifest.rs` (3,081), `manifest_error.rs`, `package.rs`, `schema.rs`, `sql_lex.rs` (1,583) with `sql_lex/grammar.rs` (809), `environment_application.rs`, `feed.rs`, `test_database.rs`. Part 2: serving manifest, component facts, profile, artifact references. Part 3: host routes, wiring, connections, workflow check, signed reads. Nothing built or run.

## Earlier work checked first

- Tab 04: one definition of each scalar type, with its check, in `wamn-scalar`.
- Tab 09: compatibility with the retired flow language goes (`flow_id`, `flow_version`).
- Tab 34 A: SQLx describe at build gives the bind count; `sql_lex::parameter_count` is no longer the check.
- Tab 60: one migration role on the SQLx `Migrator`; platform scopes on `sqlx::migrate!` (5 B). Owner 2026-11-12: developer checks belong in build or dev, not apply or deploy.
- Tab 63 1 A: an application is its release; the `packages` list leaves the environment document.
- Tab 72 3 A: comments state the rule; bead ids, plan labels and history go.

## Files read

- `lib.rs`: header, module list, `CATALOG_SCHEMA_SQL`, hash newtypes, the artifact, source and attachment types, error enum.
- `manifest.rs`: header, item list, `PackageManifest::from_slice`, `validate_operation_vocabulary` (head).
- `schema.rs`, `environment_application.rs` in full. `sql_lex.rs` and `grammar.rs`: headers and public functions. `profile.rs` header.

## Callers read

- Repository grep for each public type of `lib.rs`, for `sql_lex::`, `pg_query::`, `PackageManifest::from_slice`, `validate_operation_vocabulary`, `CATALOG_SCHEMA_SQL`, `ColumnType`, `ComponentSqlValueType`, `StatementValueType`, `EnvironmentApplications`.

## One purpose per file

- `lib.rs`: hash and digest newtypes, the catalog SQL bootstrap, the retired flow artifact identity.
- `manifest.rs`: the `wamn.json` types and their validation (about 1,800 lines of validation).
- `sql_lex.rs`: a hand-written SQL lexer for build-time checks. `grammar.rs`: a property-test grammar for it.
- `schema.rs`: `ColumnType` and `ForeignKeyColumn`. `environment_application.rs`: application name to packages. `feed.rs`, `package.rs`, `manifest_error.rs`, `test_database.rs`: as named.

## Findings

**1. The flow artifact identity types have no caller.**

Fact: `ArtifactId` (with `flow_id`, `flow_version`), `ArtifactIdentity`, `SourceId`, `AttachmentId`, `SourceKind`, `Source`, `AttachmentDraft`, `Attachment` and `CanonicalJson` are used only inside `lib.rs` and by `tests/identity.rs` (grep). `ArtifactIdentity::new` says the flow language it hashed "retired". About 400 lines of `lib.rs`, the identity hashing (`IDENTITY_FORMAT`, `frames`) and the error variants for them (for example `FlowInvalid`, `GraphHashMismatch`, `UnresolvedSource`).

Options:

- A: delete them. `DefinitionHash`, `ArtifactHash` and `ManifestDigest` stay.

Recommend A.

**2. Two SQL analysers.**

Fact: `sql_lex` is a hand-written lexer that answers `relation_access`, `delete_targets`, `conflict_constraints`, `filter_predicate`, `groups_or_windows`, `parameter_count`, `contains_schema_qualified_reference` and `profile_relation_access`. Its callers are the generator (`generate/validation.rs`, `feed.rs`, `contracts.rs`, `generate.rs`) and `profile.rs`. `pg_query` (the PostgreSQL parser, `=6.2.1`) is already a dependency of the generator, `wamn-schema-control`, `wamn-package-control` and `wamn-environment-control`; the generator's own `feed.rs` uses both.

Options:

- A: every SQL check reads `pg_query` parse trees. `sql_lex.rs`, `grammar.rs` and their property tests go.
- B: keep both.

Recommend A: one parser, and it is PostgreSQL's own.

**3. The manifest vocabulary is validated in at least eight places.**

Fact: `validate_operation_vocabulary` runs in the generator (`generate.rs`, `generate/validation.rs`), dev (`dev/config.rs`, `config/plan.rs`), `wamn-schema-control::package_migrations`, release `package_sources.rs`, package admission (`component_admission.rs` twice, `component_admission/plan.rs`), `apply_package.rs` and provision `operation_grants.rs`. Provision uses its return value, the set of operation identities. The build writes `wamn.json`; publish and apply read it from a digest-checked package artifact.

Options:

- A: validate once, in the build. Other readers only parse. A plain accessor returns the operation identities for `operation_grants`.

Recommend A (owner direction 2026-11-12).

**4. `from_slice` drops retired fields instead of refusing them.**

Fact: `PackageManifest::from_slice` removes `workflows` and each custom operation's `registration` before strict decoding.

Options:

- A: delete the removal; unknown fields are refused like any other.

Recommend A (greenfield: no compatibility paths).

**5. Three enums for the same SQL value types.**

Fact: `schema.rs::ColumnType`, `component_facts.rs::ComponentSqlValueType` and `runtime/plugins/wamn_postgres/statements.rs::StatementValueType` each list the same ten cases of `wamn:postgres/types.sql-value`. `wamn_postgres::operation::lower_statements` maps one to another.

Options:

- A: one enum, in `wamn-scalar` beside the scalar values (tab 04). The mappings go.

Recommend A.

## Follows from earlier rulings

- Tab 60 6 A3: `migration_is_transactional` and `migration_statement_count_is_safe` go; SQLx reads its own `-- no-transaction` marker. Tab 60 5 B: `CATALOG_SCHEMA_SQL` (five `include_str!` files) becomes project-scope migrations; the 60 or so test sites that run it call the project migrations instead.
- Tab 63 1 A: `ApplicationDefinition.packages` and `application_for_package` go.
- Tab 72 3 A: the `lib.rs` header goes stale three ways: "MVP outcome", a bead id, and a claim that the crate holds the wiring-activation SQL (it holds none).

## Fine as is

- `DefinitionHash`, `ArtifactHash`, `ManifestDigest` (`ManifestDigest` has no `Deserialize`, so only verified bytes make one).
- `ManifestError`, `PackageCoordinate`, `ForeignKeyColumn`, `test_database.rs`.

## Tests deleted, changed or moved

- Finding 1: `tests/identity.rs` (240) and the identity tests in `lib.rs`.
- Finding 2: `sql_lex.rs` tests (about 470 lines) and `grammar.rs` (809).
- Finding 3: vocabulary refusal tests in package admission, apply and release; the build keeps its tests.
- Finding 5: mapping tests between the three enums.
- Follows: `environment_application.rs::a_package_has_one_declared_application_without_a_fallback`.

## Not read

- `manifest.rs` validation bodies; `sql_lex.rs` bodies; `feed.rs`, `package.rs`, `manifest_error.rs` bodies.

## Rulings

- 2026-11-12: all findings A (flow artifact types deleted; pg\_query is the one SQL parser; manifest validated once at build; retired fields refused; one SQL value type enum in wamn-scalar).
