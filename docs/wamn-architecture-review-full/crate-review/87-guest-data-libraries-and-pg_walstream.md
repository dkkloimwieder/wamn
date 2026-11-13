# 87 guest data libraries and pg\_walstream

Scope at `epic/dep7` (b94833fd): `apps/platform/data/access` (`wamn-data-access`, 1,086 lines), `apps/platform/data/record-history` (`wamn-record-history`, 918 with tests), `vendor/pg-walstream` (60,210 Rust lines), and a look at `blob-put`, `jsonata`, `wamn_integration`. These are the Rust packages no earlier tab reviewed whole. Nothing built or run.

## Earlier work checked first

- Tab 04 row 2: one definition of each scalar type in `wamn-scalar`; `wamn-data-access/src/scalar.rs` as a middle layer goes. Each application's `data/error.rs` goes to one definition.
- Tab 56 finding 4 A: one cursor envelope (encode, decode, canonical check, version) in one crate; app and platform cursors keep their own payloads.
- Tab 79: `wamn-event-reg` stays as the one owner of trigger predicates.
- Tab 80 2 B: palette components (`blob-put`, `jsonata` and others) are built once with the platform.
- Tab 83: `pg_walstream` stays for the replication session. Tab 78: TLS on every PostgreSQL connection.
- Greenfield: no compatibility paths.

## Files read

- `wamn-data-access`: `lib.rs`, `feed.rs`, `feed/id.rs` (`encode`, `decode`), `cursor.rs` and `cursor/id.rs` (heads, public API), `page.rs` and `scalar.rs` (public API).
- `wamn-record-history/src/lib.rs`: module comment, constants, `HistoryRow`, `RowState`, `retain_columns` doc and head.
- `deploy/sql/record-history.sql`: `row_image`, `timestamptz_image`, the log function head.
- `vendor/pg-walstream`: `WAMN-PATCH.md`, `Cargo.toml`, directory listing; `src/types.rs` and `src/stream.rs` by grep (`Keepalive`, `current_lsn`).
- Upstream `pg_walstream` 0.10.0 `src/types.rs` on docs.rs (checked 2026-11-12).
- Heads of `blob-put`, `jsonata`, `wamn_integration/component/src/lib.rs`.

## Callers read

- `wamn_data_access`: `crates/schema/generator/src/generate/data.rs` and `authored_feed.rs` (generated `use` lines); application `data/scalar.rs` files; `apps/wamn_receiving/component/src/data/read.rs`.
- `wamn_record_history`: every use in 8 crates (constants and name helpers), `state_at` and `retain_columns` users.
- `pg_walstream`: root `Cargo.toml`, `services/cdc-reader/src/lib.rs` (`EventType::Keepalive`).

## Findings

**1. `pg_walstream` is copied whole into the repo to carry one patch.**

Fact:

- `vendor/pg-walstream` is the released 0.9.0 crate tree: `src` (48,800 lines) plus upstream integration tests, benches, examples, load tests, `.github`, `.claude`, `AGENTS.md` and its own `Cargo.lock` (about 11,400 more lines).
- The one change is a typed `EventType::Keepalive { wal_end }` (`WAMN-PATCH.md`, fork commit a0df8cee). `services/cdc-reader` matches on it to confirm an idle slot position.
- Upstream 0.10.0 still has no keepalive variant (docs.rs, checked). The patch is still needed.
- The root workspace excludes the directory and depends on it by path.
- Unknown: whether the fork `dkkloimwieder/pg-walstream` is reachable by the image build. `docs/operations/gcp.md` names the fork; `WAMN-PATCH.md` says the copy avoids publishing one.

Options:

- A: Depend on the fork by git at the patch commit; `Cargo.lock` pins it. `vendor/` goes.
- B: Keep a copy, trimmed to what Cargo builds (`src/`, `Cargo.toml`, `LICENSE`, `WAMN-PATCH.md`). Upstream tests, benches, examples and repo files go.
- C: Drop the patch. The reader bounds each `next_event` with a timeout and reads `current_lsn()` when idle. Inference, not checked: `next_event` must be safe to cancel, and `current_lsn()` (which returns `state.last_received_lsn`) must advance on keepalives.

Recommend A if the fork can be public; else B.

