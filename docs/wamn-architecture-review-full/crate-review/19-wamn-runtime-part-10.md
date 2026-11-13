# 19 wamn-runtime, part 10: blob store plugin

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

All under `plugins/wamn_blobstore/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `plugin.rs` | 1,180 | 677 |
| `files.rs` | 1,097 | 541 |
| `store.rs` | 670 | 355 |
| `host.rs` | 579 | 479 |
| `binding.rs` | 375 | 194 |
| `intake.rs` | 247 | 160 |
| `confinement.rs` | 199 | 98 |
| `wit_error.rs` | 198 | 74 |
| `drain.rs` | 133 | 133 |
| `bindings.rs` | 51 | 51 |
| `mod.rs` | 39 | 39 |
| Total | 4,768 | 2,801 |

Callers read: `services/host/src/host.rs` (plugin setup), `connection_http.rs` (closure and snapshot code, for comparison), `connection_health.rs`, `pod_identity.rs::providers`, `crates/catalog/model/src/connection.rs::check_definition`, `crates/execution/host/src/file_route.rs` (connection checks, the two `file_store` calls), `file_collector.rs`, `native_policy.rs` (bind and revoke).

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `plugin.rs` | Open a confined container for a guest or a file route | Repeats the closure authorization of `connection_http.rs`; builds stores in two places |
| `files.rs` | Staging, live and public objects for file routes | One purpose |
| `store.rs` | One confined container and its verbs | One purpose |
| `host.rs` | WIT handlers | One purpose |
| `binding.rs` | Definition → coordinates | A second parser of the definition |
| `intake.rs`, `drain.rs` | Bounded, complete body before a write | One purpose |
| `confinement.rs` | Key rules | One purpose |
| `wit_error.rs` | The one error translation | One purpose |

## Findings

### 1. Closure authorization is written twice

- **Fact.** `connection_http.rs` and `plugin.rs::container_for` both do the same steps: pick release or candidate, check env\_key and package, read digest and environment, find the frozen candidate binding, build the same `ConnectionEffectLookup`, read the snapshot, call the release or candidate closure rule.
- **Fact.** The blob store wraps the steps in `release_coordinates` and `authorize_closure`. The HTTP file has them inline. About 90 lines each.
- **Option.** One function in the connection authority module: invocation + alias → authorized snapshot. Both plugins call it. `release_coordinates`, `authorize_closure` and the inline HTTP copy go.

### 2. Third copy of the invocation map

- **Fact.** `WamnBlobstore.invocations` with `bind_invocation`, `revoke_invocation`, `invocation` holds the same `ConnectionInvocation` that `native_policy.rs` also binds into the Postgres plugin and the HTTP plugin.
- **Option.** Joins the held scope record (tab 15 finding 2). I added this file to that held row.

### 3. The object store is built in two places

- **Fact.** `file_store` and `container_for` each match on the provider, read the secret, and call `build_s3_store` or `build_gcs_store`.
- **Option.** One function: binding + connection fact → store and signer. Both paths call it.

### 4. A new store client for every open, and the pod token cache never survives

- **Fact.** Each guest `get-container` and each file request builds a new object-store client.
- **Fact.** `build_gcs_store` calls `pod_identity::providers()`, which creates a new `PodToken` with an empty cache each time.
- **Inference.** On GCS, each open fetches a new token from the metadata server. Not run.
- **Option.** Keep the built store per (connection id, generation) in a bounded `moka` map, next to the generation map in `ConnectionFacts`. Uses the cache crate ruled in tab 17. A changed generation gives a new key, so no extra drop rule is needed.

### 5. The definition is parsed and checked in four places

- **Fact.** `wamn_catalog::RequirementType::check_definition` checks the keys and the `cdn_host` origin.
- **Fact.** `binding.rs::resolve_coordinates` reads the same keys again and calls `check_definition` again for `cdn_host`.
- **Fact.** `files.rs::FileStore::new` parses and checks `cdn_host` a third time.
- **Fact.** `plugin.rs::health` reads `container`, `prefix`, `endpoint` with its own code.
- **Fact.** `file_store` writes `provider` into the JSON, then reads it back to decide GCS.
- **Option.** One typed parse in `wamn-catalog`: provider, endpoint, container, prefix, `cdn_host` as a URL. Same direction as tab 15 finding 5. `binding.rs::coordinate`, `MissingCoordinate`, the `cdn_host` code in `FileStore::new`, the field code in `health` and the JSON write-back go.

### 6. `"disk"` is accepted and can never work

- **Fact.** `file_store` and `file_route.rs` accept integration types `gcs`, `s3`, `disk`. `check_definition` and `resolve_coordinates` refuse every provider except `s3` and `gcs`.
- **Option.** Delete `"disk"` in both places.

### 7. The S3 secret shape is parsed here

- **Fact.** `build_s3_store` reads `ACCESS_KEY_ID` and `ACCESS_SECRET_KEY` from the secret JSON by hand.
- **Option.** A typed S3 secret in the one secret read ruled in tab 16 finding 6.

### 8. Optional fields that production always sets

- **Fact.** `host.rs` builds the plugin only with a release, and passes that release's env\_key. So `release: Option` is always `Some`, and the check `scope.env_key != env_key` compares a value with itself.
- **Fact.** Both production builders set `BoundContainer.authority`. `file_store` always passes a signer. `FileStore.generation` is read only by a `< 1` check and by `Debug`.
- **Option.** `release` required and `env_key` taken from it (same as tab 15 finding 11). `authority` and `signer` required. Delete `FileStore.generation`.

### 9. "Stable wire codes" that nothing reads

- **Fact.** `StoreError::code`, `IntakeError::code` and `BindingError::code` have test readers only. `KeyRefusal::code` is read by `StoreError`'s display text.
- **Fact.** The guest sees the WIT error from `wit_error.rs`. Most `BindingError` variants are turned into `Unauthorized` and then into `no-such-container`.
- **Option.** Delete the three `code` functions. `KeyRefusal::code` stays as display text.

### 10. Small unused or unreachable parts

- **Fact.** `Intake::new`, `Intake::default` and `Intake::observed` have test callers only. Production calls `with_limit`.
- **Fact.** `KeyRefusal::EscapedPrefix`: its own comment says it should be unreachable.
- **Fact.** `wit_error.rs` comment says a retryable backend error maps to `store-unavailable`. The code returns `timeout`. Nothing produces `store-unavailable`.
- **Option.** Delete the three `Intake` functions and the `EscapedPrefix` check. Correct the comment.

### 11. Whole objects in memory for a range read

- **Fact.** `get_data` reads the complete object, then cuts the requested range. `object_store` has a range read.
- **Option.** Use the range read. The repository rule is memory before CPU.

### 12. Reserved file keys are spelled several times, with an old layout

- **Fact.** `permit_raw_mutation` writes `_wamn_files` and `public/_wamn_files` by hand. `public_key`, `staging_key` and `live_objects` build the same names again.
- **Fact.** The rule accepts keys with and without the environment segment. The comment calls the form without it "legacy". `live_key` always writes the environment segment.
- **Option A (recommended, per "refuse old rows").** One set of name functions. Delete the form without the environment segment.
- **Option B.** One set of name functions. Keep both forms.
- **Open fact.** Whether any deployed bucket holds objects in the old layout. I cannot see that from the code.

## Carried

- `file_route.rs` checks active fact, generation, runtime type, integration type and version. `file_store` then checks the same again, after finding the connection again by name. To the `execution/host` tab, where all of `file_route.rs` is read.
- `pod_identity.rs` uses `reqwest`. Its option comes with the root files tab.

## Fine as is

- `drain.rs` and `intake.rs`: a write happens only after a complete, bounded body.
- `confinement.rs::resolve_key`: one place builds a store path.
- `wit_error.rs`: one translation to the WIT error.
- Refused verbs (`create-container`, `delete-container`, `copy-object`, `move-object`, `info`) each say why.
- `host.rs::instrumented`: one wrapper for span, outcome and duration. This is the shape tab 18 finding 9 gives the event plugin.

## Tests

**Delete**

- `intake.rs::the_standard_ceiling_is_the_documented_constant` (finding 10; it tests `Intake::new`)
- `binding.rs::an_empty_coordinate_is_treated_as_absent`, `an_absent_coordinate_refuses_rather_than_widening`: covered by the catalog parse (finding 5)
- `plugin.rs::a_manifest_for_another_env_key_refuses`: the two values become one (finding 8)

**Move**

- `plugin.rs`, 13 closure tests (`a_candidate_closure_carries_its_own_coordinates` through `a_released_closure_the_manifest_does_not_carry_refuses`): move with the one authorization function (finding 1). Any that repeat an HTTP test of the same rule are deleted there, one test per rule.
- `binding.rs::cdn_host_is_optional_and_malformed_values_refuse`, `a_gcs_binding_parses_and_refuses_a_credential_handle`: move to `wamn-catalog` (finding 5)

**Change**

- `intake.rs`, 4 tests: use `with_limit`; drop `observed()` and `code()` asserts (findings 9, 10)
- `store.rs`, 3 asserts on `code()`: assert the variant (finding 9)
- `files.rs` fixture `store(..)` and `store.rs` fixtures: signer and authority are required (finding 8)
- `files.rs::raw_mutations_cannot_overwrite_or_remove_private_or_public_files`: drops the old layout cases (finding 12 option A)
- `crates/control/environment/tests/connection_binding_live.rs`, `crates/execution/host/tests/files_live.rs`, `native_policy/tests.rs`: plugin setup only (finding 8)

## Not read

- Test bodies: names and call sites only.
- `crates/execution/host/src/file_route.rs`: lines around the connection checks and the two `file_store` calls only.
- `file_collector.rs`: the `file_store` call only.
- The WIT of `wasmcloud:blobstore`.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1 to 11: accepted.
- Finding 12: **option A.** One set of name functions. The form without the environment segment is deleted.
- Tab 19 is fully ruled.
