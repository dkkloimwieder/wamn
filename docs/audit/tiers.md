# Test tiers, selection and preparation

This report covers epic `wamn-kov6v`, which is audit group 3 part B (see [README](README.md)).
The branch is `worktree-tiers`, on main `229f7377e`.
Every pin below refers to that branch head, `9d4523796`.

## Issues

| Issue | Audit reference | Commits | Result |
| --- | --- | --- | --- |
| `.1` One selection policy | G3-2 | `17000c6bc` | `tests/selection.toml` replaces `tools/test-changes-suites.json` and the shell branches. An unknown path selects every applicable suite. A documentation-only change selects nothing. |
| `.2` Five tiers | G3-1, G3-6 | `3893d800c` | Every executable target has one row and one tier: unit, contract, live-postgres, live-nats or kind. `--tier kind` replaces `--cluster`. Kind targets hold only ignored kind cases. |
| `.3` Ignore hygiene | G3-4, G2-8 | `cc773720c` | Every ignore reason has the form `requires: bin:<program>, env:<VARIABLE>`. One parser reads it, and each ignored case makes sure of it before setup. The SQLite child helper is the `intent-store-child` binary. |
| `.4` Isolated contract compilation | G3-5 | `c1944bb36` | No root workspace target reads `apps/target` at compile time. One compatibility test, `wamn-client` `test:generated_schema`, reads the generated schema at run time. |
| `.5` One preparation path | F2-10 | `192e3204b` | `tools/test-changes` prepares every input of the selected suites from `[[preparation]]` rows. A set variable is used as is. `WAMN_POSTGRES_BIN_DIR` overrides the PostgreSQL binary directory. |

Commit `9d4523796` (`wamn-t66s7`) is on the branch beside the issues. It gives `wasmtime-wasi` its reason in `ENGINE_ACCEPTED`, which `e27052b6e` left out. Without it, repo-lint fails on main.

## Decisions

Root took these decisions inside the brief.

1. A library target that holds pure and PostgreSQL cases is not split. It takes the strongest tier that it needs. `mixed-targets.tsv` lists 17 such targets with their case counts.
2. A run without `--tier` keeps one cargo command per workspace. Only an explicit `--tier` filters targets per package.
3. The Receiving and WMS cluster cases moved to a `test:cluster` target in each package, with `required-features = ["cluster"]`. The fixtures stay unchanged. They are shared with `include!` and `#[path]`, and D3-6 (part 2) consolidates them.
4. No business assertion was deleted. No Receiving or WMS case had its full assertions in a lower tier.
5. Both bead-only ignores recorded real defects, so both were repaired. `wamn-ennwh` lost a frozen argument count. `wamn-bjs11` records `--files-addr` as a deployed-only flag.
6. A temporary exclusion names only its bead, in the `exclusions` list of `tests/selection.toml`.

## Identity comparison

The inventory before the moves held 3,580 lines, and after the moves it held 3,588 lines. `ids.accounted.md` accounts for 53 moved pairs and 8 added lines. No case was lost.

## Deferred to `wamn-kov6v.6`

These six ignores keep their old reasons, because their files were hard stops for this epic. They are listed as exclusions under `wamn-kov6v.6`.

- `crates/control/lib/src/platform/broker.rs:570` and `:689`, `requires: WAMN_NATIVE_C_NATS_BIN`
- `crates/control/lib/src/platform/edge.rs:359`, `requires: helm`
- `crates/platform/runtime/src/auth_cache.rs:399`, `:429` and `:525`, `requires: WAMN_NATIVE_C_NATS_BIN`

## Defects on main that the new coverage found

The close run executed cases that main ignored or never selected. Each failure below also occurs on main `229f7377e`.

| Bead | Priority | Case | Failure |
| --- | --- | --- | --- |
| `wamn-191yv` | P1 | `family_denial_matrix` identity-reader family | The identity-reader family reaches `identity.org_memberships` `SELECT`. |
| `wamn-s76tz` | P2 | `control_generation_live`, `management_live` (4 cases) | A role is assigned before the principal is an active org member. |
| `wamn-p5msj` | P2 | `identity_issuer_live` | The current issuer does not hold the approved expanded ACL. |
| `wamn-6zx1c` | P2 | `jetstream` suite | The case needs a TLS broker, and the suite starts a plain one. |
| `wamn-efs58` | P2 | Receiving `command_histories` | The fixture revokes the fixed `admin` role, which `f4cbec305` refuses. |
| `wamn-fiahj` | P2 | WMS `local_release` reload | The second shell never loads after a refused outbox entry. |
| `wamn-rdlfn` | P3 | `build_live` grouped summary | `record-history.sql` fails once in a full root run. It passes alone. |