**2. The change feed cursor is a second cursor encoding.**

Fact:

- The keyset cursor (`cursor/id.rs`) is base64 of canonical JSON with `VERSION = 1`.
- The feed cursor (`feed/id.rs`) is base64 of `<model>:<transaction>[:<id>]` text, with no version. `decode` tries the id as a JSON string, then as parsed JSON, then re-encodes to refuse other spellings.
- Both are in the same crate, written by the same generated data functions.

Options:

- A: The feed cursor uses the one envelope of tab 56 4 A (versioned canonical JSON) with its own payload `{model, transaction, after}`. The colon format and its two-step id decode go.

Recommend A.

**3. The history fold has no product caller.**

Fact:

- `wamn-record-history::state_at` with `HistoryRow`, `RowState`, `RowImage`, `FoldError` (about 175 lines) folds history rows back to a row's state at a position.
- Callers are tests only: `tests/fold.rs` (352), `crates/control/provision/tests/deploy_sql_authority.rs`, `apps/wamn_receiving/tests/receiving_data_access.rs`. No guest, host, control or client code calls it (grep, including `web/`).
- The rest of the crate (history names, trigger names, retention role, column list) is used by 8 crates.

Options:

- A: Delete the fold and its tests. If an as-of read becomes a product feature, it is written then.

Recommend A.

**4. A guest rewrites history JSON that SQL already writes.**

Fact:

- `retain_columns` (with `members` and `canonical_timestamptz`, about 165 lines) scans JSONB text by hand in the guest. It keeps only the declared columns and rewrites timestamptz values to one spelling. One caller: `apps/wamn_receiving/component/src/data/read.rs`.
- Its comment keeps two spellings: "a package entry keeps the `to_jsonb` spelling with an offset" and "an entry written before `wamn-c8a5v.2` keeps the canonical one".
- `deploy/sql/record-history.sql`: the log function writes both images through `wamn_history.row_image`, which already spells every finite timestamptz in the canonical form. `timestamptz_image` exists so "a history read that names its columns builds the current row image".

Options:

- A: The generated history read projects the declared columns in SQL. Images already carry one spelling, so the guest rewrite and the old-spelling path go (greenfield). `retain_columns`, `members`, `canonical_timestamptz` go. Inference: a `jsonb` projection keeps numeric scale, as `row_image` does today.

Recommend A.

## Follows from earlier rulings

- Tab 04 row 2: `wamn-data-access/src/scalar.rs` and the applications' `data/scalar.rs` layers.
- Tab 04: `wamn_integration/component/src/lib.rs::statement_error` is one more per-package copy of the SQL error mapping (inference: same mapping as `data/error.rs`).
- Tab 80 2 B: `blob-put` and `jsonata` are built once with the platform.
- Tab 72 3 A: bead ids in comments (`wamn-c8a5v.2`, `wamn-362o.42`, `wamn-59z6`).

## Fine as is

- `wamn-record-history` names (history suffix, trigger names, retention role, history columns): one owner for 8 crates.
- `wamn-data-access` keyset cursor, `Page`, `Range`, `Invalid`: one copy, used by generated code.
- `blob-put`: caller-supplied key, so a redelivery overwrites. `jsonata`: pinned `jsonata-core`, no imports.

## Tests deleted, changed or moved

- 1 A: the upstream tests, benches and load tests in `vendor/pg-walstream`.
- 2 A: feed cursor tests in `feed/id.rs` change to the envelope.
- 3 A: `record-history/tests/fold.rs`; the `state_at` cases in `deploy_sql_authority.rs` and `receiving_data_access.rs`.
- 4 A: `retain_columns` unit tests; its uses in `receiving_data_access.rs`.

## Not read

- `pg_walstream` `src` beyond the grep above.
- Bodies of `members`, `state_at`, `cursor/id.rs`; `wamn_integration` beyond `lib.rs`.
- The generated history read SQL.

## Rulings

2026-11-12: findings 1–4 A (1: git dependency on the fork at the patch commit, `vendor/` goes; the owner confirmed the fork can be public, so B is not needed. 2: feed cursor in the one envelope. 3: `state_at` fold deleted. 4: history read projects columns in SQL; `retain_columns` deleted).
