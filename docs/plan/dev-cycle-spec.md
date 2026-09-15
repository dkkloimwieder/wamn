# Short development cycles

**Status:** rev 2 · 2026-09-15 · external review applied · measured at
`ab9859e7`.
Scope: the time between saving a change and knowing whether it works. Two
removals of repeated work and one consolidation. No caching
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
2. **No selection.** Every change runs the app's whole database group.
3. **Seventy-five crates.** About forty are guests and must exist; among the
   native crates, some merely divide one implementation into several
   packages. A shared-contract change rebuilds most of the tree.

## 3. Rule

**A cycle repeats only the work the change invalidated.** Nothing here caches
a result; it stops recomputing inputs that didn't move.

## 4. Changes

### 4.1 SQLx offline by default

- `SQLX_OFFLINE=true` in `.cargo/config.toml` `[env]`. Ordinary `cargo build`
  and `cargo test` compile from committed `tests/.sqlx/` with no database; an
  ambient `DATABASE_URL` cannot restore online checking.
- **Refresh trigger — the effective inputs, not file extensions.** Generate
  already byte-compares its emitted SQL; `prepare` runs when Generate emitted
  different SQL for the application, when its effective schema changed (a
  base migration under an overlay counts), or when the verifier version
  moved. No new digest.
- Command and location, stated: `cargo sqlx prepare` run in the application's
  `tests/` directory against the verification database, writing
  `tests/.sqlx/`, for the targets and features the verifier builds.
- CI runs `cargo sqlx prepare --check` once per qualification against a
  fresh database and refuses stale metadata. Local cycles never pay it.
- Tests: an ordinary build with no database succeeds offline; preparation and
  the CI check reach the verification database and cover the required
  targets.

### 4.2 One test-server entrypoint, isolated databases

4.2 landed under `wamn-eg9d`; `docs/operations/running-tests.md` owns test-database isolation.

### 4.3 Selection by ownership, then by Cargo

- A small wrapper (it has selection logic; it is not an alias) runs: the
  owning application's tests for a change under `apps/<name>/`, plus
  `cargo test -p` for the Rust crates whose files changed and their
  dependents from `cargo metadata`. Generator tests run when the generator
  changes, and for an application edit only where an existing test consumes
  that application. No impact registry.
- Fallbacks: saved, untracked, and deleted inputs count; a change to a shared
  manifest, the lockfile, or an unowned input runs the full existing command.
  A targeted green run never advances a global "everything tested" state.
- The full run stays one command for CI and pre-merge.

### 4.4 Crate consolidation

Rule: **merge crates that merely divide one cohesive implementation, provided
the merge does not worsen dependency weight, target compatibility, or
ordinary edit/build time.** Native/Wasm and feature-isolation boundaries stay.
One consumer does not make a boundary useless; two do not make it useful.

Candidates for review — each gets a decision, none is a destination:

| candidate | question |
|---|---|
| `tools/probes/*` (4) | any live consumer of their executable tests? if none, delete |
| `crates/identity/project-state` + `platform` | one implementation? |
| `crates/control/registry` + `provision` | one implementation, or the control library from the `ctl` extraction? |
| `crates/execution/run-state`, `scheduler`, `router` | cohesive, or does merging concentrate dependencies? |
| `crates/platform/pg-core`, `component-policy`, `component-virtualizer`, `runtime` | lightweight primitives stay separate if merging grows `runtime`'s unit |
| `crates/scenarios/model`, `authoring/model`, `catalog/model` | unrelated contracts — merge only if they are one model |
| `crates/client/terminal`, `tui`, `core` | UI machinery into `core` only if edit/build time doesn't worsen |

Each merge is one mechanical commit with a shim, independently reviewable,
coordinated with the `ctl` extraction (manifests, imports, test ownership
overlap). Measure **elapsed rebuild time** of a shared-contract change before
and after; not a crate count.

## 5. What must not change

- Every test that passed before passes after, same assertions.
- CI still prepares SQL against a fresh database and runs the full suite.
- A selected test that cannot get its prerequisite fails; nothing passes by
  skipping.
- No result caching, no dependency registry, no new framework.

## 6. Proof

1. After 4.1: the walkthrough's Rust-only edit shows 0 s preparation; a SQL
   edit shows preparation and nothing else regressed.
2. After 4.3: a Receiving SQL edit runs Receiving's tests and the generator's
   only where a test consumes Receiving; a lockfile change runs the full
   command.
3. After 4.4: rebuild seconds for a shared-contract change, before and after.
4. The walkthrough rerun: cold setup and warm iteration reported separately,
   executed-test counts preserved.

## 7. Order

4.1 first. 4.3 second. 4.4 in small independent commits alongside,
coordinated with the `ctl` extraction. Each lands with its measurement.
