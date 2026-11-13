# 75 wamn-catalog, part 2

Scope: `crates/catalog/model` at `epic/dep7` (b94833fd), part 2 of 3: `serving_manifest.rs` (2,571), `component_facts.rs` (1,936), `profile.rs` (419), `component_artifact_reference.rs` (395), `edge_bundle.rs` (41), `route_identity.rs` (42). Nothing built or run.

## Earlier work checked first

- Tab 01 (2026-11-10): request repeats are compared by a hash the server takes of the parsed request; no per-field respelling.
- Tab 24: `LoadedRelease::load` and the mount path constant go; the release comes from the registry.
- Tab 38 ruling 1: the route table is its own component, not composed with the application. Tab 70: the edge bundle writes the route and application components separately.
- Tab 73 2 A: build-time composition and per-member statement instances stay.
- Tab 74: 2 A (`pg_query` is the one SQL parser), 5 A (one SQL value type enum in `wamn-scalar`). Greenfield: no compatibility paths.

## Files read

- `serving_manifest.rs`: header, item list, `RouteCanonicalization`, `ServingComposedEntry`, `contains_retired_identity`.
- `component_facts.rs`: item list, doc comments of the effect types, `normalize_component_fact`, `verify_stored_effect_projection`, `normalize_effects`.
- Headers of `profile.rs`, `component_artifact_reference.rs`, `edge_bundle.rs`, `route_identity.rs`.

## Callers read

- Repository grep for `verify_stored_effect_projection`, `normalize_component_fact`, `bind_component_statement_facts`, `imports_fingerprint`, `.effects`, `AdmittedComponentEffect`, `ComponentEffectProvenance`, `RouteCanonicalization`, `RELEASE_MANIFEST_*`, `release_manifest_configmap_name`.
- `crates/platform/engine/src/component_admission.rs` (where effects are made).

## One purpose per file

- `serving_manifest.rs`: the release document every serving process loads, its canonical bytes and digest.
- `component_facts.rs`: the facts admission derives from component bytes (ports, parameters, connections, SQL statements, effects).
- `profile.rs`: per operation, the tables and columns its statements read, write or lock.
- `component_artifact_reference.rs`: OCI references from a base and a digest. `edge_bundle.rs`: the edge release file names. `route_identity.rs`: operation token, route path and attachment names.

## Findings

**1. The effect projection is derived, stored, re-checked and never read.**

Fact:

- Admission builds `AdmittedComponentEffect` entries from the component's audited imports (`engine/component_admission.rs`); every entry's provenance is `Imported`. The doc comment on `verify_stored_effect_projection` says effects are "a total function of the audited imports".
- `normalize_effects` checks the list against the imports in both directions. `verify_stored_effect_projection` re-runs that check on stored rows, to catch rows an earlier schema change defaulted to `'[]'`; it runs in `wamn-package-artifact`, `publish_release.rs`, `wiring_resolution.rs` and `local_application.rs`.
- No production code reads `component.effects` (grep; only `engine/component_admission.rs` tests do).

Options:

- A: delete the `effects` field, `AdmittedComponentEffect`, `ComponentEffectProvenance`, `normalize_effects`, `verify_stored_effect_projection` and their error types. Whoever needs effects later derives them from the imports.

Recommend A.

**2. The serving manifest scans for retired keys.**

Fact: `contains_retired_identity` walks the whole document and refuses keys such as `flow-id`, `plan-hash`, `calls` and `source-artifact`. The manifest is also refused unless its format version is 11 (`validate_format_version`), and decoding uses `deny_unknown_fields`.

Options:

- A: delete the scan; the format version and strict decoding refuse old documents.

Recommend A.

## Follows from earlier rulings

- Tab 01: `RouteCanonicalization`, `RouteLineOrder`, `CanonicalSpelling`, `respell`, `remove` go, and in `manifest.rs` `CommandCanonicalization`, `CommandLineOrder` and `validate_command_canonicalization`. Callers: `flow_http_routing.rs`, `operation/intent.rs`, `execution/host/operation.rs`, release `publish_release.rs` and `package_sources.rs`, generator `client_plan.rs`.
- Tab 24: `RELEASE_MANIFEST_CONFIGMAP_PREFIX`, `release_manifest_configmap_name` (no caller) and `RELEASE_MANIFEST_MOUNT_PATH` go.
- Tab 38 ruling 1, tab 70: `ServingComposedEntry` stops naming a route plus application entry; `edge_bundle.rs` drops the composed `flow-http.wasm` and names the route and application components.
- Tab 74 2 A: `profile.rs` reads relations through `pg_query`. Tab 74 5 A: `ComponentSqlValueType` becomes the `wamn-scalar` enum.

## Fine as is

- `ServingManifest` canonical bytes and digest; `AttachmentAuthPolicy` with its closed modes list.
- `component_artifact_reference.rs`, `route_identity.rs`.
- `bind_component_statement_facts`, `imports_fingerprint`, statement instances (tab 73 2 A).

## Tests deleted, changed or moved

- Finding 1: effect tests in `component_facts.rs` and `engine/component_admission.rs`; the stored-row check tests.
- Finding 2: the retired-key tests in `serving_manifest.rs`.
- Tab 01: canonicalization tests in `serving_manifest.rs`, `flow_http_routing.rs` and the generator.

## Not read

- `serving_manifest.rs::from_canonical_bytes` body; `profile.rs` and `component_artifact_reference.rs` bodies.

## Rulings

- 2026-11-12: both findings A (effect projection deleted; retired-key scan deleted).
