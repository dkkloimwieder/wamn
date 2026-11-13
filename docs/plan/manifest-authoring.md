# Manifest authoring

**Scoping rule.** This document names two epics. Only epic 1 is scoped to issues. Epic 2 is named with its goal and boundary and nothing more, and is scoped only after epic 1 is closed and reviewed by the owner. An agent that finishes epic 1 stops.

## 2. Fixed rules

See [current manifest authoring](../operations/building.md#package-manifests).

- No new manifest vocabulary. A declaration the compiled JSON cannot already express is epic 2.

## 3. Current state

See [package artifact identity](../operations/building.md#reading-a-pushed-package).

## 4. Design

### 4.1 The language

See [package manifest authoring](../operations/building.md#package-manifests).

### 4.2 The schema module

See [the current manifest schema](../operations/building.md#package-manifests).

The older schema list below differs from the current module and needs an owner ruling.

One module, `manifest`, with one schema per manifest object: `Package`, `Model`, `Get`, `Query`, `Create`, `Update`, `Delete`, `Command`, `Projection`, `Statement`, `Relation`, `Field`, `Ref`, `Revision`, `AuditLog`, `Connection`, `Component`, `Workflow`, `BaseDependency`. Each schema's fields are the retained declarations of the Retain/Derive table; each default is one of the §2 list. Typed field constructors `uuid(name)`, `text(name)`, `int32(name)`, `numeric(name)`, `timestamptz(name)` give `{name, type, nullable: false}`; `nullable(...)` wraps one. Two shared values: `command_inputs` (the three platform inputs) and `platform_errors` (the seven codes with their standard details). The module is data, not code: no function beyond the constructors.

The module is named `manifest` because `wamn.k` cannot import a module named `wamn` (§7, ruling 7). As built, it also has `int64(name)` and `json(name)` constructors. The shared values are `request_id`, `idempotency_key`, `occurred_at`, `command_inputs` and `platform_errors`, and the standard details `not_found_details` and `conflict_details` are separate values. A model's `operations` must use the union form `operations: {...}`, because the `=` form replaces the permissions that the model fills, and the package refuses it.

### 4.3 The compile step

See [the current compile procedure](../operations/building.md#wamn-build).

### 4.4 Overlay composition

See [overlay composition](../operations/building.md#package-manifests).

### 4.5 Proof

See [the current build contract](../operations/building.md#wamn-build).

## 6. Out of scope

- Epic 2, constraints the JSON vocabulary cannot state. Goal: cross-field tests at compile time (a `revision` input names a `Ref` input; a `writable_field` exists on the model; a `relation` names a table some statement touches). Boundary: compile-time refusals only; no new compiled vocabulary.
- Any change to the Retain/Derive table, to `relations`/`statements` authoring, or to the canonical JSON.
- Language-level import of a base into an overlay.
- Authoring of `deploy/` files, chart values or publication attachments in KCL.
- A UI that writes `wamn.k`.

## 7. Owner rulings

See [current package manifests](../operations/building.md#package-manifests).