The native host case now fails with `missing Secret wamn-system/wamn-oauth` from `crates/control/lib/src/platform/oauth.rs`. That symptom is a comment on the open bead `wamn-i37lr`.

## Test results

- Each issue: fmt, workspace clippy with the ops and cluster features, and the unit and contract tests of changed packages passed. The conformance targets `test_changes` and `test_tiers`, and `tools/test-revision-test`, passed.
- Issue 2: the touched live targets passed after a rebuild of stale build records. `install_sequence_live` passed after the rebase onto `229f7377e`.
- Issue 4: `cargo check --workspace --all-targets` with the ops features passed from a copy of the tree without `apps/target`.
- Issue 5: `tools/test-changes --suite receiving-business,wms-local` ran 14 business cases, and 12 passed. The two failures are `wamn-efs58` and `wamn-fiahj`.
- Epic close: `tools/test-revision 192e3204b` failed only on the defects above and on the repo-lint gap that `9d4523796` fixes. The apps, no-std, prekind, release chart render, web and guest suites passed.

The logs are in `/var/tmp/wamn-test-revision.y0kwRD8N`, with `epic-run.log`, `business-proof.log` and the identity, ignore, mixed-target and generated-read inventories. The runs did not use kind, and they built no image.

## Open items

- `wamn-kov6v.6`: the six deferred ignores above.
- A clippy run over the whole apps workspace stops at `connection-http-standard` with a duplicate `panic_impl` lang item. This epic did not change that crate.
- `wamn-bcplj` goes to `integration-schema-e1` after this epic lands.

## Follow-up: selection and the deferred ignores

Epic `wamn-91uih` (owner brief 2026-11-02) closes two items from this report. The branch is `worktree-select`, on main `489244477`.

| Issue | Commit | Result |
| --- | --- | --- |
| `wamn-hpr8h` Selection gap | `0c9b7b93a` | The mapping did not miss. The new case `a_grant_sql_change_selects_the_family_denial_matrix` keeps the selection true. |
| `wamn-kov6v.6` Deferred ignores | `c8ab95d24` | The broker.rs and edge.rs ignores use the fixed form. `tests/selection.toml` has no exclusion left. |

### Cause of the selection gap

`wamn-ms1sn.2` (`f4cbec305`) landed while `test:family_denial_matrix` failed. Root ran the selection runner of that commit over that commit. A change under `deploy/sql/` selected a full root workspace run, and the failing case was a plain `#[test]` in that run. On main, `--tier live-postgres` selects `-p wamn-control-provision --test family_denial_matrix` for the same change.

The close run of `wamn-ms1sn.2` used a hand-picked target list ("provision live", "ten packages") and not `tools/test-changes`. Its logs are gone. The new conformance case reads the real targets row, so it fails if the row disappears or changes tier.

### Test results

- `test_changes` passed 32 of 32, and `test_tiers` passed 5 of 5 with zero exclusions.
- The three broker.rs and edge.rs cases passed with `--ignored`, with `WAMN_NATIVE_C_NATS_BIN` and `helm` set.

The logs are in `/var/tmp/wamn-docs`: `select-conformance.log` and `kov6v6-test.log`. The runs built no image and did not use kind.

### Selection by policy row

`wamn-s182s` (`fb5c01ae0`) makes `tools/test-changes` read a diff of `tests/selection.toml` by row. A row is one `[[suites]]` table or one element of `targets` or `exclusions`. The tool compares the parsed rows of the base and the working file, so a format change of a row selects nothing.

- An added or changed suite row selects that suite and `-p wamn-conformance-tests --test test_changes --test test_tiers`. The suite still passes the tier filter.
- A changed targets or exclusions element selects only the two selector tests.
- A removed suite row selects nothing for that suite. The tool fails if a tracked file outside `docs/history/` and `docs/audit/` still names the suite.
- A change to the shared header or to `tools/test-changes` selects everything, as before.

A change of code files and rows selects the union of both rules. A dry run on a one-row edit of `packages-alone` selected only that suite and the selector tests. Five new cases cover the rules. `test_changes` passed 37 of 37 and `test_tiers` passed 5 of 5, and format and clippy passed. The logs are `rowdiff-test.log` and `rowdiff-dry.log` in `/var/tmp/wamn-docs`. The runs built no image and did not use kind.
