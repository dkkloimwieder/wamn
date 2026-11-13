# 01 `wamn-execution-contract`

Path `apps/platform/execution/contract`. 825 non-test lines, no workspace dependencies, 26 crates depend on it. Every module was read. Callers were counted by search, not read.

## Verdict

- It is shared types and helpers. Nothing in it executes anything. (fact)
- It is not one contract. It holds six unrelated topics. (fact)
- About a third of it has no caller or one caller. (fact, by search)
- Cost of the shape: 26 crates rebuild on any change, and the crate's own comment on `bytes_sha256` says a new compiled item changes every guest's bytes and pinned digest. (fact)

## Rulings (2026-11-10)

Owner ruling: the division below is accepted. The crate `wamn-execution-contract` stops existing. Platform pieces do not live with client pieces. The preference is always to delete. Every caller of every piece was read before this table was written.

| Piece | What the callers showed (fact) | Ruling |
| --- | --- | --- |
| Canonical JSON and `sha256` digests (`lib.rs`) | 24 crates use it. Two other copies of the digest function exist: `wamn_package_artifact::sha256_digest`, `schema/control/src/connections.rs::prefixed_sha256`. | New crate `wamn-digest`. One digest function. |
| Number and timestamp text format (`canonical_scalar.rs`) | After ruling C the users are `wamn-data-access` and `wamn-record-history` (libraries linked into applications), `schema/generator/src/cursor.rs`, and `runtime/src/plugins/wamn_postgres/types.rs`, which writes timestamps in this format. | New crate `wamn-scalar`. |
| `EffectOutcome` | Used by `effect_span.rs`, `connection_http.rs`, `wamn_blobstore/host.rs` in the runtime. `http-route/src/guest.rs` maps the WIT enum of the same name to it by hand, variant by variant. `wiring.rs` uses the labels once in a JSON schema. | Into the runtime. The route guest uses its WIT enum. The hand mapping goes. |
| `portable_http_target.rs` | Only `connection_http.rs` and `connection_authority.rs` in the runtime. | Into the runtime. |
| `MAIN_PORT`, `ERROR_PORT` | Defined three times: here, `wamn_router` `outcome.rs`, `apps/platform/execution/jsonata/src/lib.rs`. `crates/execution/workflow/tests/port_constant_agreement.rs` exists to check two copies agree. | One definition. The agreement test goes. Open: which crate holds it; the router and the catalog both use it and neither depends on the other. |
| `node_contract.rs`: `NodeError`, `ErrorDetail`, `RateLimitDetail`, `Emission`, `ConnectionRequirement` | `Emission`, `ConnectionRequirement`: no callers. `NodeError`: one `From` conversion in `run-state/src/status.rs`, and no code builds the value. The router has its own `NodeError`. | Deleted, with the `run-state` conversion. |
| `EntryKind` | No callers. | Deleted. |
| Test cases: `expect.rs`, `test_set.rs`, `status.rs` (461 lines with tests) | Stored in wiring documents and checked for shape. Never executed: `crates/authoring/model/src/gate.rs::judge_gate_document` says "without executing a case", and nothing outside tests reads the expected values. | Deleted. The `cases` array leaves the wiring document, with `validate_gate_cases` and the `EffectfulComponentReached` refusal that depends on it. |
| Request comparison | See "Why requests are compared" below. | Option C. |
| Read query (`read_query.rs`) | Encode: `wamn-client`, `wamn-ctl`, bench. Decode: `http-route/src/lib.rs::read_query_request_element` only. | Open: strictness. |

### Read query: strictness dropped

Owner ruling (2026-11-10): drop strictness, as long as caching still works. Be generous in what is accepted and conservative in what is sent.

Why the query is in the URL (fact, `docs/architecture/execution.md` and `engine/src/flow_http_routing.rs::read_cache_control`): every read is an HTTP GET so that it can be cached. A list or projection sends `max-age=10, stale-while-revalidate=60`. A single record sends an ETag and can answer 304.

Caching does not depend on strictness (fact, `crates/execution/host/src/read_cache.rs` read in full):

- A tag is built from the release, and the record revision, the WAL position, or the feed cursor. The URL is not an input. So 304 answers are unchanged.
- A browser cache keys on the URL. The generated client keeps writing the one canonical spelling, so its requests share entries as today.
- A request spelled another way gets a correct answer and its own cache entry.

What changes:

| Goes | Stays |
| --- | --- |
| `decode_read_query`'s `NotCanonical` refusal | The client's encoder, in `wamn-client`. `wamn-ctl` and the bench tool call it from there. |
| A shared crate for the read query | A plain query parser in `http-route`: any parameter order, any valid escaping, each value read as JSON |
|  | The 8 KiB limit on the request target |

