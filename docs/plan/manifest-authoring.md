# Manifest authoring

**Scoping rule.** This document names two epics. Only epic 1 is scoped to issues. Epic 2 is named with its goal and boundary and nothing more, and is scoped only after epic 1 is closed and reviewed by the owner. An agent that finishes epic 1 stops.

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

| Place | Today |
| --- | --- |
| Hash | `apply-package` hashes the raw file bytes (`crates/control/lib/src/apply_package/package_version.rs:191`). No canonicalization step exists. |
## 4. Design

### 4.1 The language

KCL. It has schemas with typed fields and defaults, mixins, comments, and compiles to JSON. The generator runs a pinned `kcl` CLI. `tools/install-kcl` installs it the way `tools/install-wash` installs `wash`, with the version and digest pinned in that one file. The repository takes no source dependency on the KCL workspace, which is not published to crates.io. CUE has the same model. Jsonnet and JSON5 have no schemas, so a wrong `fetch` value would still reach the generator. The author's file is `wamn.k`; the schema module is `wamn.k`'s import, shipped with the generator.

### 4.2 The schema module

One module, `manifest`, with one schema per manifest object: `Package`, `Model`, `Get`, `Query`, `Create`, `Update`, `Delete`, `Command`, `Projection`, `Statement`, `Relation`, `Field`, `Ref`, `Revision`, `AuditLog`, `Connection`, `Component`, `Workflow`, `BaseDependency`. Each schema's fields are the retained declarations of the Retain/Derive table; each default is one of the §2 list. Typed field constructors `uuid(name)`, `text(name)`, `int32(name)`, `numeric(name)`, `timestamptz(name)` give `{name, type, nullable: false}`; `nullable(...)` wraps one. Two shared values: `command_inputs` (the three platform inputs) and `platform_errors` (the seven codes with their standard details). The module is data, not code: no function beyond the constructors.

The module is named `manifest` because `wamn.k` cannot import a module named `wamn` (§7, ruling 7). As built, it also has `int64(name)` and `json(name)` constructors. The shared values are `request_id`, `idempotency_key`, `occurred_at`, `command_inputs` and `platform_errors`, and the standard details `not_found_details` and `conflict_details` are separate values. A model's `operations` must use the union form `operations: {...}`, because the `=` form replaces the permissions that the model fills, and the package refuses it.

### 4.3 The compile step

The generator reads `wamn.k`, compiles it with the pinned `kcl` CLI, writes `generated/wamn.json`, and compares it with the checked-in file; a difference fails generation with the first differing path. The output format is 2-space indent with keys in the order the schema lists them. From the first converted version on, that format is the format of the package. Every reader of `wamn.json` (`manifest.rs`, `publish_release.rs`, `dev/config.rs`, `tools/build-components`) reads `generated/wamn.json`; none reads `wamn.k`. A package that holds both `wamn.json` and `wamn.k` at its root is refused with "the manifest is authored in wamn.k; wamn.json is generated". A package with a root `wamn.json` and no `wamn.k` is read as today until the last conversion issue (§5) lands, and that issue adds its refusal (§7, ruling 8).

### 4.4 Overlay composition

An overlay's `wamn.k` imports nothing from its base; `base_dependencies` stays a declaration (package, version, digest, operations) as today, because the pin is the contract and a language-level import would let the overlay drift from the sealed base. The language adds comments and defaults to the overlay; it does not compose the two packages.

### 4.5 Proof

Each converted package compiles to a document semantically identical to its `wamn.json` of today, apart from `package.version`. A test compares the parsed documents. The bytes change with the version. That equality is the acceptance of every conversion issue.

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
7. The schema module is `manifest`, because `wamn.k` cannot import a module named `wamn` (§4.2).
8. Now, only a package holding both `wamn.json` and `wamn.k` is refused. The refusal of a root `wamn.json` with no `wamn.k` lands with the last conversion issue (§4.3). The extra constructors, shared values and the union form are recorded as built (§4.2).
9. An accepted plan is not trimmed. A closed issue gets a status line.
