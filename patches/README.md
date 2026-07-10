# Carried patches for wasmCloud (`wash-runtime`)

wamn builds against `wash-runtime` from the wasmCloud monorepo, pinned to the
exact rev in the root `Cargo.toml` (`workspace.dependencies.wash-runtime.rev`
— the **single source of truth** for the pin), with the diffs in this
directory applied on top. Upstream is `publish = false`, so a git/path
dependency is the only way to consume it.

## Current patches

| Patch | What / why |
|---|---|
| `0001-wash-runtime-store-epoch-deadline.patch` | Functional. `new_store_from_templates` (the crate's single production store-creation site) gives every store an epoch deadline: the active component's `wamn.epoch-deadline-ticks` config (plumbed from the WorkloadDeployment CRD's `localResources.config`), else the `WAMN_EPOCH_DEADLINE_TICKS` env var, else effectively unbounded (`u64::MAX / 2` — `u64::MAX` would wrap in wasmtime's `current_epoch + delta`). Without it, stores keep wasmtime's default deadline of 0 and trap on the first epoch tick, so epoch interruption (S2 chaos gate, hard cancellation) is unusable. Deliberately kept to one call site to minimize rebase drift. |
| `0002-workspace-lints-warn-not-deny.patch` | Build mechanics only. Relaxes the monorepo's `-D warnings`: as a git dep cargo builds the crate with `--cap-lints allow`, but as a `[patch]` path dep it gets the full lint set, and wamn's feature subset legitimately leaves some upstream code unused. |

Everything else epoch-related lives **unpatched** in wamn-host:
`Config::epoch_interruption(true)` layers in via `EngineBuilder::with_config`,
and `spawn_epoch_ticker` drives the public `Engine::increment_epoch()`
(`crates/wamn-host/src/engine.rs`; `host --epoch-tick-ms`, 0 = off).

## How the patches are applied

`scripts/vendor-wasmcloud.sh` produces `vendor/wasmcloud` (gitignored): it
reads the rev from `Cargo.toml`, shallow-fetches the monorepo at that rev,
force-resets the checkout, and `git apply`s each `patches/*.patch` in order —
idempotent, always "pinned rev + patches", never patch-on-patch. The
`[patch."https://github.com/wasmCloud/wasmCloud.git"]` section in the root
`Cargo.toml` then redirects `wash-runtime` from the git dep to that path.
The path points *inside a real monorepo checkout* so its `workspace = true`
dependencies resolve there (and `vendor` is in our workspace `exclude` for
the same reason). The Dockerfile runs the same script in its builder stage,
so local and image builds compile identical sources.

Run the script once before the first build (and after any rev bump or patch
change); `cargo build` fails with a missing-path error until it has run.

## Updating the wasmCloud pin

1. Change `rev = "<sha>"` on `wash-runtime` in the root `Cargo.toml`. That is
   the only place the pin lives; the script reads it from there.
2. Rerun `scripts/vendor-wasmcloud.sh`.
   - **Applies cleanly** (likely — the functional patch is one call site):
     `cargo build -p wamn-host`, then run `wamn-host bench`. Phase 4 is the
     regression test that the patch is present *and functional*: without the
     deadline, stores trap on the first tick, so a lost patch fails loudly.
   - **`git apply` fails**: re-do the edit by hand in `vendor/wasmcloud` at
     the new rev (or `git apply --3way` and resolve), then regenerate with
     `git -C vendor/wasmcloud diff > patches/0001-...patch` and commit the
     refreshed patch together with the rev bump. If upstream refactored store
     creation out of `new_store_from_templates`
     (`crates/wash-runtime/src/engine/linked_call.rs`), find the new
     production `Store::new` site and move the deadline call there.
3. Commit the rev bump and any regenerated patches in the same commit.

**Exit condition:** if upstream ships native epoch-deadline support, delete
`0001` (the wamn-host ticker/config side stays as-is). `0002` lives for as
long as we consume the crate as a path dep.