Found while reading, for the route and host crates when their turn comes (fact): the tag code exists twice. `read_cache.rs` in the host has `get_tag`, `list_tag`, `changes_tag`; `http-route/src/entry.rs` has `list_etag`, `changes_etag`, `tag_digest`.

## What it holds

| Module | What it is | Callers outside the crate (files, non-test) | Finding |
| --- | --- | --- | --- |
| `lib.rs` | `canonical_json_bytes`, `canonical_json_sha256`, `bytes_sha256` | 36, 18, 19 | The real core. Small and clear. |
| `canonical_scalar.rs` | One spelling for timestamps and decimals | about 10 | Used. Exists because the write log compares requests by bytes. |
| `effect_outcome.rs` | `EffectOutcome`: six words for what happened to an outbound call | 7 | Used. About 40 comment lines for 6 variants. |
| `read_query.rs` | Query-string encoding of a read request | 3 encode, 2 decode | Used. Decode refuses anything not byte-identical to what encode writes. |
| `ports.rs` | `MAIN_PORT`, `ERROR_PORT`, `EntryKind` | ports 6 to 7; `EntryKind` 0 | `EntryKind` has no caller. |
| `node_contract.rs` | `NodeError`, `ErrorDetail`, `RateLimitDetail`, `Emission`, `ConnectionRequirement` | `NodeError` 1 (`crates/execution/run-state/src/status.rs`); `Emission` 0; `ConnectionRequirement` 0 | The module comment says the nodes it was written for are gone. Other `NodeError` uses in the repo are the WIT-generated type, not this one. |
| `portable_http_target.rs` | `normalize_portable_http_target` | 3, all in the runtime's HTTP connection plugin and `connection_authority.rs` | One area uses it. |
| `expect.rs`, `test_set.rs`, `status.rs` | `TestSetCase`, `Expect`, `validate_cases`, `WiringFailureKind` | 1 (`crates/catalog/model/src/wiring.rs`), which stores and validates them | Inference: nothing runs the cases. A search for readers of `cases` found none. |

## Who calls it

26 crates. Method: a search of each crate for `wamn_execution_contract::` paths (fact for what it found; a use through an alias would be missed).

| What they take | Crates | Count |
| --- | --- | --- |
| Only digests and canonical JSON | `wamn-control-provision`, `wamn-edge`, `wamn-engine`, `wamn-environment-control`, `wamn-execution-host`, `wamn-package-artifact`, `wamn-package-control`, `wamn-platform-identity`, `wamn-release-control`, `wamn-scenario-worker` | 10 |
| Digests or JSON, plus the read query | `wamn-ctl`, `wamn-dev-control`, `wamn-integration-tests`, `wamn-bench` (read query only) | 4 |
| Scalar spelling, with or without canonical JSON | `wamn-data-access`, `wamn-record-history`, `wamn-client` (also read query), and the apps `receiving`, `wms` | 5 |
| Several topics at once | `http-route` (JSON digest, scalars, `EffectOutcome`, read query), `wamn-runtime` (JSON, scalars, `EffectOutcome`, HTTP target), `wamn-schema-generator` (JSON, digests, scalars, `MAIN_PORT`), `wamn-catalog` (JSON, scalars, `EffectOutcome`, test cases, ports) | 4 |
| One small thing | `wamn-run-state` (`NodeError`), `wamn-workflow` (`MAIN_PORT`, `ERROR_PORT`, in a test only), `wamn-authoring-model` (`validate_cases`) | 3 |

Reading:

- 14 of 26 callers want hashing or canonical JSON and nothing about execution. (fact)
- `EffectOutcome` has three callers: `http-route`, `wamn-runtime`, `wamn-catalog`. (fact)
- The read query has one producer side (`wamn-client`, `wamn-ctl`, `wamn-dev-control`, tests, bench) and one consumer (`http-route`). (fact)
- Two example apps (`receiving`, `wms`) import the scalar functions directly. An application author therefore calls platform internals to spell a number. (fact)

## Ergonomics

- The name misleads. Most callers want a digest or canonical JSON and import an execution contract to get it. (fact)
- The read query is strict to the byte. `?b=1&a=2`, a repeated parameter or lowercase hex is refused as `ReadQueryError::NotCanonical`. A hand-written request or any client other than the generated one will hit this. The stated reason in the module comment is cache keys. (fact)
- Three functions produce the same `sha256:<hex>` string: `bytes_sha256` here, `wamn_package_artifact::sha256_digest` (uses `ring`), and `schema/control/src/connections.rs::prefixed_sha256`. (fact)

## Complexity that comes from elsewhere

- `canonical_scalar.rs` and the strict read query both exist because requests are compared by exact bytes. If that comparison changes, both shrink. The write log comparison code was not read for this tab.

## Why requests are compared, and where

