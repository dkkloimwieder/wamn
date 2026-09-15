section 6 item 3 completed by `wamn-llst.5`
# Short development cycles

**Status:** rev 2 · 2026-09-15 · external review applied · measured at
`ab9859e7`.
Scope: the time between saving a change and knowing whether it works. One
removal of repeated work and one consolidation. No caching
framework, no attribute system, no machine-wide service, no crate quota.

## 1. Measured

Adding one nullable column to Receiving (`field-change-walkthrough.md`):

| step | seconds |
|---|---:|
| generation | 15 |
| SQLx preparation | 167 |
| one scalar test | 33 |
| the app's database tests | 218 |
| **total** | **433** |

A Rust-only edit that touches no SQL pays the same 167 s of preparation and
the same 218 s of serial database tests.

## 2. Causes, in cost order

1. **SQLx preparation on every cycle.** `.sqlx` offline metadata is committed
   and documented; it is not the default, so `cargo sqlx prepare` recompiles
   every `query!` against a live database on every run.
2. **Seventy-five crates.** About forty are guests and must exist; among the
   native crates, some merely divide one implementation into several
   packages. A shared-contract change rebuilds most of the tree.

## 3. Rule

**A cycle repeats only the work the change invalidated.** Nothing here caches
a result; it stops recomputing inputs that didn't move.

## 4. Changes

### 4.1 SQLx offline by default

4.1 landed under `wamn-0lgk`; `docs/operations/running-tests.md` owns SQLx offline compilation, the development-loop preparation rule, and the metadata check.

### 4.2 One test-server entrypoint, isolated databases

4.2 landed under `wamn-eg9d`; `docs/operations/running-tests.md` owns test-database isolation.

### 4.3 Selection by ownership, then by Cargo

4.3 landed under `wamn-eg9d`; `docs/operations/running-tests.md` documents `tools/test-changes`.

### 4.4 Crate consolidation

4.4 closed under `wamn-zh3p`; `tools/probes` and `wamn-scenario-model` were deleted, and the other six candidates stay separate crates.
No merge landed, so section 6 item 2 records no measurement.

## 5. What must not change

- Every test that passed before passes after, same assertions.
- CI still prepares SQL against a fresh database and runs the full suite.
- A selected test that cannot get its prerequisite fails; nothing passes by
  skipping.
- No result caching, no dependency registry, no new framework.

## 6. Proof

1. After 4.1: the walkthrough's Rust-only edit shows 0 s preparation; a SQL
   edit shows preparation and nothing else regressed.
2. After 4.4: rebuild seconds for a shared-contract change, before and after.
3. The walkthrough rerun: cold setup and warm iteration reported separately,
   executed-test counts preserved.

## 7. Order

4.1 first. 4.4 in small independent commits alongside,
coordinated with the `ctl` extraction. Each lands with its measurement.
