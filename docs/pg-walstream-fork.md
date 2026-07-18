# Consuming `pg_walstream` from the wamn fork

wamn builds against `pg_walstream` from **our fork** —
https://github.com/dkkloimwieder/pg-walstream (upstream:
https://github.com/isdaniel/pg-walstream, BSD-3, single-author) — consumed as
a cargo git dependency, the wash-runtime discipline applied to a second
library (D19 v3 §4: the CDC reader's decode layer is load-bearing enough to
vendor, pin, and never casually `cargo update`).

- **Branch naming:** `wamn/X.Y.Z` = upstream release `vX.Y.Z` + the carried
  wamn commits on top. Current: `wamn/0.8.0` = upstream v0.8.0 (`784e644`,
  verified byte-identical to the crates.io `pg_walstream-0.8.0` sources the
  S-CDC-1 spike vetted) + the failover-slot-syntax commit.
- **The pin:** `workspace.dependencies.pg_walstream.rev` in the root
  `Cargo.toml` — the **single source of truth**. Pin a **rev** (immutable),
  never a branch name (branches move); the branch's existence on the fork is
  what keeps the SHA fetchable. Consumers (`poc/cdc1`, the Phase-1 reader)
  take it via `workspace = true`. Sanity on every bump:
  `grep -c '^name = "pg_walstream"$' Cargo.lock` must be 1.
- **Features:** default (`rustls-tls`); `sslmode=disable` works for the
  non-TLS in-cluster paths.

## Carried commits (the ledger)

The fork carries **correctness commits only** — things upstream should own.
wamn features never land there. Each commit records its **exit condition**:
the upstream change that makes it deletable. Upstreaming is deferred cleanup
(the arch-rework directive), not active work.

| Commit (on `wamn/0.8.0`) | What / why | Exit condition |
|---|---|---|
| `0b007cd` "fix(slot): emit FAILOVER via the parenthesized CREATE_REPLICATION_SLOT option list" | Functional (S-CDC-1 finding F1, wamn-l5i9.2). `src/sql_builder.rs` `build_create_slot_sql` appended `FAILOVER` to the legacy space-separated keyword form, which PostgreSQL rejects with a 42601 — the option exists only in the PG17+ parenthesized grammar. When `slot_options.failover` is set the builder now emits every option through the option list (e.g. `LOGICAL "pgoutput" (SNAPSHOT 'nothing', FAILOVER)`); non-failover callers keep the byte-identical legacy form, so nothing else changes. Unit pins updated in `sql_builder.rs` + both connection wrappers; proven by live A/B on PostgreSQL 18 (crates.io 0.8.0 → 42601, fork → `failover=t` in `pg_replication_slots`). | upstream emits the parenthesized option list for failover slots — delete this commit |

## Sync runbook

**Triggers:** upstream release with a fix/feature we need (evaluate, don't
chase — single-author repo, releases are irregular); a decode-correctness bug
we hit (report upstream, carry the fix); `cargo audit` flag on the dependency
line.

**Steps per sync** (upstream releases X.Y.Z at rev `NEWREV`):

```bash
# in the fork clone (/home/kaalin/dev/pg-walstream; add remote 'upstream' = isdaniel/pg-walstream)
git fetch upstream
git log --oneline <OLD_BASE>..NEWREV -- src/sql_builder.rs   # what moved under our commit?
git checkout -b wamn/X.Y.Z NEWREV
git cherry-pick 0b007cd    # re-check the exit condition first: if upstream fixed it, DROP
cargo test --lib           # the unit pins (incl the parenthesized-FAILOVER strings)
git push -u origin wamn/X.Y.Z
```

Then in wamn: bump the root `Cargo.toml` rev, `cargo update -p pg_walstream`,
rebuild, and run the **upgrade gate subset** — the fork-load-bearing behaviors,
not the whole spike:

- `wamn-cdc1 setup` against a throwaway PG 17+ (`postgres:18 -c
  wal_level=logical`) — the crate-native failover slot must land with
  `failover=t` (the carried commit present *and functional*; on an
  unpatched base this fails loudly with a 42601).
- `wamn-cdc1 message` + `wamn-cdc1 stream --rows 100000` — decode regression
  (Message events + streamed-transaction constant memory).
- When infra warrants (a decode-layer or protocol change): the full S-CDC-1
  switchover drill per `docs/build-and-test.md [EVT-S-CDC-1]`.

**Rollback:** repoint the rev at the previous `wamn/*` branch tip. Never
delete a branch wamn ever pinned.

**Escalation threshold (standing):** same as wash-runtime — past ~4–5 carried
commits, or repeat conflicts on the same commit, engage upstream or accept
maintainer status explicitly.

## Sync log

| Date | Base | Carried | Gates |
|---|---|---|---|
| 2026-07-18 | crates.io `=0.8.0` → fork `wamn/0.8.0` @ `0b007cd` (base = upstream v0.8.0 `784e644`, byte-identical to the crates.io sources) | + failover-slot-syntax commit (F1, wamn-l5i9.8) | fork `cargo test --lib` 1247/1247; live A/B on postgres:18 (crates.io → 42601, fork → `failover=t`); `wamn-cdc1 message` streaming regression PASS; lockfile carries exactly one git-sourced pg_walstream |
