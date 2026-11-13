# 35 wamn-schema-generator, part 4

Scope: route publication (what the generator writes about routes, and how readers merge it). Read with tab 31 finding 3 A in mind: the composer builds the guest route table from this data. Nothing built or run.

## Files read

- `src/generate/publication.rs`, `src/generate/routes.rs`, `src/route_schema.rs` (non-test parts, in full)

## Callers read

- Every reader of `route_schema::*`: `client_ir.rs`, `client_route.rs`, `materialize.rs`, `control/release/src/publish_release/attachments.rs` and `attachments/plan.rs` (`validate_attachment_definition_hashes`), `control/package/src/component_admission.rs`, `component_declaration.rs`, `examples/emit_client_ts.rs`.
- Every authored `publication/attachments.json` under `apps/` (search for `generated/routes/` and `definition-hash`).
- `engine/src/flow_http_routing.rs::ADAPTER_GOVERNED_BYTES`; `catalog/model/src/host_route.rs` default modes; `manifest.rs` `routes.auth_modes` check.

## One purpose per file

- `generate/publication.rs`: write one route entry and one operation entry per generated CRUD operation.
- `generate/routes.rs`: derive the route input schema of a generated operation from its input contract.
- `route_schema.rs`: resolve `$ref` route schemas, check and recompute definition hashes, merge generated and authored publication documents.

## Findings

**1. Generated and authored route documents are merged and resolved by five readers.** Fact: the generator writes `generated/publication/attachments.json` and `component-operations.json`, whose schemas are `{"$ref": "generated/routes/<model>/<action>.json"}`, and hashes each definition in its `$ref` form. Every reader (release publish, component admission, component declaration, client IR, client route) merges the generated documents into the authored ones, resolves each `$ref`, checks the hash of the `$ref` form, then hashes again over the resolved form. No authored document under `apps/` uses a `generated/routes/` reference; only generated entries do.

- A: the build writes the final documents once: authored and generated entries merged, schemas inline, each hash computed once. Readers read that one output. Delete `$ref` resolution and the second hash (`reference`, `checked`, `resolve`, `resolve_attachment(s)`, `resolve_declaration`, `names_generated_schema`, `PackageFiles::schema`), the merge helpers in readers (`merge_attachments`, `merge_operations`, `generated_attachments`), and the `generated/routes/` files.

**2. Authored documents carry a hash of their own definition.** Fact: 22 authored `definition-hash` values under `apps/` must equal the hash of the definition beside them; `validate_attachment_definition_hashes` refuses a mismatch at publish. The value is derived from data next to it.

- A: the build computes the hash; authored documents no longer carry it.

**3. The body limit is set in three places and the guest caps it silently.** Fact: generated routes declare 1 MiB (`RAW_BODY_MAXIMUM`); an attachment without a limit gets `u32::MAX` (`ADAPTER_GOVERNED_BYTES`); the guest takes the smaller of the route's limit and its own 1 MiB (`AdapterLimits::default`). So an authored limit above 1 MiB has no effect.

- A: one limit per route in the guest table (tab 31 finding 3 A), defaulting to 1 MiB when not declared. Delete `AdapterLimits.body_bytes` and `ADAPTER_GOVERNED_BYTES`.
- B: one platform limit, 1 MiB. Delete `raw-body-bytes` from attachments, `RAW_BODY_MAXIMUM` and `ADAPTER_GOVERNED_BYTES`. Large uploads go through the file routes (tab 26).
- Recommend B (fewer moving parts; uploads already have their own path).

**4. The default auth modes are written twice.** Fact: `["pat", "session"]` is the default in `publication.rs` and again in `catalog/model/src/host_route.rs`.

- A: one constant in `wamn-catalog`.

## Carried

- `wamn-catalog` has two derivations named "definition hash": `canonical_json_sha256(definition)` for serving attachments, and a framed hash over artifact, definition and resolved sources (`lib.rs`). Catalog tab.

## Tests deleted, changed or moved

- Delete (finding 1): `route_schema.rs` tests of `$ref` resolution, hash recompute and merge; reader tests that build documents with `$ref` (`client_ir.rs`, `control/package` declaration tests, release attachment tests).
- Delete (finding 2): `release/.../attachments/plan.rs` tests of `validate_attachment_definition_hashes`; authored fixtures drop `definition-hash`.
- Change (finding 3): `tests/adversarial.rs::default_raw_body_ceiling_accepts_one_mebibyte_and_refuses_the_next_byte`, `pending_body_resumes_under_the_selected_route_limit`.

## Not read

- `client_ir.rs`, `client_route.rs` beyond the reader lines (client parts).

## Rulings

- Finding 3: A (one body limit per route in the guest table, 1 MiB when not declared; `AdapterLimits.body_bytes` and `ADAPTER_GOVERNED_BYTES` go).
- Findings 1, 2, 4: A.