Owner question (2026-11-10): why compare bytes at all; it sounds like accidental complexity. Code read: `crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs` in full; the write path of `apps/platform/ingress/http-route/src/entry.rs`; `crates/client/core/src/request.rs::build_request`; `apps/wamn_receiving/component/src/data/record_receipt.rs`.

What the comparison is for (fact): one case. A request arrives with an idempotency key that is already in `platform.write_log`. If the stored `request` value equals the new one, the stored result is returned. If not, the answer is `idempotency_conflict`. See `write_log.rs::begin_with` and `OperationTransaction::begin_opaque`.

What is compared (fact): not the arriving bytes. The route parses the JSON, respells it, removes excluded fields, and hashes it (`entry.rs`, `canonicalize` then `canonical_json_sha256`). The hash text is stored in the `request` column.

The same respelling is done in three places (fact):

| Place | What it does |
| --- | --- |
| Client: `wamn-client` `build_request` | Respells every timestamp, numeric and UUID, and sends sorted-key JSON |
| Platform route: `http-route` `entry.rs::canonicalize` | Respells the fields named in `request_hash_rule`, sorts lines, then hashes |
| Application: `record_receipt.rs`, `apps/wamn_wms/component/src/data/scalar.rs` | Respells timestamps, numerics and UUIDs again, and sorts lines again, by hand |

The rule itself is also declared a fourth time, in the manifest: `canonicalization` (`line_order`, `excluded_fields`), which becomes `request_hash_rule` in `crates/platform/engine/src/flow_http_routing.rs`.

Options:

| Option | What goes | What is lost |
| --- | --- | --- |
| A. Keep the comparison, do it only in the platform route | Client-side and application-side respelling. A client sends any valid spelling. | Nothing. The route already does it. (inference: needs a check that every write route carries a rule) |
| B. Drop the comparison. The key alone decides: a repeat returns the stored result. | Also the `request` column, `request_hash_rule`, the manifest `canonicalization` block and its rules, line sorting, `idempotency_conflict`, and most of `canonical_scalar.rs` | A client that reuses a key with different input gets the first result back and no error. |

Not read: the cursor code in `apps/platform/data/access` and `record-history`, which also call the scalar functions for a different purpose; `numbering/set.rs` and `wamn_postgres/operation.rs`, the two callers of `begin_opaque`.

### Ruling: option C

Owner ruling (2026-11-10): the server hashes the parsed request with sorted keys, stores the hash with the key, and compares on a repeat. Nothing else is done to the request for this purpose. This is how it is commonly handled (Stripe, the IETF Idempotency-Key draft; from general knowledge, not checked this session).

| Stays | Goes |
| --- | --- |
| `canonical_json_sha256` in the platform route | Client respelling in `wamn-client` `build_request` |
| The `request` column in `platform.write_log` | Application respelling for this purpose (`record_receipt.rs`, `wms` `scalar.rs`) |
| `idempotency_conflict` | `request_hash_rule` and `entry.rs::canonicalize`, `respell` |
|  | The manifest `canonicalization` block (`line_order`, `excluded_fields`) and `validate_command_canonicalization` |
|  | Line sorting before the hash |

What is lost: a retry that spells one value differently (`12.0` then `12.00`, or `Z` then `+00:00`) gets `idempotency_conflict`, not the stored result.

To check before any change (not read):

- `excluded_fields`: which fields operations exclude from the hash today, and why. If a field such as `request_id` differs on every retry, it must still be left out of the hash.
- Whether `canonical_scalar.rs` keeps any caller after this. The cursor code in `wamn-data-access` and `record-history` uses it for another purpose.
- The two callers of `begin_opaque`.

### Option C after the full read

The three open points above are now read. C stands, with four corrections to what was written before the read. All rows are fact unless marked.

**1. The rule lives in eight places, not three.**

| Place | What it holds |
| --- | --- |
| `crates/schema/generator/kcl/manifest/manifest.k` `Canonicalization` | The authored block: `excluded_fields` (default `request_id`, `value.idempotency_key`), `line_order` |
| `catalog/model/src/manifest.rs` | The parsed block and `validate_command_canonicalization` |
| `schema/generator/src/client_plan.rs` | Derives a spelling per field from its type |
| `control/release/src/publish_release/package_sources.rs` | Copies the rule into the serving manifest |
| `catalog/model/src/serving_manifest.rs` `RouteCanonicalization` | `apply` (respell, sort lines) and `hashed` (drop excluded paths) |
| `platform/engine/src/flow_http_routing.rs` | Turns it into `request_hash_rule` for the route guest; always adds `request_id` to the excluded set |
| `apps/platform/ingress/http-route/src/entry.rs` | A second copy of the respell and sort code, then the hash |
| `platform/engine/src/operation/intent.rs::invoke_logged` | Calls `hashed` on the workflow and box path |

