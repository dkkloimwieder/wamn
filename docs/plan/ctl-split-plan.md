# Plan — extract the control library out of `services/ctl`

The [architecture overview](../architecture/overview.md#control-and-publication) describes the current `wamn-control` library and its dependency direction.
This page keeps only the unbuilt split work. Beads records its status.

## 1. Problem

`services/ctl` still holds control verbs beside the CLI and the dev loop.
`wamn-integration-tests`, `wamn-receiving-tests`, and `wamn-wms-tests` reach those verbs through the `wamn-ctl` package.
That control logic is reachable only through the module tree of a binary package.

## 2. Target

One control library, one CLI:

```
crates/control/lib        admission, release, provisioning, reconcile, package, delivery.
                          Does database, filesystem, and network work.
                          Runs the processes its work needs (git, docker, kubectl, cargo).
                          No CLI presentation and no CLI lifecycle.
services/ctl              argument parsing, output formatting, exit codes, signals, its stdout.
                          Each verb: parse → one library call → print.
services/ctl/src/dev/     the loop. It stays here until its later separation.
```

The library owns the processes that its work needs.
The CLI owns its own lifecycle: exit codes, signals, and its stdout.
A package that only holds process calls, with no decision of its own, is not a boundary.

The dependency direction is fixed:

```
CLI / dev loop / other callers → control library → existing lower libraries
```

The control library never depends on `services/ctl` to make a move compile.
The library is one Cargo package: `crates/control/lib`, package `wamn-control`.
Each area is a module in that package, not a new crate.
`crates/control/provision` does not grow, because `wamn-runtime` depends on it and moved code uses `wamn_runtime`.

A separated API takes ordinary function inputs and returns results and errors.
It keeps validation inside the shared function, so a direct caller cannot bypass it.
Formatting stays outside the library.
A separated API uses no pure planners, adapter traits, or injection.
Moved functions keep `anyhow::Result`, so test downcasts stay unchanged.

## 3. Method

Each extraction is two commits:

1. Move: `git mv` the modules into the library, with mechanical import and visibility changes.
   Coupled modules move together.
   A `pub use` shim at the old path keeps outside callers compiling.
2. Separate: take argument types, printing, environment handling, and exits out of the library API.
   The API returns results and keeps validation.
   This commit repoints every caller and removes the shim.

The shim resolves an outside import path only.
It does not resolve the imports of a moved module from its `ctl` siblings.
For this reason, coupled modules move together, and order follows dependency.

The separate commit changes every caller in the dependent packages and in the `#[path]`-shared test files.
After the `wamn-ctl` binary parse tests and `services/ctl/tests/verb_surface.rs` pass, the commit lands.

`crates/control/lib` is one directory deeper than `services/ctl`.
Each relative include path and `CARGO_MANIFEST_DIR` join in moved code gains one `../`.

Order follows dependency, not size.
Start with the smallest closed module set, then move outward.
Serialize each extraction with the lanes that edit `services/ctl`, and serialize `Cargo.toml` and module-root edits.
The lane owns the files of the current extraction and their manifest and module declarations.
It does not own the two directory trees.

## 4. Remaining work

`wamn-df1z` extracts the remaining control code with the same method, in one lane:

- The control verbs `bind_connection`, `identity_issuer`, `project_env_membership`, `provision_org`, `push_release_manifest`, `print_release_env`, `reconcile_replica_identity`, and `terminalize_effect_uncertain`.
- `services/ctl/src/delivery/`, which holds deployment, publication, and qualification. CI calls this code, so it must not stay in a CLI module.
- The `ops` feature verbs `copy_project_env`, `dump_project_env`, `restore_project_env`, `prune_record_history`, `prune_run_history`, `event_advisories`, and `ops_schema`.

`delivery/` runs `git`, `docker`, `kubectl`, and `cargo` processes.
Those process calls move into `wamn-control` with the delivery code, as section 2 allows.

The separation of the dev loop in `services/ctl/src/dev/` is later work.

## 5. Tests

- Implementation tests move with their code.
  CLI parsing, output, and exit-code tests stay beside the CLI.
- A move allows mechanical import and fixture-path updates.
  Assertions and behavior stay unchanged.
- Each extraction runs its focused tests.
  The integrated result runs the relevant live tests.

## 6. Exit

- Every extracted operation is callable and tested from `wamn-control` without `services/ctl`.
- No shim remains for a separated API.
- `services/ctl` is a thin operational CLI over `wamn-control` for the extracted operations, with the dev loop still attached.
- The 3,000-line target is guidance, not the criterion.
