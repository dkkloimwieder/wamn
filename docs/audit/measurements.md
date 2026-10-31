# Audit measurements, 2026-10-30

This file holds the tool output that every audit lane reads. Root ran the tools on the `worktree-audit` worktree at commit 788cdadb4 with its own Cargo target directory. Raw outputs sit under `raw/`. Each section names the tool, the exact command, and the result. A lane cites a row of this file as evidence.

## 1. Lines by language (tokei)

Command: `tokei --exclude vendor --exclude node_modules --exclude target --exclude apps/target -s lines`. The `web/wfe` submodule was not checked out at this run.

```text
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 Language              Files        Lines         Code     Comments       Blanks
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 YAML                     83        46235        43221         1937         1077
 JSON                    321        14334        14332            0            2
 TSX                      44        11437        10960          162          315
 TypeScript               81         8383         6865          923          595
 SQL                      91         7286         5551         1409          326
 BASH                     28         4211         3691          286          234
 JavaScript               16         3625         3416          122           87
 TOML                     92         3272         2463          493          316
 K                        25         2455         2343            0          112
 Shell                     7         1770         1572           98          100
 Autoconf                 22         1162         1160            0            2
 Python                   13         1136          968           31          137
 WebAssembly               5          587          564           22            1
 SVG                       2          495          483            0           12
 Dockerfile                4          353          232           98           23
 CSS                       2           64           63            0            1
 HTML                      4           49           49            0            0
 Pan                       1           23           21            0            2
 Plain Text                1            3            0            3            0
─────────────────────────────────────────────────────────────────────────────────
 Rust                   1048       439736       408735         7103        23898
 |- Markdown             994        32219            3        29172         3044
 (Total)                           471955       408738        36275        26942
─────────────────────────────────────────────────────────────────────────────────
 Markdown                126        29367            0        21841         7526
 |- BASH                  23         1995         1964           19           12
 |- JSON                   8           52           52            0            0
 |- Python                 1            3            3            0            0
 |- Rust                   3           35           32            2            1
 |- SQL                    9          644          584           46           14
 |- TOML                   1            1            1            0            0
 |- YAML                   1           18           18            0            0
 (Total)                            32115         2654        21908         7553
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 Total                  2016       610950       509346        63767        37837
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## 2. Counts per crate (rg)

Columns: src lines, tests/ lines, `unwrap()` or `.expect(` in src (test modules included), `unsafe ` in src, `#[ignore` in the crate, files in src that mention anyhow, `Command::new` in src, `include_str!` of a .sql file in src. Sorted by src lines. Commands: `find <crate>/src -name '*.rs' | xargs cat | wc -l` and `rg -c <pattern> <crate>/src`.

| Crate | src | tests | unwrap/expect | unsafe | ignore | anyhow files | Command::new | include_str sql |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `crates/control/lib` | 80688 | 32437 | 2017 | 0 | 41 | 121 | 71 | 5 |
| `crates/platform/runtime` | 45267 | 7071 | 1348 | 2 | 18 | 27 | 2 | 4 |
| `crates/schema/generator` | 34991 | 14674 | 852 | 2 | 0 | 11 | 10 | 2 |
| `tests/integration` | 17506 | 0 | 233 | 1 | 22 | 30 | 29 | 1 |
| `crates/control/provision` | 12596 | 10479 | 147 | 0 | 3 | 0 | 0 | 10 |
| `crates/execution/host` | 12345 | 4883 | 316 | 0 | 0 | 16 | 2 | 0 |
| `crates/platform/engine` | 12068 | 0 | 337 | 1 | 0 | 19 | 1 | 0 |
| `crates/catalog/model` | 10062 | 738 | 226 | 0 | 0 | 0 | 0 | 5 |
| `services/ctl` | 7640 | 4162 | 115 | 0 | 7 | 24 | 1 | 0 |
| `crates/schema/introspection` | 6829 | 4375 | 12 | 0 | 1 | 0 | 0 | 0 |
| `crates/identity/platform` | 6732 | 4725 | 86 | 0 | 2 | 4 | 0 | 0 |
| `crates/schema/control` | 6225 | 147 | 65 | 1 | 0 | 0 | 0 | 2 |
| `services/edge` | 5001 | 1541 | 85 | 0 | 1 | 14 | 0 | 0 |
| `crates/execution/workflow` | 4771 | 486 | 80 | 0 | 1 | 7 | 0 | 0 |
| `services/host` | 4453 | 1225 | 98 | 3 | 2 | 4 | 0 | 0 |
| `crates/execution/run-state` | 4145 | 2566 | 56 | 0 | 0 | 0 | 0 | 3 |
| `tests/conformance` | 4107 | 6128 | 48 | 0 | 4 | 2 | 5 | 2 |
| `test-support/harness` | 3744 | 0 | 39 | 0 | 1 | 9 | 13 | 0 |
| `services/identity` | 3487 | 4419 | 130 | 0 | 1 | 0 | 0 | 0 |
| `services/scenario-worker` | 3195 | 2200 | 27 | 0 | 0 | 4 | 0 | 0 |
| `services/cdc-reader` | 3127 | 1256 | 34 | 0 | 2 | 2 | 1 | 0 |
| `crates/control/registry` | 2570 | 352 | 12 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/ingress/http-route` | 2051 | 1161 | 12 | 0 | 0 | 0 | 0 | 0 |
| `crates/client/core` | 2035 | 1490 | 7 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/data/postgres-sqlx` | 1667 | 0 | 45 | 0 | 0 | 0 | 0 | 0 |
| `crates/catalog/package-artifact` | 1463 | 120 | 27 | 0 | 0 | 3 | 0 | 0 |
| `crates/platform/component-composer` | 1301 | 0 | 54 | 0 | 0 | 3 | 0 | 0 |
| `apps/platform/events/materializer` | 1285 | 0 | 37 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/execution/contract` | 1200 | 0 | 16 | 0 | 0 | 0 | 0 | 0 |
| `tools/bench` | 1109 | 0 | 1 | 0 | 0 | 1 | 0 | 1 |
| `apps/platform/execution/materializer` | 1100 | 0 | 11 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/data/access` | 1034 | 0 | 42 | 0 | 0 | 0 | 0 | 0 |
| `crates/identity/client` | 942 | 0 | 64 | 0 | 0 | 1 | 0 | 0 |
| `apps/platform/data/postgres-statements` | 884 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/events/wire` | 882 | 0 | 17 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/no-std/http-page` | 880 | 0 | 17 | 0 | 0 | 0 | 0 | 0 |
| `crates/authoring/model` | 840 | 599 | 8 | 0 | 0 | 0 | 0 | 0 |
| `crates/platform/component-policy` | 819 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |
| `crates/identity/session` | 680 | 134 | 2 | 0 | 0 | 0 | 0 | 0 |
| `crates/identity/credential-store` | 661 | 0 | 29 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/data/record-history` | 644 | 352 | 0 | 0 | 0 | 0 | 0 | 0 |
| `tests/system` | 566 | 1123 | 20 | 0 | 0 | 1 | 0 | 0 |
| `crates/platform/number-pattern` | 552 | 0 | 11 | 0 | 0 | 0 | 0 | 0 |
| `crates/identity/project-state` | 526 | 1550 | 2 | 0 | 0 | 0 | 0 | 1 |
| `crates/execution/run-state-sqlite` | 437 | 210 | 0 | 0 | 1 | 0 | 0 | 0 |
| `apps/platform/events/registration` | 405 | 236 | 1 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/no-std/http-request` | 381 | 0 | 8 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/execution/blob-put` | 353 | 0 | 2 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/wamn_integration/component` | 350 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/execution/jsonata` | 301 | 0 | 9 | 0 | 0 | 0 | 0 | 0 |
| `test-support/fixture-package` | 276 | 0 | 17 | 0 | 0 | 0 | 0 | 0 |
| `crates/platform/pg-core` | 241 | 0 | 2 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/no-std/label-template` | 225 | 160 | 3 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/no-std/label-render` | 190 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/fixtures/sockprobe` | 140 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/fixtures/connection-http-standard` | 125 | 0 | 0 | 11 | 0 | 0 | 0 | 0 |
| `apps/platform/no-std/transform` | 124 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/fixtures/std-virtualization-probe` | 104 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `apps/platform/fixtures/sqlx-command` | 104 | 0 | 1 | 0 | 0 | 0 | 0 | 0 |
| `tests/orchestrator` | 99 | 0 | 0 | 0 | 0 | 1 | 0 | 0 |
| `crates/platform/component-virtualizer` | 92 | 0 | 0 | 0 | 0 | 1 | 0 | 0 |
| `apps/platform/fixtures/busyloop` | 20 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `test-support/postgres` | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 |
| `test-support/infrastructure` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `apps/wamn_wms/tests` | 0 | 0 | 0 | 0 | 13 | 0 | 0 | 0 |
| `apps/wamn_receiving/tests` | 0 | 0 | 0 | 0 | 15 | 0 | 0 | 0 |

## 3. Pedantic and nursery clippy (cargo clippy)

Command: `cargo clippy --workspace --all-targets --keep-going --message-format=json -- -W clippy::pedantic -W clippy::nursery -A clippy::must_use_candidate -A clippy::missing_errors_doc -A clippy::doc_markdown -A clippy::too_many_lines -A clippy::missing_panics_doc`. The five allowed lints are the ones the workspace manifest already opts out of. Warnings are counted once per rendered message.

Total warnings: 1691. Compile errors: 16, all of them `include_str!` of generated files under `apps/target/wamn/` that no `wamn build` produced in this worktree. `cargo clippy --all-targets` therefore needs built application packages, which `tools/repo-lint` does not state.

| Lint | Count |
| --- | ---: |
| `clippy::missing_const_for_fn` | 360 |
| `clippy::redundant_pub_crate` | 351 |
| `clippy::use_self` | 223 |
| `clippy::significant_drop_tightening` | 197 |
| `clippy::too_long_first_doc_paragraph` | 190 |
| `clippy::assert_is_empty` | 88 |
| `clippy::option_if_let_else` | 57 |
| `clippy::future_not_send` | 47 |
| `clippy::redundant_clone` | 35 |
| `clippy::derive_partial_eq_without_eq` | 28 |
| `clippy::literal_string_with_formatting_args` | 26 |
| `clippy::or_fun_call` | 22 |
| `clippy::return_self_not_must_use` | 13 |
| `clippy::suspicious_operation_groupings` | 9 |
| `clippy::iter_on_single_items` | 9 |
| `clippy::needless_collect` | 8 |
| `clippy::needless_pass_by_ref_mut` | 8 |
| `clippy::useless_let_if_seq` | 3 |
| `clippy::significant_drop_in_scrutinee` | 3 |
| `clippy::uninhabited_references` | 3 |
| `clippy::suboptimal_flops` | 3 |
| `clippy::equatable_if_let` | 3 |
| `clippy::doc_link_code` | 2 |
| `clippy::while_float` | 2 |
| `clippy::tuple_array_conversions` | 1 |

| Crate | Warnings |
| --- | ---: |
| `crates/platform/runtime` | 250 |
| `crates/control/lib` | 214 |
| `crates/schema/generator` | 207 |
| `crates/execution/run-state` | 164 |
| `crates/execution/host` | 129 |
| `crates/control/provision` | 88 |
| `crates/control/registry` | 85 |
| `crates/execution/workflow/router` | 51 |
| `crates/platform/engine` | 46 |
| `crates/identity/project-state` | 44 |
| `crates/catalog/model` | 41 |
| `crates/schema/control` | 41 |
| `services/identity` | 38 |
| `apps/wamn_wms/tests` | 32 |
| `crates/identity/platform` | 31 |
| `tests/integration` | 29 |
| `services/edge` | 23 |
| `services/cdc-reader` | 20 |
| `crates/execution/workflow` | 20 |
| `services/ctl` | 18 |
| `tests/conformance` | 14 |
| `crates/client/core` | 13 |
| `crates/schema/introspection` | 12 |
| `test-support/harness` | 10 |
| `test-support/infrastructure` | 8 |
| `services/host` | 8 |
| `crates/platform/component-policy` | 7 |
| `crates/platform/pg-core` | 7 |
| `crates/authoring/model` | 7 |
| `services/scenario-worker` | 7 |
| `crates/platform/number-pattern` | 4 |
| `tools/submodule-check` | 4 |
| `crates/identity/session` | 3 |
| `crates/catalog/package-artifact` | 3 |
| `tools/bench` | 3 |
| `crates/identity/client` | 2 |
| `crates/platform/component-composer` | 2 |
| `crates/execution/run-state-sqlite` | 2 |
| `tests/system` | 2 |
| `test-support/postgres` | 1 |
| `crates/identity/credential-store` | 1 |

Compile errors (first line of each):

```text
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/wamn.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/client_acme_receiving/wamn.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/sql/purchase_order/update.sql`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/client_acme_receiving/sql/purchase_order/get.sql`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/client_acme_receiving/sql/purchase_order/update.sql`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/contracts/purchase_order/get.result.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/sql/receiving_load_open_receipt_lines/feed_changes.sql`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/platform-policy/data-access.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/platform-policy/data-access.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/client_acme_receiving/platform-policy/data-access.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/wamn_receiving/platform-policy/data-access.json`: No such file or directory (os error 2)
error: couldn't read `apps/wamn_receiving/tests/../../../apps/target/wamn/client_acme_receiving/platform-policy/data-access.json`: No such file or directory (os error 2)
error: couldn't read `crates/control/lib/src/qualify_upgrade/../../../../../apps/target/wamn/platform_fixture_overlay/wamn.json`: No such file or directory (os error 2)
error: couldn't read `crates/client/core/tests/../../../../apps/target/wamn/platform_fixture/routes/widget/update.json`: No such file or directory (os error 2)
error: couldn't read `crates/client/core/tests/../../../../apps/target/wamn/platform_fixture/routes/widget/update.json`: No such file or directory (os error 2)
error: couldn't read `crates/schema/generator/tests/../../../../apps/target/wamn/platform_fixture/sql/widget/query_created_at_ascending.sql`: No such file or directory (os error 2)
```

## 4. Unused dependencies (cargo machete)

Command: `cargo machete --with-metadata` at the repository root. machete could not parse the guest workspace manifests of `apps/wamn_wms/tests` and `apps/wamn_receiving/tests` because `apps/Cargo.toml` has no workspace dependency `anyhow`; those two crates are unmeasured. machete reports a dependency as unused when no `use` or path names it, so a crate used only through a macro or a feature is a false positive. A lane confirms each row.

```text
Analyzing dependencies of crates in this directory...
error when handling ./apps/wamn_wms/tests/Cargo.toml: can't load root workspace at /home/kaalin/dev/wamn/.claude/worktrees/audit/apps/Cargo.toml: workspace dependencies are missing `anyhow`: workspace dependencies are missing `anyhow`
error when handling ./apps/wamn_receiving/tests/Cargo.toml: can't load root workspace at /home/kaalin/dev/wamn/.claude/worktrees/audit/apps/Cargo.toml: workspace dependencies are missing `anyhow`: workspace dependencies are missing `anyhow`
cargo-machete found the following unused dependencies in this directory:
wamn-ctl -- ./services/ctl/Cargo.toml:
	chrono
	serde
	wamn-authoring-model
	wit-component
	wit-parser
wamn-gate-harness -- ./test-support/harness/Cargo.toml:
	async-nats
	wamn-event-wire
	wamn-schema-control
	wamn-test-postgres
wamn-test-infrastructure -- ./test-support/infrastructure/Cargo.toml:
	reqwest
	wamn-event-wire
wamn-system-tests -- ./tests/system/Cargo.toml:
	wamn-run-state
wamn-integration-tests -- ./tests/integration/Cargo.toml:
	async-trait
	proptest
	ring
	tokio-rustls
	wamn-client
	wamn-event-reg
	wamn-schema-introspection
	wasm-encoder
	wasmparser
	wat
wamn-gates -- ./tests/orchestrator/Cargo.toml:
	wamn-runtime
wamn-conformance-tests -- ./tests/conformance/Cargo.toml:
	bytes
	chrono
	http-body-util
	hyper
	hyper-util
	ring
	rust_decimal
	sqlx
	uuid
	wamn-execution-contract
	wamn-execution-host
	wamn-runtime
	wasmtime-wasi
wamn-modbus -- ./integrations/modbus/Cargo.toml:
	dlmalloc
wamn-schema-generator -- ./crates/schema/generator/Cargo.toml:
	wamn-event-wire
wamn-control -- ./crates/control/lib/Cargo.toml:
	tokio-rustls
wamn-platform-identity -- ./crates/identity/platform/Cargo.toml:
	wamn-catalog
wamn-runtime -- ./crates/platform/runtime/Cargo.toml:
	percent-encoding
	rand_core
wamn-run-state -- ./crates/execution/run-state/Cargo.toml:
	wamn-pg-core
wamn-workflow -- ./crates/execution/workflow/Cargo.toml:
	boon
	croner
	ring
wamn-execution-host -- ./crates/execution/host/Cargo.toml:
	base64
	boon
	ring
	wamn-run-state
	wamn-schema-control
wamn-client -- ./crates/client/core/Cargo.toml:
	rust_decimal
client-acme-receiving -- ./apps/client_acme_receiving/component/Cargo.toml:
	serde
	serde_json
	uuid
wamn-client-acme-receiving-data-access -- ./apps/client_acme_receiving/data/Cargo.toml:
	futures-executor
	serde
	serde_json
	wamn-data-access
rest-connector -- ./apps/rest_connector/component/Cargo.toml:
	serde
	uuid
rest-consumer-fixture -- ./apps/rest_consumer_fixture/component/Cargo.toml:
	uuid
netsuite -- ./apps/netsuite/component/Cargo.toml:
	jsonata-expression
	serde
	uuid
wms -- ./apps/wamn_wms/component/Cargo.toml:
	serde
	serde_json
	uuid
	wamn-execution-contract
wamn-wms-data-access -- ./apps/wamn_wms/data/Cargo.toml:
	wamn-data-access
modbus-reader -- ./apps/modbus_reader/component/Cargo.toml:
	serde
	serde_json
	uuid
fixture-overlay -- ./apps/platform_fixture_overlay/component/Cargo.toml:
	serde
	serde_json
	uuid
	wamn-data-access
	wamn-postgres-statements
receiving -- ./apps/wamn_receiving/component/Cargo.toml:
	serde
	uuid
	wamn-execution-contract
wamn-receiving-data-access -- ./apps/wamn_receiving/data/Cargo.toml:
	wamn-data-access
label-render -- ./apps/platform/no-std/label-render/Cargo.toml:
	dlmalloc
http-request -- ./apps/platform/no-std/http-request/Cargo.toml:
	dlmalloc
http-page -- ./apps/platform/no-std/http-page/Cargo.toml:
	dlmalloc
transform -- ./apps/platform/no-std/transform/Cargo.toml:
	dlmalloc
wamn-integration -- ./apps/platform/wamn_integration/component/Cargo.toml:
	serde
	uuid
wamn-data-access -- ./apps/platform/data/access/Cargo.toml:
	chrono
samples -- ./apps/edge_samples/component/Cargo.toml:
	serde
	serde_json
	uuid
	wamn-execution-contract
wamn-edge-samples-data-access -- ./apps/edge_samples/data/Cargo.toml:
	wamn-data-access
	wamn-execution-contract
device -- ./apps/edge_device/component/Cargo.toml:
	serde
	serde_json
	uuid
fixture -- ./apps/platform_fixture/component/Cargo.toml:
	serde
	serde_json
	uuid
	wamn-execution-contract
wamn-platform-fixture-data-access -- ./apps/platform_fixture/data/Cargo.toml:
	wamn-data-access

If you believe cargo-machete has detected an unused dependency incorrectly,
you can add the dependency to the list of dependencies to ignore in the
`[package.metadata.cargo-machete]` section of the appropriate Cargo.toml.
For example:

[package.metadata.cargo-machete]
ignored = ["prost"]

Done!
```

## 5. Advisories (cargo audit)

Command: `cargo audit fetch` then `cargo audit` on the root lockfile and `cargo audit --file apps/Cargo.lock`. The advisory database was refreshed on 2026-10-30. The earlier local copy was dated 2026-04-07 and reported zero findings.

### Root lockfile

error: 17 vulnerabilities found! warning: 12 allowed warnings found

| Crate | Advisory | Title |
| --- | --- | --- |
| ctutils | RUSTSEC-2026-0385 | Non-constant-time mask generation and `u128` comparisons |
| rustls | RUSTSEC-2026-0347 | Buffered application data sent to outer server after ECH rejection |
| rustls | RUSTSEC-2026-0346 | `Debug` impls for client session values expose TLS secrets |
| rustls | RUSTSEC-2026-0349 | Unauthenticated ServerHello can downgrade later connections to classical key exchange |
| rustls | RUSTSEC-2026-0348 | Brotli certificate decompression can allocate up to 1 GiB before authentication |
| rustls-webpki | RUSTSEC-2026-0382 | Certificate verification may accept distrusted intermediates |
| wasmtime | RUSTSEC-2026-0424 | Unchecked memory usage in transferring borrowed resources between components |
| wasmtime | RUSTSEC-2026-0423 | Cross-engine type confusion with host-created struct-types-with-supertypes |
| wasmtime | RUSTSEC-2026-0422 | Typed functions with `i31` or `nullref` arguments may panic due to insufficient type-check |
| wasmtime-wasi | RUSTSEC-2026-0420 | wasip3 UDP `send` can deliver a datagram to an address only allowed for receiving |
| wasmtime-wasi-http | RUSTSEC-2026-0421 | wasi-http p2: outgoing content-length can be invalid or not match body |
| webpki-root-certs | RUSTSEC-2026-0383 | Bundled root certificates allow trust in distrusted emSign intermediates |
| webpki-roots | RUSTSEC-2026-0381 | Bundled root certificates allow trust in distrusted emSign intermediates |
| zstd | RUSTSEC-2026-0391 | Data from an abandoned stream leaks into the next stream on a reused context |
| zstd-safe | RUSTSEC-2026-0403 | Uninitialized memory exposed when streaming decompression of a legacy frame fails |
| zstd-safe | RUSTSEC-2026-0398 | `Seekable::decompress` sets a `Vec` length beyond its capacity for offsets past the end |
| zstd-sys | RUSTSEC-2026-0410 | WebAssembly allocator shim corrupts memory on allocation failure |
| unsafe-libyaml | RUSTSEC-2024-0449 | unsafe-libyaml is unmaintained |
| tokio | RUSTSEC-2026-0386 | `AsyncFd::new` and related constructors violate I/O safety |
| zstd-safe | RUSTSEC-2026-0396 | `Seekable` reads a NULL or out-of-bounds seek table |
| zstd-safe | RUSTSEC-2026-0406 | Use-after-free when changing a multithreaded context with an unfinished frame |
| zstd-safe | RUSTSEC-2026-0395 | Use-after-free with `CParameter::StableInBuffer` |
| zstd-safe | RUSTSEC-2026-0402 | Heap buffer overflow in `CCtx::try_clone` on a multithreaded context |
| zstd-safe | RUSTSEC-2026-0399 | Use-after-free through `init_using_cdict` and `init_using_ddict` |
| zstd-safe | RUSTSEC-2026-0404 | Double free in `DCtx::try_clone` |
| zstd-safe | RUSTSEC-2026-0397 | `OutBuffer::around_pos` exposes uninitialized memory |
| zstd-safe | RUSTSEC-2026-0405 | Out-of-bounds read with an out-of-range `InBuffer::pos` |
| zstd-safe | RUSTSEC-2026-0400 | Use-after-free of a dictionary after a one-shot call on a context with an open stream |
| zstd-safe | RUSTSEC-2026-0401 | Out-of-bounds read in `train_from_buffer` when sample sizes overflow |

### Guest lockfile apps/Cargo.lock

No findings.

## 6. Duplicate crate versions (cargo tree -d)

Command: `cargo tree -d --workspace`. Lines at depth zero (a crate with more than one version in the graph): 2022. Full output in `raw/tree-dup.txt`.

```text
[dev-dependencies]
base64 v0.22.1
base64 v0.23.1
block-buffer v0.10.4
block-buffer v0.12.1
byteorder v1.5.0
bytes v1.12.1
chrono v0.4.45
const-oid v0.10.2
const-oid v0.9.6
cpufeatures v0.2.17
cpufeatures v0.3.1
crypto-common v0.1.7
crypto-common v0.2.2
darling v0.20.11
darling v0.24.1
darling_core v0.20.11
darling_core v0.24.1
darling_macro v0.20.11
darling_macro v0.24.1
digest v0.10.7
digest v0.11.3
fallible-iterator v0.2.0
fallible-iterator v0.3.0
fixedbitset v0.4.2
fixedbitset v0.5.7
foldhash v0.1.5
foldhash v0.2.0
form_urlencoded v1.2.2
futures-channel v0.3.34
futures-sink v0.3.34
futures-util v0.3.34
getrandom v0.2.17
getrandom v0.3.4
getrandom v0.4.3
gimli v0.32.3
gimli v0.33.0
hashbrown v0.15.5
hashbrown v0.16.1
hashbrown v0.17.1
hmac v0.12.1
hmac v0.13.0
id-arena v2.3.0
idna v1.1.0
io-lifetimes v2.0.4
io-lifetimes v3.0.1
itertools v0.10.5
itertools v0.13.0
itertools v0.14.0
itertools v0.15.0
linux-raw-sys v0.12.1
linux-raw-sys v0.4.15
log v0.4.34
md-5 v0.11.0
nom v7.1.3
num-traits v0.2.19
object v0.39.1
petgraph v0.6.5
petgraph v0.8.3
postcard v1.1.3
prettyplease v0.2.37
prettyplease v0.3.0
proc-macro2 v1.0.107
prost v0.13.5
prost v0.14.4
prost-build v0.13.5
prost-build v0.14.4
prost-derive v0.13.5
prost-derive v0.14.4
prost-types v0.13.5
prost-types v0.14.4
pulley-interpreter v48.0.7
quote v1.0.47
rand v0.10.3
rand v0.8.8
rand v0.9.5
rand_chacha v0.3.1
rand_chacha v0.9.0
rand_core v0.10.1
rand_core v0.6.4
rand_core v0.9.5
regex v1.13.1
regex-automata v0.4.18
regex-syntax v0.8.11
rustc-hash v1.1.0
rustc-hash v2.1.3
rustix v0.38.44
rustix v1.1.5
semver v1.0.28
serde_core v1.0.229
serde_json v1.0.151
serde_spanned v0.6.9
serde_spanned v1.1.1
sha2 v0.10.9
sha2 v0.11.0
shlex v1.3.0
shlex v2.0.1
slab v0.4.12
smallvec v1.16.2
sqlx-core v0.9.0
sqlx-postgres v0.9.0
syn v2.0.119
syn v3.0.6
synstructure v0.13.2
synstructure v0.14.0
thiserror v1.0.69
thiserror v2.0.21
thiserror-impl v1.0.69
thiserror-impl v2.0.21
tokio v1.53.2
tokio-stream v0.1.19
toml v0.8.23
toml v0.9.12+spec-1.1.0
toml v1.1.6+spec-1.1.0
toml_datetime v0.6.11
toml_datetime v0.7.5+spec-1.1.0
toml_datetime v1.1.1+spec-1.1.0
untrusted v0.7.1
untrusted v0.9.0
url v2.5.8
uuid v1.27.0
wac-graph v0.10.1
wac-graph v0.12.0
wac-types v0.10.1
wac-types v0.12.0
wasm-encoder v0.245.1
wasm-encoder v0.247.0
wasm-encoder v0.252.0
wasm-encoder v0.253.0
wasm-encoder v0.254.2
wasm-encoder v0.258.3
wasm-encoder v0.259.0
wasm-encoder v0.261.0
wasm-metadata v0.247.0
wasm-metadata v0.252.0
wasm-metadata v0.253.0
wasm-metadata v0.254.2
wasm-metadata v0.258.3
wasm-metadata v0.259.0
wasmparser v0.245.1
wasmparser v0.247.0
wasmparser v0.252.0
wasmparser v0.253.0
wasmparser v0.254.2
wasmparser v0.258.3
wasmparser v0.259.0
wasmparser v0.261.0
wasmtime-environ v48.0.7
wasmtime-internal-core v48.0.7
wast v254.0.2
wast v35.0.2
whoami v2.1.3
winnow v0.7.15
winnow v1.0.4
wit-component v0.252.0
wit-component v0.253.0
wit-component v0.254.2
wit-component v0.259.0
wit-parser v0.252.0
wit-parser v0.253.0
wit-parser v0.254.2
wit-parser v0.259.0
│           [dev-dependencies]
│           └── nkeys v0.4.5
│           └── rcgen v0.14.10
│           └── wamn-component-virtualizer v0.1.0
│           └── wamn-conformance-tests v0.1.0
│       [dev-dependencies]
│       │   └── nkeys v0.4.5
│       │   └── wash-runtime v2.11.0
│       │   ├── ed25519-dalek v2.2.0
│       └── icu_provider v2.3.1
│       └── oci-spec v0.9.0
│       └── prost-build v0.13.5
│       └── signatory v0.27.1
│       └── sqlx v0.9.0
│       └── sqlx-macros-core v0.9.0
│       └── sqlx-postgres v0.9.0
│       └── tonic v0.14.6
│       └── walrus v0.26.5
│       └── wamn-control v0.1.0
│       └── wamn-edge v0.1.0
│       └── wamn-integration-tests v0.1.0
│       └── wamn-receiving-tests v0.1.0
│       └── wamn-runtime v0.1.0
│       └── wamn-schema-generator v0.1.0
│       └── wamn-workflow v0.1.0
│       └── wash-runtime v2.11.0
│       └── wasi-virt v0.2.0
│       └── wasmtime-wasi v48.0.7
│       └── yasna v0.6.0
│       ├── asn1-rs v0.7.2
│       ├── async-nats v0.49.1
│       ├── ed25519 v2.2.3
│       ├── ed25519-dalek v2.2.0
│       ├── icu_properties v2.3.0
│       ├── jsonwebtoken v10.4.0
│       ├── oci-wasm v0.6.0
│       ├── rcgen v0.14.10
│       ├── tracing-subscriber v0.3.23
│       ├── wamn-conformance-tests v0.1.0
│       ├── wamn-control v0.1.0
│       ├── wamn-edge v0.1.0
│       ├── wamn-engine v0.1.0
│       ├── wamn-execution-host v0.1.0
│       ├── wamn-gate-harness v0.1.0
│       ├── wamn-gates v0.1.0
│       ├── wamn-host v0.1.0
│       ├── wamn-identity v0.1.0
│       ├── wamn-identity-client v0.1.0
│       ├── wamn-integration-tests v0.1.0
│       ├── wamn-package-artifact v0.1.0
│       ├── wamn-runtime v0.1.0
│       ├── wamn-scenario-worker v0.1.0
│       ├── wamn-system-tests v0.1.0
│       ├── wash-runtime v2.11.0
│       ├── x509-parser v0.18.1
│   [build-dependencies]
│   [dev-dependencies]
│   │           [dev-dependencies]
│   │           │       └── wasmtime-wasi-http v48.0.7
│   │           │       ├── pg_walstream v0.9.0
│   │           │       ├── wash-runtime v2.11.0
│   │           │   [dev-dependencies]
│   │           │   │       [dev-dependencies]
│   │           │   │       └── wamn-gates v0.1.0
│   │           │   │       └── wamn-integration-tests v0.1.0
│   │           │   │       └── wamn-receiving-tests v0.1.0
│   │           │   │       └── wamn-wms-tests v0.1.0
│   │           │   │       ├── wamn-control v0.1.0
│   │           │   │       ├── wamn-ctl v0.1.0
│   │           │   │       ├── wamn-host v0.1.0
│   │           │   │       ├── wamn-receiving-tests v0.1.0
│   │           │   │   [dev-dependencies]
│   │           │   │   │   [dev-dependencies]
│   │           │   │   │   │   [dev-dependencies]
│   │           │   │   │   │   │   [dev-dependencies]
│   │           │   │   │   │   │   │   [dev-dependencies]
│   │           │   │   │   │   │   │   │   [dev-dependencies]
│   │           │   │   │   │   │   │   │   │   [dev-dependencies]
```

## 7. Copy and paste detection (jscpd)

Command: `pnpm dlx jscpd --config jscpd.json crates services tests test-support tools web apps deploy` with `minTokens` 70 and formats rust, typescript, tsx, javascript, sql, bash. `web/wfe` was not checked out. Full report in `raw/jscpd-report.json`.

Clones: 592. Duplicated lines: 8548 of 509024 (1.68 percent).

| Format | Clones | Duplicated lines | Lines |
| --- | ---: | ---: | ---: |

Directory pairs by duplicated lines (top 30):

| Lines | Directory A | Directory B |
| ---: | --- | --- |
| 1183 | `crates/platform/runtime` | `crates/platform/runtime` |
| 1033 | `crates/control/lib` | `crates/control/lib` |
| 422 | `crates/control/provision` | `crates/control/provision` |
| 360 | `crates/schema/generator` | `crates/schema/generator` |
| 358 | `tests/integration/src` | `tests/integration/src` |
| 299 | `crates/execution/host` | `crates/execution/host` |
| 265 | `tests/conformance/tests` | `tests/conformance/tests` |
| 254 | `apps/wamn_receiving/tests` | `apps/wamn_receiving/tests` |
| 201 | `apps/wamn_receiving/tests` | `apps/wamn_wms/tests` |
| 197 | `services/ctl/tests` | `services/ctl/tests` |
| 169 | `apps/wamn_wms/tests` | `apps/wamn_wms/tests` |
| 164 | `apps/platform/no-std` | `apps/platform/no-std` |
| 150 | `crates/identity/platform` | `crates/identity/platform` |
| 143 | `apps/wamn_receiving/tests` | `tests/integration/src` |
| 130 | `apps/wamn_receiving/tests` | `test-support/infrastructure/examples` |
| 125 | `apps/wamn_receiving/web` | `apps/wamn_receiving/web` |
| 118 | `crates/platform/engine` | `crates/platform/runtime` |
| 100 | `apps/platform_ui/test` | `apps/platform_ui/test` |
| 95 | `web/sync-bench/bench` | `web/sync-bench/bench` |
| 93 | `crates/execution/run-state` | `crates/execution/run-state` |
| 84 | `apps/netsuite/tests` | `apps/rest_connector/tests` |
| 81 | `crates/control/provision` | `crates/identity/project-state` |
| 78 | `tools/receiving-cluster-journey-run` | `tools/wms-cluster-journey-run` |
| 71 | `crates/schema/introspection` | `crates/schema/introspection` |
| 66 | `apps/edge_samples/data` | `apps/wamn_wms/data` |
| 65 | `crates/control/lib` | `crates/execution/workflow` |
| 65 | `crates/execution/workflow` | `crates/execution/workflow` |
| 58 | `crates/control/lib` | `crates/schema/generator` |
| 57 | `deploy/sql/postgres-init.sql` | `deploy/sql/run-state.sql` |
| 55 | `crates/execution/host` | `crates/platform/runtime` |

Files by duplicated lines (top 40):

| Lines | File |
| ---: | --- |
| 559 | `crates/platform/runtime/src/plugins/wamn_postgres/resources.rs` |
| 549 | `tests/integration/src/route_interface_live.rs` |
| 498 | `crates/platform/runtime/src/plugins/wamn_postgres/claims/tests.rs` |
| 414 | `crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs` |
| 281 | `apps/wamn_receiving/tests/route_authentication_live/local_business.rs` |
| 259 | `crates/control/lib/tests/workflow_manual_run_live.rs` |
| 242 | `crates/execution/host/tests/control_route_live.rs` |
| 199 | `crates/control/lib/tests/workflow_schedule_live.rs` |
| 195 | `crates/control/lib/src/package_upgrade/progress.rs` |
| 194 | `crates/execution/host/src/operation/native_policy/tests.rs` |
| 162 | `apps/wamn_wms/tests/local_feed.rs` |
| 160 | `apps/wamn_receiving/tests/receiving_data_access.rs` |
| 157 | `apps/platform/no-std/http-request/src/lib.rs` |
| 152 | `services/ctl/tests/platform_ui_live.rs` |
| 152 | `services/ctl/tests/receiving_ui_live.rs` |
| 149 | `apps/platform/no-std/http-page/src/lib.rs` |
| 131 | `crates/execution/run-state/tests/admission_live.rs` |
| 130 | `apps/wamn_receiving/tests/examples/exclusion_diagnostics.rs` |
| 130 | `test-support/infrastructure/examples/exclusion_diagnostics.rs` |
| 127 | `crates/execution/workflow/src/queue/automation_live.rs` |
| 124 | `crates/control/provision/tests/family_denial_matrix.rs` |
| 122 | `apps/wamn_wms/tests/local_outbox.rs` |
| 121 | `crates/control/provision/tests/system_reader_grants.rs` |
| 120 | `crates/schema/generator/tests/platform_generation.rs` |
| 119 | `deploy/sql/run-state.sql` |
| 118 | `apps/wamn_receiving/tests/route_authentication_live/cluster/delivery_case.rs` |
| 118 | `apps/wamn_wms/tests/cluster/delivery_case.rs` |
| 117 | `tests/conformance/tests/contract_diff.rs` |
| 114 | `tests/conformance/tests/cranelift_dev.rs` |
| 112 | `deploy/sql/postgres-init.sql` |
| 104 | `crates/execution/host/src/operation/native_policy/tests/authenticated.rs` |
| 104 | `tests/conformance/tests/repo_lint.rs` |
| 103 | `crates/identity/project-state/tests/authority.rs` |
| 103 | `crates/control/provision/tests/family_surface_grants.rs` |
| 102 | `tests/integration/src/write_log_live.rs` |
| 100 | `crates/control/lib/src/dev/activation.rs` |
| 97 | `crates/control/provision/tests/cdc.rs` |
| 95 | `apps/wamn_wms/component/src/inventory.rs` |
| 95 | `deploy/sql/app-schema.sql` |
| 89 | `tools/receiving-cluster-journey-run` |

## 8. Unused files, exports and dependencies in the pnpm workspace (knip)

Command: `pnpm install --frozen-lockfile` then `pnpm dlx knip --no-progress --reporter json` with `WAMN_DEV_ENV_DIR` and `WAMN_ROUTE_URL` set to placeholder values. Even with both variables set, knip cannot load `apps/wamn_receiving/web/vite.config.ts`, because that file reads `WAMN_DEV_ENV_DIR/dev.json` and an `issuer` field from it at import time; the Receiving web package is therefore analysed without its Vite entry. Rows under `web/wfe` belong to the submodule and are listed in `raw/knip.json` only.

Counts outside `web/wfe`: dependencies 1, devDependencies 5, exports 3, files 18.
Counts inside `web/wfe`: binaries 1, dependencies 1, enumMembers 3, exports 181, files 669, types 446.

| File | Issues |
| --- | --- |
| `crates/schema/generator/src/client_ts_sync.ts` | files 1 |
| `crates/schema/generator/tests/client_ts_behavior.config.mjs` | files 1 |
| `crates/schema/generator/tests/client_ts_behavior.test.ts` | files 1 |
| `crates/schema/generator/tests/client_ts_sync.config.mjs` | files 1 |
| `crates/schema/generator/tests/client_ts_sync.test.ts` | files 1 |
| `crates/control/lib/src/dev/web.mjs` | files 1 |
| `crates/schema/generator/tests/support/client_ts_consumer.ts` | files 1 |
| `apps/wamn_receiving/web/test/browser.mjs` | files 1 |
| `apps/wamn_receiving/web/test/offline-browser.mjs` | files 1 |
| `apps/wamn_receiving/web/test/sync-browser.mjs` | files 1 |
| `apps/platform_ui/test/browser.mjs` | files 1 |
| `web/sync-bench/bench/both.mjs` | files 1 |
| `web/sync-bench/bench/case2.mjs` | files 1 |
| `web/sync-bench/bench/follow.mjs` | files 1 |
| `web/sync-bench/bench/old-entry.ts` | files 1 |
| `web/sync-bench/bench/outbox.mjs` | files 1 |
| `web/sync-bench/bench/release.mjs` | files 1 |
| `web/sync-bench/bench/take-offline.mjs` | files 1 |
| `web/sync-bench/package.json` | dependencies 1, devDependencies 1 |
| `package.json` | devDependencies 1 |
| `apps/wamn_receiving/web/package.json` | devDependencies 1 |
| `apps/platform_ui/package.json` | devDependencies 1 |
| `web/shell/package.json` | devDependencies 1 |
| `apps/platform_ui/src/projects.tsx` | exports 2 |
| `web/shell/src/sync.tsx` | exports 1 |

## 9. Churn, last 90 days (git log)

Command: `git log --since=90.days --name-only --format= | sort | uniq -c | sort -rn | head -40`.

| Commits | File |
| ---: | --- |
| 1017 | `.beads/issues.jsonl` |
| 396 | `Cargo.lock` |
| 348 | `.beads/interactions.jsonl` |
| 234 | `docs/operations/gcp.md` |
| 208 | `docs/operations/build-and-test.md` |
| 208 | `docs/architecture/execution.md` |
| 191 | `docs/operations/running-tests.md` |
| 168 | `docs/architecture/data-access.md` |
| 160 | `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs` |
| 137 | `services/host/src/host.rs` |
| 128 | `crates/control/provision/src/sql.rs` |
| 124 | `tests/integration/src/route_authentication_live.rs` |
| 118 | `tools/receiving-cluster-journey-run` |
| 114 | `crates/schema/generator/tests/generation.rs` |
| 112 | `docs/operations/deployment.md` |
| 111 | `deploy/platform/release/Chart.yaml` |
| 110 | `crates/control/lib/src/release_chart.rs` |
| 110 | `Cargo.toml` |
| 107 | `crates/schema/control/src/run_plane.rs` |
| 106 | `crates/schema/generator/src/manifest.rs` |
| 104 | `crates/platform/runtime/src/plugins/connection_http.rs` |
| 98 | `crates/catalog/model/src/lib.rs` |
| 98 | `apps/wamn_wms/tests/environment.rs` |
| 96 | `docs/plan/gcp-deployment.md` |
| 93 | `tests/integration/src/trusted_http_route.rs` |
| 90 | `crates/control/lib/tests/environment_apply_kind.rs` |
| 89 | `crates/control/lib/src/publish_release.rs` |
| 86 | `services/ctl/src/main.rs` |
| 85 | `crates/schema/generator/src/lib.rs` |
| 83 | `docs/archive/build-and-test.md` |
| 82 | `services/ctl/tests/run_plane_live.rs` |
| 82 | `crates/execution/host/src/router_driver.rs` |
| 81 | `apps/wamn_receiving/tests/route_authentication_live/environment.rs` |
| 80 | `deploy/sql/run-state.sql` |
| 80 | `deploy/sql/catalog-schema.sql` |
| 79 | `docs/plan/README.md` |
| 79 | `crates/control/provision/tests/deploy_sql_authority.rs` |
| 79 | `architecture/state-owners.json` |
| 78 | `crates/schema/generator/src/generate/wit.rs` |
| 78 | `crates/schema/generator/src/generate.rs` |

## 10. Workspace crate graph (cargo metadata)

Command: `cargo metadata --no-deps --format-version 1`, reduced to in-workspace edges. Columns: normal dependencies, dev dependencies.

| Crate | Normal in-workspace deps | Dev in-workspace deps |
| --- | --- | --- |
| `wamn-host` | wamn-catalog,wamn-control-provision,wamn-control-registry,wamn-engine,wamn-execution-host,wamn-identity-client,wamn-package-artifact,wamn-platform-identity,wamn-runtime,wamn-session,wamn-workflow | wamn-catalog,wamn-test-postgres |
| `wamn-catalog` | wamn-component-policy,wamn-test-postgres | wamn-catalog,wamn-control-provision,wamn-test-postgres |
| `wamn-component-policy` | - | - |
| `wamn-test-postgres` | - | - |
| `wamn-control-provision` | wamn-control-registry,wamn-pg-core,wamn-project-state,wamn-run-state,wamn-schema-generator,wamn-test-postgres | wamn-catalog,wamn-fixture-package,wamn-schema-control,wamn-test-postgres |
| `wamn-control-registry` | - | - |
| `wamn-pg-core` | - | - |
| `wamn-project-state` | wamn-catalog,wamn-test-postgres | wamn-catalog,wamn-control-provision,wamn-project-state,wamn-runtime,wamn-test-postgres |
| `wamn-runtime` | wamn-catalog,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-engine,wamn-number-pattern,wamn-package-artifact,wamn-pg-core,wamn-platform-identity,wamn-project-state,wamn-run-state,wamn-session | wamn-catalog,wamn-control-provision,wamn-engine,wamn-package-artifact,wamn-project-state,wamn-run-state,wamn-runtime,wamn-schema-control,wamn-session,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-credential-store` | - | - |
| `wamn-engine` | wamn-catalog,wamn-component-policy,wamn-project-state,wamn-run-state,wamn-session | wamn-catalog |
| `wamn-run-state` | wamn-pg-core | wamn-catalog,wamn-control-provision,wamn-test-postgres |
| `wamn-session` | - | - |
| `wamn-number-pattern` | - | - |
| `wamn-package-artifact` | wamn-catalog | - |
| `wamn-platform-identity` | wamn-catalog,wamn-credential-store,wamn-project-state,wamn-session | wamn-control-provision,wamn-project-state,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-test-infrastructure` | wamn-authoring-model,wamn-catalog,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-platform-identity,wamn-schema-generator,wamn-test-postgres | wamn-control |
| `wamn-authoring-model` | wamn-catalog | - |
| `wamn-schema-generator` | wamn-catalog,wamn-number-pattern,wamn-schema-introspection | wamn-catalog,wamn-fixture-package,wamn-test-postgres |
| `wamn-schema-introspection` | - | wamn-fixture-package,wamn-test-postgres |
| `wamn-fixture-package` | - | - |
| `wamn-control` | wamn-authoring-model,wamn-catalog,wamn-component-composer,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-engine,wamn-identity-client,wamn-package-artifact,wamn-pg-core,wamn-platform-identity,wamn-project-state,wamn-run-state,wamn-runtime,wamn-schema-control,wamn-schema-generator,wamn-schema-introspection,wamn-session,wamn-test-postgres | wamn-catalog,wamn-control-provision,wamn-execution-host,wamn-fixture-package,wamn-gate-harness,wamn-package-artifact,wamn-project-state,wamn-runtime,wamn-test-infrastructure,wamn-test-postgres,wamn-workflow |
| `wamn-component-composer` | wamn-component-policy | - |
| `wamn-identity-client` | wamn-platform-identity,wamn-session | - |
| `wamn-schema-control` | wamn-catalog,wamn-pg-core,wamn-schema-generator | - |
| `wamn-execution-host` | wamn-catalog,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-engine,wamn-identity-client,wamn-platform-identity,wamn-project-state,wamn-run-state,wamn-runtime,wamn-schema-generator | wamn-catalog,wamn-component-composer,wamn-control,wamn-control-provision,wamn-fixture-package,wamn-runtime,wamn-schema-control,wamn-session,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-gate-harness` | wamn-catalog,wamn-control,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-engine,wamn-package-artifact,wamn-platform-identity,wamn-test-infrastructure | wamn-schema-control,wamn-test-postgres |
| `wamn-workflow` | wamn-catalog,wamn-engine,wamn-execution-host,wamn-project-state,wamn-router,wamn-run-state,wamn-runtime | wamn-catalog,wamn-control,wamn-control-provision,wamn-platform-identity,wamn-runtime,wamn-schema-control,wamn-schema-generator,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-router` | - | - |
| `wamn-edge` | wamn-catalog,wamn-control-registry,wamn-engine,wamn-run-state,wamn-run-state-sqlite,wamn-session | wamn-catalog,wamn-control,wamn-schema-generator |
| `wamn-run-state-sqlite` | wamn-run-state | wamn-run-state |
| `wamn-identity` | wamn-control-provision,wamn-platform-identity,wamn-session | wamn-catalog,wamn-control-registry,wamn-pg-core,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-ctl` | wamn-catalog,wamn-control,wamn-control-provision,wamn-control-registry,wamn-package-artifact,wamn-platform-identity,wamn-runtime,wamn-schema-control,wamn-schema-generator,wamn-workflow | wamn-authoring-model,wamn-catalog,wamn-control,wamn-control-provision,wamn-fixture-package,wamn-pg-core,wamn-project-state,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-scenario-worker` | wamn-authoring-model,wamn-catalog,wamn-control-provision,wamn-control-registry,wamn-package-artifact,wamn-platform-identity,wamn-runtime,wamn-schema-control | wamn-catalog,wamn-control,wamn-control-provision,wamn-session,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-cdc-reader` | wamn-control-provision,wamn-control-registry,wamn-pg-core | wamn-catalog,wamn-test-postgres |
| `wamn-bench` | - | - |
| `wamn-submodule-check` | - | - |
| `wamn-gates` | wamn-conformance-tests,wamn-integration-tests,wamn-runtime,wamn-system-tests | - |
| `wamn-conformance-tests` | wamn-component-policy,wamn-engine,wamn-gate-harness,wamn-run-state,wamn-runtime | wamn-execution-host,wamn-fixture-package,wamn-run-state,wamn-schema-generator,wamn-test-postgres |
| `wamn-integration-tests` | wamn-catalog,wamn-component-composer,wamn-component-policy,wamn-control,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-engine,wamn-execution-host,wamn-gate-harness,wamn-package-artifact,wamn-platform-identity,wamn-project-state,wamn-run-state,wamn-runtime,wamn-schema-control,wamn-schema-generator,wamn-session,wamn-test-infrastructure,wamn-workflow | wamn-catalog,wamn-client,wamn-control,wamn-run-state,wamn-schema-introspection,wamn-test-postgres |
| `wamn-client` | - | - |
| `wamn-system-tests` | wamn-run-state,wamn-runtime | wamn-run-state |
| `wamn-receiving-tests` | - | wamn-authoring-model,wamn-catalog,wamn-cdc-reader,wamn-client,wamn-component-composer,wamn-control,wamn-control-provision,wamn-control-registry,wamn-engine,wamn-gate-harness,wamn-integration-tests,wamn-platform-identity,wamn-project-state,wamn-run-state,wamn-runtime,wamn-schema-control,wamn-schema-generator,wamn-schema-introspection,wamn-test-infrastructure,wamn-test-postgres,wamn-workflow |
| `wamn-wms-tests` | - | wamn-authoring-model,wamn-catalog,wamn-cdc-reader,wamn-control,wamn-control-provision,wamn-control-registry,wamn-credential-store,wamn-gate-harness,wamn-integration-tests,wamn-platform-identity,wamn-project-state,wamn-runtime,wamn-schema-generator,wamn-test-infrastructure,wamn-test-postgres |
| `wamn-component-virtualizer` | - | - |

## 11. Tools not run

- `shellcheck`: not installed and installation needs sudo. A lane that needs it asks the owner.
- `cargo llvm-cov` and `cargo mutants`: deferred to the crates the lanes name, because a workspace run needs the live test infrastructure and hours of time.
- `cargo udeps`, `cargo deny`, `semgrep`, `kube-linter`, `trivy`: not installed. Install on request.
- `cargo tree --workspace -e features`: 7,167 lines, in `raw/` on request only.