Plus the client (`build_request`) and the applications by hand.

**2. Two fields must always be left out of the hash.** `request_id` is a per-delivery id that a client uses to match answers to request elements. The key field is either `request_id` or one the route names, such as `value.idempotency_key`. No application in `apps/` authors `excluded_fields`; all use the default. So under C the rule is fixed: hash the request without `request_id` and without the key field. Nothing is declared.

**3. The block also carries two labels that are not about request identity.** `line_order` and its `positive_member` make `schema/generator/src/generate/contracts.rs` add the labels `duplicate_line` and `nonpositive_quantity` to the generated contract document. The platform enforces neither. Both checks are hand-written in the application (`apps/wamn_receiving/component/src/data/record_receipt.rs`). They are application validation and have nothing to do with idempotency keys. When the block goes, the two labels go, and the application's checks are untouched.

**4. The scalar functions keep callers.** `apps/platform/data/access/src/scalar.rs` and `cursor.rs` use `canonical_numeric` and `canonical_timestamptz` to accept a value and build its typed form. `record-history` uses the timestamp one. These are input parsing, and stay.

One more fact: `wamn_postgres/numbering/set.rs` already works the plain way. It hashes the serialized request with SHA-256 and passes it to `begin_opaque`.

What C changes, corrected:

| Goes | Stays |
| --- | --- |
| The authored `canonicalization` block and its validation | Hash of the parsed request, sorted keys, without `request_id` and the key field |
| Per-field spelling in `client_plan.rs`, the copy in the release, `RouteCanonicalization::apply`, `request_hash_rule.spelling` and `.lines`, `entry.rs::canonicalize` and `respell` | `idempotency_conflict` |
| Client respelling for the hash in `build_request` | The scalar functions, as input parsing in `wamn-data-access` |
| Application respelling done only for the hash |  |

No open question remains for C.

## Options

Each needs its caller read before it is done.

| Option | What is lost |
| --- | --- |
| Delete `Emission`, `ConnectionRequirement`, `EntryKind` | Nothing. No callers. |
| Move `NodeError`, `ErrorDetail`, `RateLimitDetail` into `run-state` | Nothing. One caller. |
| Move `portable_http_target.rs` into the runtime HTTP plugin | Nothing. One area uses it. |
| Delete the test-case types, or move them beside `wiring.rs` | If deleted: the `cases` array in a wiring document. Depends on whether anything is meant to run them. |
| Keep one digest function | Nothing. |
| Accept any parameter order in a read query and sort before use | Byte-equal URLs from clients that do not sort. A cache would key on the decoded request. |

What remains after the first five: canonical JSON, digests, scalars, effect words, port names. About 400 lines.

## Not read

- The test half of each module.
- The 26 callers, beyond counting them.
- The write log's request comparison.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab.

Deleted:

- `crates/execution/workflow/tests/port_constant_agreement.rs` (one port definition).
- `contract/src/node_contract.rs::successful_emission_may_replace_context`.
- `contract/src/expect.rs` (3 tests), `status.rs` (3), `test_set.rs` (4): the test-case types (also tab 84).
- `contract/src/read_query.rs::a_query_that_is_not_the_canonical_encoding_is_refused` (strictness dropped).
- `engine/src/operation/intent.rs::reordered_lines_and_other_spellings_are_one_request` (option C: no respelling, no line sort).
- The `canonicalization` parts of `schema/generator/tests/platform_generation.rs::retired_workflows_are_dropped_and_line_profiles_are_closed`, `a_claim_command_names_its_key_and_its_codec_claims_nothing`, `a_claim_command_declares_no_claim_object`, of `manifest_authoring.rs::a_command_that_takes_the_envelope_gets_its_bounds_and_canonical_input`, and the block in `tests/support/platform_claim.rs`.
- The respelling tests in `crates/client/core/tests/` go with `wamn-client` (tab 85).

Changed:

- `engine/src/operation/intent.rs::the_request_hash_keeps_the_key_and_is_the_request_element_alone`: the hash leaves out `request_id` and the key field.
- `http-route/tests/adversarial.rs::a_read_authenticates_its_body_but_takes_its_request_element_from_the_canonical_query`: any parameter order is accepted.

Moved:

- `contract/src/lib.rs` (4 digest and canonical JSON tests) to `wamn-digest`.
- `contract/src/canonical_scalar.rs` (2) to `wamn-scalar`.
- `contract/src/effect_outcome.rs::serialized_outcomes_and_schema_labels_keep_the_frozen_vocabulary` to the runtime.
- `contract/src/read_query.rs::every_vector_encodes_to_its_query_and_decodes_back`: the encoder half to the CLI's route client (tab 85), the decoder half to the plain parser in `http-route`.
