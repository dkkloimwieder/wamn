# Contract dedup

This report covers epic `wamn-ycyvb`, which takes audit rows A2-3, A2-12 and A2-15 of [code quality](code-quality.md).
The branch is `worktree-contracts-r3`, on main `f85c11eb9`.
Every pin below refers to the last code commit, `aa5dc116d`.
The branch changes 151 files, with 744 insertions and 3,223 deletions.

## Issues

| Issue | Audit reference | Commit | Result |
| --- | --- | --- | --- |
| `.1` Host contracts author each shape once | A2-3 | `0cefdd50a` | 104 contract files in 33 groups of equal bytes become 33 shapes. `shapes.json` in each host route set assigns each shape to its operations by name. The generator writes the per-operation files as build output. |
| `.2` One raw-byte digest spelling | A2-12 | `c65405ca5` | `wamn_execution_contract::bytes_sha256` writes `sha256:<hex>` beside `canonical_json_sha256`. Five copies go: publication, platform build, generator, scenario-worker and the catalog `edge_bundle::file_digest`. |
| `.3` A missing attachments file means no attachments | A2-15 | `aa5dc116d` | `operation_reference::read_attachments_document` owns the rule for every reader. The four empty `{}` files go. |

## Measurements

Issue 1 measured main again before the change: 33 groups, 104 member files, 71 redundant files and 2,947 redundant lines.
The audit counted 71 files and 3,020 lines at `8c831349d`.
The connection and integration routes changed between the two commits.

## Acceptance

Each issue kept its output bytes.

1. Issue 1: all 228 resolved contract files and both generated clients are byte-identical before and after.
2. Issue 2: the native tests pin the empty, binary (0 to 255) and text vectors to fixed SHA-256 values, and `canonical_json_sha256` equals `bytes_sha256` of the canonical bytes. All 12 package digests equal main.
3. Issue 3: after a `wamn build` of `rest_connector`, `netsuite`, `modbus_reader` and `platform_fixture_overlay`, every output file is byte-identical to the copy saved before the change. Only the `inputs` of `build.json` differ, because the deleted file was an input.

## Decisions

The owner gave these answers.

1. Equal shapes get explicit assignments, never inference. Byte-stable outputs are the acceptance everywhere.
2. Issue 3, option (a), recorded verbatim on `wamn-ycyvb.3`. The refusal "publish-release requires at least one package-owned --attachments document" and its pin `apps/rest_consumer_fixture/tests/publication.rs` go. Malformed JSON still refuses.

Root made these decisions inside the brief.

1. `bytes_sha256` compiles only for native targets. Every guest links the contract crate, and a new compiled item changes each guest's bytes and the `client_acme_receiving` base pin. No guest takes a raw digest.
2. `services/edge` and `crates/control/provision` take `wamn-execution-contract` as a new dependency, because they called the catalog helper that issue 2 deletes.
3. A missing attachments file reads as an empty map without reading the package owners, because an empty map names no reference.
4. The control publisher passes every package root to the reader. A package without an authored file still publishes the entries that generation wrote. `rest_consumer_fixture` and `wamn_integration` have neither file, so their releases do not change.

## Tests

Each issue ran fmt, Clippy, the unit tests of the changed crates and `tools/test-changes --base origin/main run`.
The run times and results are on each issue, and the logs are in `/var/tmp/wamn-contracts-log`.

1. Issue 1, run 2: the failures equal those of the same run on main `b7eb0d844`. `control_client_live` passed 2 of 2 and `host_route_live` passed 7 of 7.
2. Issue 2, run 3: the baseline failures, plus two failures caused by the test environment. Both pass on a rerun, and each has a bead below.
3. Issue 3, run 4 on `341b7a5f3`, the same patch before the rebase: exactly the 13 baseline failures. The `netsuite` and `rest_connector` publication tests pass.

The 13 baseline failures are filed as `wamn-vv3zm`, `wamn-s76tz`, `wamn-p5msj`, `wamn-i37lr`, `wamn-6zx1c`, `wamn-efs58`, `wamn-fiahj` and `wamn-fszfy`.

## Follow-up

1. `wamn-azv06`: `deploy/sql/record-history.sql` creates `wamn_db_owner` with a test-then-create race, and run 3 hit it.
2. `wamn-j1md8`: `wamn build` calls a package current after its virtualized components are gone, and run 3 hit it on a fresh target.
