# 44 Build tooling and embedded KCL (investigation)

Scope: the evaluation the owner asked for on tab 43 finding 4 (KCL in process), and how `cargo xtask` and `just` would take over the repository's scripts. Nothing built or run.

## Sources read

- KCL source, `kcl-lang/kcl` (main, version 0.13.2): `crates/api/Cargo.toml`, `crates/api/build.rs`, `crates/api/spec.proto` (`ExecProgramArgs`, `ExecProgramResult`, `ExternalPkg`), `crates/runner/Cargo.toml`, `crates/runner/src/runner.rs` (`ExecProgramArgs`, `exec_program`).
- KCL Rust API page (kcl-lang.io); article "Embedding KCL in Rust" (dev.to).
- crates.io search for `kcl-`.
- wamn: `tools/install-kcl`, `tools/guest-rustflags`, `.cargo/config.toml`, `Dockerfile` (tool use), every `tools/` reference in Rust code (count), the `tools/` inventory (tab 43).

## Embedded KCL: results

Fact:

- The KCL Rust crates are not on crates.io. The crates.io names `kcl-api` and `kcl-lib` belong to another project (KittyCAD's KCL). The dependency would be git, pinned to a tag.
- `kcl-api` (`API::exec_program`) depends on the KCL language server, `tokio` with all features, a JSON-RPC server, and a vendored `protoc` build step.
- `kcl-runner` (`exec_program(session, &ExecProgramArgs)`) is the lower layer, without those. It still brings the KCL compiler crates, `cc` and `libloading`.
- `ExecProgramArgs` covers what wamn uses today: source as strings (`k_code_list`), external packages by name and path (`external_pkgs`), `disable_none`, `sort_keys`. The API result carries `json_result` and `err_message`; an invalid program reports through `err_message`, not an error result (article).
- An external package is a directory path, so the embedded schema module still needs a directory on disk, unless it is passed as source (inference).
- `tools/install-kcl` pins the CLI at 0.13.0 and exits on anything but Linux x86\_64. So today `wamn build` cannot compile a manifest on macOS or on ARM unless `WAMN_KCL` names a binary.
- Build size and compile time: not measured (no builds in this review).

Options for tab 43 finding 4:

- A: keep the binary (Linux x86\_64 only, download script, color stripping, error path rewrite).
- B: `kcl-runner` as a git dependency pinned to the matching tag. Deletes `kcl_binary`, `tools/install-kcl`, `WAMN_KCL`, `without_terminal_colors`, the error path rewrite, and the platform limit. Costs: a git dependency (offline builds need it in the Cargo cache or vendored), more compile time.
- Recommend B, after one measured build of the dependency (needs the owner's leave, since this review runs no builds).

## cargo xtask and just: how they would fit

Fact (the patterns):

- `cargo xtask`: a workspace member binary named `xtask`, run through a Cargo alias (`[alias] xtask = "run -p xtask --"` in `.cargo/config.toml`). Plain Rust, nothing to install.
- `just`: one binary that runs recipes from a root `justfile`. Recipes are short command lines; logic lives in `cargo xtask`.
- wamn has neither today. `.cargo/config.toml` exists (linker, jobs, `SQLX_OFFLINE`).

What goes where (proposal from the inventory; each script is reviewed in its own tab after the crates):

- Product code must not run a script. Two do today: `tools/guest-rustflags` (13 references, including the `Dockerfile`) becomes a Rust function of the build; `tools/install-kcl` goes with KCL option B.
- Logic-heavy scripts become `cargo xtask` subcommands: `test-changes` (961 lines of bash), `require-test-result`, `test-revision`, `repo-lint`, `repo-lint-dependencies` and `check-packages-alone` (Python), `wasmcloud-release-check`, `contract-diff`, `delivery-owned`.
- Short operator commands become `just` recipes that call `cargo xtask` or `kind`/`helm`: `kind-cluster-delete`, `kind-kept-delete`, `local-registry`, `lane-start`, `http-reuse-run`, `cargo-cranelift`.
- Pinned tool installers (`install-helm`, `install-tofu`, `install-wash`): a `cargo xtask install` with the digests in one table. Downloads use `hyper` (owner rule: no `reqwest`).
- `tools/bench` is already a Rust crate; its 6 Python and 5 shell files fold into it. `tools/submodule-check` is a Rust crate and becomes an xtask subcommand.
- Out of scope: `deploy/gcp/guard` (owner, 2026-11-11).

## Rulings

- KCL (tab 43 finding 4): A, keep the `kcl` binary (owner, 2026-11-11). Note kept: embedding with `kcl-runner` (option B above) stays possible; it would remove the Linux x86\_64 limit and the binary handling, and needs a measured build first.
- Tooling: move to `cargo xtask` and `just` (owner, 2026-11-11). The placement above is the direction; each script is reviewed in its own tab after the crates. `tools/install-kcl` stays with the binary and becomes an `xtask install` entry with the other pinned tools.
