# Plan — extract the control library out of `services/ctl`

**Status:** rev 3 · 2026-09-15 · measured corrections and owner rulings
applied. Beads epic `wamn-k7v1` owns status. Serialized with the lanes that
edit `services/ctl`, at manifest and module-root edits.

## 1. Problem

`services/ctl` is 55,000 lines and holds four of the platform's six largest
files. It is a CLI binary that became the control plane: admission
(`push_component.rs`, 134 KB), release semantics (`publish_release.rs`,
121 KB), the dev loop (`dev/coordinator.rs` 126 KB, `watch.rs` 104 KB,
`config.rs` 99 KB), provisioning, reconcilers, retention. Four other packages,
all test or test-support, depend on its internals — **on the CLI package**,
which is the architectural defect: control logic is reachable only through a
binary's module tree.

## 2. Target

One control library, one CLI:

```
crates/control/lib        admission, release, provisioning, reconcile, package.
                          Does database, filesystem, and network work.
                          No CLI presentation, no process ownership.
services/ctl              argument parsing, output formatting, exit codes.
                          Each verb: parse → one library call → print.
services/ctl/src/dev/     the loop. This wave repoints its callers and moves
                          six helpers out (section 4).
```

Dependency direction, fixed:

```
CLI / dev loop / other callers → control library → existing lower libraries
```

Never a control-library dependency back into `services/ctl` to make a move
compile. The library is **one Cargo package**: `crates/control/lib`, package
`wamn-control`. Growing `crates/control/provision` creates a Cargo cycle,
because `wamn-runtime` depends on provision and the moved code uses
`wamn_runtime`. No new crate per area; areas are modules.

"Separated" means: ordinary function inputs, returned results and errors,
validation kept inside the shared function so a direct caller cannot bypass
it, and formatting outside. Not pure planners, adapter traits, or injection.
Library functions keep `anyhow::Result` in this wave, so test downcasts stay
unchanged.

## 3. Method

Each extraction is **two commits**:

1. **Move** — `git mv` into the library, mechanical import and visibility
   changes, coupled modules moved together. A `pub use` shim at the old path
   keeps outside callers compiling. The shim resolves an outside import path;
   it does not resolve the moved module's own dependencies on `ctl` siblings,
   which is why coupled modules move together and why order is by dependency.
2. **Separate** — the minimum to make the API a library API: argument types,
   printing, environment handling, and exits taken out; results returned;
   validation retained. Callers repointed and **the shim removed in this
   commit**. Every caller in the four dependents changes in that commit. The
   commit lands when the `wamn-ctl` binary parse tests and `verb_surface` pass.

`crates/control/lib` is one directory deeper than `services/ctl`. Every
relative include path and `CARGO_MANIFEST_DIR` join in moved code gains one
`../`, as a mechanical path update.

Order is **by dependency, not size**: start with the smallest closed module
set, then outward. Serialize every extraction with the lanes that edit
`services/ctl`; serialize `Cargo.toml` and module-root edits. The lane owns
the files of the current extraction and their manifest and module
declarations, not the two directory trees.

## 4. Sequence (measured with production edges at 1f5f34b65)

| step | extraction | why |
|---|---|---|
| 0 | empty `wamn-control` crate; `services/ctl/tests/support` moved into `wamn-test-infrastructure` | manifest edits land once, before any move |
| 1 | `apply_package/` tree, `sql_params`, `reconcile_package_data_access` | parent code builds `ApplyPackageError` from `pub(super)` fields, so the reconcilers cannot move without their parent; harness caller |
| 2 | `provision_project_env/`, `env_policies`, `pat_client`, `enable_cdc_project_env`, `event_streams`, `ident`; `claim_environment_instance` from `dev/coordinator.rs`; `ProvisionedRoute`, `read_json`, `secret_value` from `dev/environment.rs` | provisioning closure; harness callers; moved tests call the claim helper |
| 3 | `push_component` | `publish_release.rs:432` and `promote.rs:836` depend on it |
| 4 | `authored_base_digests` and `render_declaration_document` from `dev/coordinator.rs`, then `publish_release/`, `promote/`, `author_wiring`, `verification_policy`, `reconcile_run_plane` | `verification_policy` and `reconcile_run_plane` call each other; moved tests call the declaration helpers |

The dev loop's callers are repointed as each extraction separates, not as a
final step. The six helpers named in steps 2 and 4 are the only dev-loop code
that moves.

The remaining control verbs (`identity_issuer`, `project_env_membership`,
`provision`, `provision_org`, `bind_connection`, `push_release_manifest`,
`print_release_env`, `reconcile_replica_identity`,
`terminalize_effect_uncertain`) follow with the same method under
`wamn-df1z`.

## 5. Proof

- **`test-support/harness` drops its `wamn-ctl` dependency** and depends on
  `wamn-control`. A changed import with the `wamn-ctl` edge kept does not
  show the dependency direction. No CI wrapper.
- Implementation tests move with their code; CLI parsing, output, and
  exit-code tests stay beside the CLI. Mechanical import and fixture-path
  updates allowed; **assertions and behavior unchanged**.
- Focused tests on each extraction; the relevant live tests on the
  integrated result.

## 6. Exit

- Every extracted operation is callable and tested from the library without
  `services/ctl`.
- No shim remains for a separated API.
- `test-support/harness` has no `wamn-ctl` dependency.
- `services/ctl` is a thin **operational** CLI over the library for the
  extracted operations, with the dev loop still attached. The remaining
  control verbs are `wamn-df1z`. The loop's own separation is later work; the
  3,000-line target is guidance, not the criterion.
