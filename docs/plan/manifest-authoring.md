# Manifest authoring

Updated through: 2026-10-02, `main` at `6c9241c9a`. Accepted by the owner on 2026-10-02, with the rulings recorded in §7.

**Scoping rule.** This document names two epics. Only epic 1 is scoped to issues. Epic 2 is named with its goal and boundary and nothing more, and is scoped only after epic 1 is closed and reviewed by the owner. An agent that finishes epic 1 stops.

## 1. Goal

A package author writes the manifest in a configuration language with comments, defaults and schemas, and the generator compiles it to the canonical `wamn.json` it reads today. The manifest vocabulary and its reader do not change: the compiled JSON is the file that `apply-package` applies, `publish-release` hashes and the generator checks. What changes is the author's file: a formulaic 2,000-line JSON with no comments becomes a typed document a third of the size, where a wrong value fails at compile time and the reason for a decision can be written beside it.

## 2. Fixed rules

- The canonical `wamn.json` stays the sealed artifact, and every reader of it is unchanged by this epic. It moves from an authored file to a generated one, `generated/wamn.json`, checked into the repository like the TypeScript bindings and compared by the generator on every run.
- Sealed bytes never change. A conversion writes new bytes, so the conversion of a package lands as that package's next version (§5).
- The Retain/Derive table of `docs/architecture/data-access.md` §"Manifest declarations" does not move. Everything it retains is still written by the author: `relations`, `statements` and their shape, parameter and row names, `fetch`, `permission`, writable fields, revision fields, input and result fields, canonicalization choices, business errors. The language fills only what the table already calls fixed or conventional.
- The compiled JSON states what today's authored files state. A key that the reader defaults when it is absent (`visibility`, `connection`) stays absent, as today. The schema's defaults are authoring defaults, not output: a default fills only a value that today's files write out. The authoring defaults of epic 1 are: `connection: postgres`; `visibility: public`; `transaction` and `automatic_retry` to their common values; `permission` to `<model>.<operation>`; `schema` to the package's one schema; `nullable: false` on a typed field; a statement's `path` from `command/<operation>/<statement>.sql` and `query/<name>.sql`; the three platform-supplied command inputs; the platform error list and the standard `error_details` of the not-found and conflict errors; `audit_log.retention: none`; the query `pagination` and `limit` blocks.
- The generator refuses a hand-edited `generated/wamn.json`: the compiled bytes must equal the checked-in bytes, as `generated/` is checked today.
- One language. The spec picks it in §4.1 and no second syntax is admitted.
- Plain English names in the schema module; `type` never `kind`; no plurals that the manifest does not already use.
- No new manifest vocabulary. A declaration the compiled JSON cannot already express is epic 2.

## 3. Current state

Measured on `main` at `6c9241c9a`.

| Place | Today |
| --- | --- |
| Authored manifests | `apps/wamn_wms/wamn.json` 2,098 lines; `apps/wamn_receiving/wamn.json` 1,337; `apps/client_acme_receiving/wamn.json` 805; `apps/platform_fixture/wamn.json` 704; `apps/edge_samples/wamn.json` 218; `apps/edge_device/wamn.json` 72; `apps/platform_fixture_overlay/wamn.json` 64. Hand-written JSON, no comments, 2-space indent, keys in the order the author wrote them. |
| Hash | `apply-package` hashes the raw file bytes (`crates/control/lib/src/apply_package/package_version.rs:191`). No canonicalization step exists. |
| Where the bytes go | WMS: 22 KB of 27 KB are five `custom_operations`; models are 5 KB. |
| Repetition, WMS commands | The platform inputs `request_id`, `value.idempotency_key`, `value.occurred_at` and the matching `canonicalization.excluded_fields` appear in 5 of 5 (2 variants). The seven platform error codes appear in 5 of 5. `connection: postgres` and `visibility: public` in 5 of 5. `transaction`, `automatic_retry`, `idempotent_by`: two values each. Each not-found error restates `error_details: { required: [field, id] }`. |
| Repetition, WMS models | `"schema": "wms"` in 5 of 5. `permission` equals `<model>.<operation>` in 15 of 15. `pagination` and `limit` are byte-identical in 5 of 5 queries. `audit_log` has three variants of one shape. |
| Statements | `statements.*.parameters` and `.row` restate the `$n` parameters and `RETURNING`/`SELECT` columns of the SQL file with their types (`inventory.move`: 87 lines for three statements). Retained on purpose as the declared authority (`data-access.md`, rows `relations[]` and `statements.*`). |
| Reader | `crates/schema/generator/src/manifest.rs:14` parses the strict vocabulary and refuses anything outside it (`:621`). `publish_release.rs:433` requires the exact `wamn.json` that `apply-package` applied. `dev/config.rs:70` names the file. `tools/build-components` reads it. |
| Version | `package.version` is authored once in `wamn.json` and every operation reference resolves against it (`manifest.rs:2272-2286`). |
| Overlay | `client_acme_receiving` pins its base in `base_dependencies` (package, version, digest, operations) and declares 2 models and 5 custom operations of its own. |
| Generated files | `generated/` holds compiled outputs checked by the generator on every run; a drift fails generation. |

