# 51 wamn-schema-generator, part 19

Scope: `cursor.rs`, `generate/numbering.rs`, `operation_reference.rs`, `inheritance.rs`, `generate/validation.rs` (structure), and how a manifest becomes valid (`wamn_catalog::manifest::from_slice`, `validate_operation_vocabulary`). Nothing built or run.

## Files read

- `src/cursor.rs` (types and function list); `apps/platform/data/access/src/cursor.rs` (head).
- `src/generate/numbering.rs` (header, function list).
- `src/operation_reference.rs` (header, `package_owners_of`, function list).
- `src/inheritance.rs` (in full).
- `src/generate/validation.rs` (function list); `wamn_catalog::manifest` validation function list, `PackageManifest::from_slice`, `validate_operation_vocabulary` (head).

## Callers read

- Every use of `encode_cursor`, `decode_cursor`, `CursorV1`, `CursorValue` (grep).
- Every reader of `operation_reference::*`: `route_schema.rs`, `client_ir.rs`, `client_route.rs`, `control/release/src/publish_release/attachments.rs`, `control/environment/src/author_workflow.rs::read_wiring_document`.
- Every caller of `load_base_manifests`, `complete_manifest`, `inherit_model_fields`, `validate_operation_vocabulary` (grep).

## One purpose per file

- `cursor.rs`: encode and decode the v1 keyset cursor.
- `generate/numbering.rs`: refuse numbering and caller-filled column declarations that cannot mean anything.
- `operation_reference.rs`: turn `<package>:<interface>/<operation>` references in authored documents into sealed operation ids.
- `inheritance.rs`: give an overlay model the number and actor fields of its relation owner.
- `generate/validation.rs`: the checks that need the database catalog (columns, constraints, SQL); `wamn_catalog::manifest` holds the checks on the manifest alone. One split, kept.

## Findings

**1. A second cursor implementation, used only by a test.**

Fact: generated code and guests use `wamn-data-access::cursor`. The generator's `cursor.rs` (exported `encode_cursor`, `decode_cursor`, `CursorV1`, `CursorValue`, `CursorError`) has one caller: `tests/cursor.rs`.

Options:

- A: delete the generator's `cursor.rs` and its exports; tests of the cursor live with `wamn-data-access`.

**2. Every reader resolves operation references again, each compiling `wamn.k`.**

Fact: release publish (`attachments.rs`), the client IR and route reader, `route_schema.rs`, and environment workflow authoring each resolve references themselves. `package_owners_of` finds the nearest package root above the document and compiles its `wamn.k` with KCL for each read. Environment workflow authoring resolves a workflow file this way, while its gate checks the workflow against the release the environment serves.

Options:

- A: the build resolves references once, into the final documents (tab 35 finding 1 A); readers read sealed ids. Environment workflows resolve against the served release's manifests, which the gate already loads. Delete `package_owners_of` and the per-reader resolve calls.

**3. Inheritance runs twice and is spliced into JSON.**

Fact: `complete_manifest` (build and materialize) parses the manifest, runs `inherit_model_fields`, then writes the inherited members back into the untyped JSON to keep its other members. `generate` then runs `inherit_model_fields` again on the completed manifest.

Options:

- A: inherit once, on the typed manifest, in the derive step; serialize the typed manifest (with tab 43 finding 1 A). Delete `complete_manifest` and the second call.

**4. A parsed manifest is not yet a valid one.**

Fact: `PackageManifest::from_slice` decodes and checks connections. The operation vocabulary check (`validate_operation_vocabulary`: package identity, base dependencies, internal relations, routes, operation and component membership) is a separate call, made by 9 callers after parsing (`generate.rs` and `validation.rs` both call it in one run; `component_admission.rs` twice).

Options:

- A: `from_slice` runs the vocabulary check, so a `PackageManifest` value is valid by construction; the operation set becomes a method. Delete the 9 separate calls.

**5. The manifest reader drops two old members silently.**

Fact: `from_slice` removes `workflows` and each custom operation's `registration` before it decodes, so a manifest carrying them loads.

Options:

- A: delete the removal; the strict decoder refuses them like any unknown member (greenfield rule).

## Fine as is

- `generate/numbering.rs`: uses the one pattern parser (`wamn_number_pattern`).

## Follows from earlier rulings

- `inheritance.rs::load_base_manifests`: delete the `read_current` call and the manifest digest check against the build record (tab 42 finding 1). It reads the base manifest from the base build output only.

## Tests deleted, changed or moved

- Finding 1: `tests/cursor.rs` moves to `wamn-data-access` where it adds cases, else goes.
- Finding 2: reader tests that resolve references build final documents.
- Findings 3–5: tests that call `validate_operation_vocabulary` or feed `workflows`/`registration` change.

## Not read

- Bodies of `validation.rs` checks; `operation_reference.rs` resolve functions; `numbering.rs` checks.

## Rulings

- 2026-11-12: all findings A.
- Owner note: the generator as a whole looks too complex, and its value is unclear.
