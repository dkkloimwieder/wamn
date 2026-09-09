# Fresh-only boundary: partial evidence

`wamn-ctc8.15.4` remains open. This report covers the boundary commit
`43b2fa33`, based on `fbc77b75`.
The tests ran before the commit, against the same source changes.

A PAT is a personal access token. A fresh-only operation requires the original
caller's fresh PAT. The commit carries the operation metadata, the host guard,
and the consumers of the WIT host-to-guest interface.
The owner requires this boundary before cutover. No session route activation or
live proof forms part of this result. This report makes no latency claim.

## Cutover handoff

Merge `a0ead75a` combines the boundary with main `17c53970` and preserves its TUI changes.
Commit `22384f96` supplies the `kind` field that main requires in the new generator test.
The final combined-source run tests that source.
The unfinished proof stays on `work/ctc8-15-4-fresh-only-20260909` at `b978e916`.
That checkpoint does not form part of this boundary.

The boundary changes these fenced files:

- `crates/execution/host/src/router_delivery.rs`
- `crates/execution/host/src/router_driver.rs`
- `crates/execution/host/wit/deps/wamn-router-delivery/package.wit`
- `crates/platform/runtime/src/component_admission.rs`
- `crates/platform/runtime/src/component_artifact.rs`
- `crates/platform/runtime/src/component_artifact_source.rs`
- `crates/platform/runtime/src/plugins/connection_http.rs`
- `crates/platform/runtime/src/plugins/flow_http_routing.rs`
- `crates/platform/runtime/src/plugins/wamn_blobstore/plugin.rs`
- `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs`
- `crates/platform/runtime/src/release_manifest.rs`

The runtime changes only add `fresh_only: false` to test fixtures.
No changes affect `services/host`, `services/executor`, or `deploy/`.
No identity work remains uncommitted in the fenced paths.
Further edits there wait for cutover stage 3 on main and a rebase onto that commit.

The shared CLI files are `services/ctl/src/push_component.rs`, `publish_release.rs`,
`author_wiring.rs`, `dev/read.rs`, and `dev/tui.rs`.
Two CLI fixtures also change: `services/ctl/tests/author_wiring_gate_report_live.rs`
and `services/ctl/tests/bind_connection_live.rs`.
The merge preserves the TUI edits in `dev/read.rs` and `dev/tui.rs`.

## Local results

All four completed commands exited 0. The counts below come from the retained
test summaries. Counts do not establish live database or deployed-host coverage.

| Scope | Result | Raw log |
| --- | --- | --- |
| Host | 31 passed | [Host tests](local-001/host-test.log) |
| HTTP route and materializer | 28 passed | [Final component tests](local-001/components-final.log) |
| Catalog, generator, and CLI | 438 passed, 28 ignored, across 29 test summaries | [Metadata run 002](local-001/metadata-test-002.log) |
| WASI consumers | Compilation passed for `wasm32-wasip2` | [WASI compilation](local-001/components-wasm-check.log) |

The [initial metadata command](local-001/metadata-test.log) exited 101 before
compilation. The integration manifest inherited `wat` from a workspace dependency
that did not exist. The corrected run supplies the metadata result above.
The [earlier component run](local-001/components-test.log) also passed 28 tests.
It does not add another 28 distinct tests.

The exact commands ran from
`/home/kaalin/.cache/wamn-lanes/ctc8-15-4-fresh-only-20260909`:

```sh
cargo test -p wamn-execution-host --lib --offline --no-fail-fast
cargo test -p http-route -p materializer --manifest-path components/Cargo.toml --all-targets --locked --offline --no-fail-fast
cargo test -p wamn-catalog -p wamn-schema-generator -p wamn-ctl --lib --tests --offline --no-fail-fast
cargo check -p http-route -p materializer --manifest-path components/Cargo.toml --target wasm32-wasip2 --locked --offline
```

## Combined-source results

The [first combined run](local-001/integrated-boundary.log) exited 101 with 541 passes, two failures, and 28 ignored tests.
The new generator fixture omitted the `kind` field that main requires.
Commit `22384f96` fixes that fixture without weakening the parser.

The unchanged TUI launch test also failed with `Text file busy` from its generated executable.
Bead `wamn-10yt.62.9` records this failure and its unresolved cause.

The [second combined run](local-001/integrated-boundary-002.log) exited 0 with 543 passes, zero failures, and 28 ignored tests.
It includes 33 test summaries and the unchanged TUI launch test.
This is an affected-package run, not the full workspace sweep or the deployed gate.
Both runs use this command:

```sh
cargo test -p wamn-catalog -p wamn-schema-generator -p wamn-ctl -p wamn-execution-host --lib --tests --locked --offline --no-fail-fast
```

## Deliberate faults

Mutation tests introduce deliberate faults. Each of these three changed programs
compiled, failed its intended assertion, and exited 101. Each restored source
matched its recorded original SHA-256 hash and passed its test scope with exit 0.
These tests did not remove or bypass the host guard.

| Fault | Intended failure | Restored result |
| --- | --- | --- |
| Catalog flag | `fresh_only_is_preserved_only_for_registered_operations` failed at `facts.component.operations[registered].fresh_only` | [2 passed](mutations-001/catalog-flag.restored.log), after [1 passed and 1 failed](mutations-001/catalog-flag.log) |
| Nested error type | `nested_fresh_only_refusal_retains_its_exact_wire_contract` failed because the nested boundary lost the operation refusal | [5 passed](mutations-001/nested-type.restored.log), after [4 passed and 1 failed](mutations-001/nested-type.log) |
| Admission agreement | `fresh_only_requires_authored_component_and_generated_contract_agreement` failed because the authored and component flags differed | [3 passed](mutations-001/admission-agreement.restored.log), after [2 passed and 1 failed](mutations-001/admission-agreement.log) |

The retained records name the failing tests above, but do not preserve all earlier
mutation command flags. This report does not reconstruct those commands.
The final admission restoration used this exact command:

```sh
cargo test -p wamn-ctl --lib fresh_only --locked --offline --no-fail-fast
```

## Remaining proof

The live proof must pair direct and nested session refusals with successful PAT
calls from the same human. It must show next-request refusal after role removal
and after project-environment membership removal. It must also show that a later
fresh-only refusal leaves earlier committed work intact.

Local metadata tests cover flag propagation, strict boolean values, private-export
refusal, and agreement between authored, generated, and admitted declarations.
The default false value preserves existing bytes, and the release manifest remains
format 1. These local results do not close the live-proof requirement or authorize
session route activation.