## 4. Design

### 4.1 The language

KCL. It has schemas with typed fields and defaults, mixins, comments, and compiles to JSON. The generator runs a pinned `kcl` CLI. `tools/install-kcl` installs it the way `tools/install-wash` installs `wash`, with the version and digest pinned in that one file. The repository takes no source dependency on the KCL workspace, which is not published to crates.io. CUE has the same model. Jsonnet and JSON5 have no schemas, so a wrong `fetch` value would still reach the generator. The author's file is `wamn.k`; the schema module is `wamn.k`'s import, shipped with the generator.

### 4.2 The schema module

One module, `wamn`, with one schema per manifest object: `Package`, `Model`, `Get`, `Query`, `Create`, `Update`, `Delete`, `Command`, `Projection`, `Statement`, `Relation`, `Field`, `Ref`, `Revision`, `AuditLog`, `Connection`, `Component`, `Workflow`, `BaseDependency`. Each schema's fields are the retained declarations of the Retain/Derive table; each default is one of the §2 list. Typed field constructors `uuid(name)`, `text(name)`, `int32(name)`, `numeric(name)`, `timestamptz(name)` give `{name, type, nullable: false}`; `nullable(...)` wraps one. Two shared values: `command_inputs` (the three platform inputs) and `platform_errors` (the seven codes with their standard details). The module is data, not code: no function beyond the constructors.

### 4.3 The compile step

The generator reads `wamn.k`, compiles it with the pinned `kcl` CLI, writes `generated/wamn.json`, and compares it with the checked-in file; a difference fails generation with the first differing path. The output format is 2-space indent with keys in the order the schema lists them. From the first converted version on, that format is the format of the package. Every reader of `wamn.json` (`manifest.rs`, `publish_release.rs`, `dev/config.rs`, `tools/build-components`) reads `generated/wamn.json`; none reads `wamn.k`. A package with `wamn.json` at its root and no `wamn.k` is refused with "the manifest is authored in wamn.k; wamn.json is generated".

### 4.4 Overlay composition

An overlay's `wamn.k` imports nothing from its base; `base_dependencies` stays a declaration (package, version, digest, operations) as today, because the pin is the contract and a language-level import would let the overlay drift from the sealed base. The language adds comments and defaults to the overlay; it does not compose the two packages.

### 4.5 Proof

Each converted package compiles to a document semantically identical to its `wamn.json` of today, apart from `package.version`. A test compares the parsed documents. The bytes change with the version. That equality is the acceptance of every conversion issue.

## 5. Issues (epic 1)

One branch, the routes agent, after the kind→type cutover lands. Each issue with its tests; every commit green.

1. `tools/install-kcl`, the `wamn` schema module and the compile step in the generator: `wamn.k` → JSON through the pinned CLI, the `generated/wamn.json` check, the refusal of an authored `wamn.json`. Unit tests: each authoring default fills; a key the reader defaults stays absent; a wrong enum value (`fetch`, `transaction`, `match`) fails at compile with its path.
2. Convert `platform_fixture` as version 2.1.0 and `platform_fixture_overlay` as its next version. Acceptance: the semantic comparison of §4.5; the generator and the fixture tests pass. §4.4 proved on the overlay.
3. Convert `edge_samples` and `edge_device`, each as its next version. Same acceptance. Record the line counts before and after on the bead.
4. Documentation: `data-access.md` gains the sentence that `wamn.json` is generated from `wamn.k` and that the Retain/Derive table is unchanged; `docs/operations/building.md` names the compile step; `development-loop.md` names `wamn.k` as the watched file. Closes the epic.

`wamn_wms`, `wamn_receiving` and `client_acme_receiving` convert when their next version is authored, each in the issue that authors that version. Test fixtures that feed the JSON reader stay JSON. Only packages under `apps/` convert.

## 6. Out of scope

- Epic 2, constraints the JSON vocabulary cannot state. Goal: cross-field checks at compile time (a `revision` input names a `Ref` input; a `writable_field` exists on the model; a `relation` names a table some statement touches). Boundary: compile-time refusals only; no new compiled vocabulary.
- Any change to the Retain/Derive table, to `relations`/`statements` authoring, or to the canonical JSON.
- Language-level import of a base into an overlay.
- Authoring of `deploy/` files, chart values or publication attachments in KCL.
- A UI that writes `wamn.k`.

## 7. Owner rulings

Recorded 2026-10-02.

1. The file is `wamn.k`.
2. `generated/wamn.json` is committed and checked.
3. The fixture converts first.
4. The generator runs a pinned `kcl` CLI and takes no source dependency on the KCL workspace (§4.1).
5. The compiled JSON states what today's files state. The schema's defaults are authoring defaults (§2).
6. A conversion is new bytes and lands as the package's next version. The comparison is semantic (§4.5).
