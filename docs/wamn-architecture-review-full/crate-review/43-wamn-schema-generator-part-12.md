# 43 wamn-schema-generator, part 12

Scope: manifest authoring (KCL `wamn.k` to `wamn.json`) and the derived manifest members. Nothing built or run.

## Files read

- `src/authoring.rs` (non-test part, in full).
- `src/generate.rs::derive_manifest` (the end, where derived members are written).
- `src/build.rs` and `src/materialize.rs` at the `derive_manifest` calls.

## Callers read

- Every caller of `compile_manifest`, `write_compiled_manifest`, `compile_schema_file`, `compile_environment`, `package_manifest_path`, `is_package_root`, `manifest_package_root` (grep).
- Every `wamn.json` in the repository outside build output (find).
- KCL Rust API page (kcl-lang.io).

## One purpose per file

- `authoring.rs`: compile `wamn.k` (and the environment, cluster and project documents) with the `kcl` binary against schema modules embedded in this crate, and splice the derived members into the compiled manifest.

## Findings

**1. Member order and indentation are kept, then thrown away.**

Fact:

- `OrderedJson` (an order-keeping JSON type, about 130 lines), `two_space_indent`, `with_derived_relations` and `with_derived_feed_sources` keep the schema module's member order and a 2-space layout when the derived `relations` and `feed_source` are spliced in.
- Both callers of `derive_manifest` (`build.rs`, `materialize.rs`) turn its output straight into canonical JSON (RFC 8785, sorted keys). `write_compiled_manifest` writes canonical bytes too. No written file keeps the order.
- `relations` and `feed_source` are typed fields of `CustomOperationDeclaration`.
- "The author states no derived member" is checked twice: `authoring.rs::refuse_derived_members` (both members) and `generate.rs::derive_manifest` (`feed_source`).

Options:

- A: derive into the typed manifest fields and serialize canonically once. Check "no derived member stated" once, on the typed authored manifest. Delete `OrderedJson`, `two_space_indent`, `with_derived_relations`, `with_derived_feed_sources`, `without_derived_members`, `operation_members` and the second check.

**2. `write_compiled_manifest` has no product caller.**

Fact: it is exported from `lib.rs` and called only by `tests/manifest_authoring.rs`. It exists to compare and keep a file that two writers write (the compile and the build).

Options:

- A: delete it with `without_derived_statement_facts`, its export and its test. The build is the one writer of `wamn.json`.

**3. Two manifest formats, the second for test fixtures only.**

Fact: every package under `apps/` authors `wamn.k` (a lint refuses a root `wamn.json` there). A hand-written `wamn.json` package root exists only as test fixtures under `crates/*/tests` (10 found; `catalog/model/src/host_route/wamn.json` is the host route contract, not a package). The branch is `HAND_WRITTEN_MANIFEST`, `is_authored`, and the two-way logic of `package_manifest_path`, `manifest_package_root`, `is_package_root`.

Options:

- A: one format. Fixtures author `wamn.k`. Delete the hand-written branch and its helpers.

**4. KCL runs as an external binary with output repair.**

Fact:

- `kcl_binary` takes `$WAMN_KCL` or runs `tools/install-kcl`. Each compile writes the schema module to a temporary directory, runs `kcl run --format json`, re-indents 4 spaces to 2 (`two_space_indent`), strips terminal colors from errors (`without_terminal_colors`), and rewrites the copied file path in errors back to the original.
- KCL has an in-process Rust API (`kcl-lang`, `API::exec_program`). Its documentation installs it from git only, not crates.io, and shows a YAML result; JSON output and external package paths are not documented there (fact from the page). The build runs `--locked --offline`.

Options:

- A: keep the binary. The re-indent goes with finding 1; the rest stays.
- B: run KCL in process with `kcl-lang`. Deletes the binary lookup, temporary modules, color stripping and path rewrite, but adds a git dependency of the KCL runtime. To check first: JSON result, external packages, offline build, size.
- Recommend A now; B only if the checks pass.

## Tests deleted, changed or moved

- Finding 1: tests of `with_derived_relations` and order keeping (`authoring.rs` tests) go; derived-member tests read typed fields.
- Finding 2: `tests/manifest_authoring.rs` tests of `write_compiled_manifest` go.
- Finding 3: the fixtures under `crates/control/*/tests` and `crates/control/package/tests/fixtures` move to `wamn.k`.

## Not read

- `authoring.rs` tests (names only); the KCL schema modules under `kcl/`.

## Rulings

- Finding 4: A, keep the binary (owner, 2026-11-11, after the tab 44 evaluation; embedding noted as possible). Owner (2026-11-11): evaluate B before deciding. Open check: in-process KCL (`kcl-lang`) gives JSON output, takes the embedded schema modules as external packages, builds `--locked --offline`, and its size and build time are acceptable.
- Findings 1–3: A (owner, 2026-11-11). Owner: deploy/gcp/guard stays as is (a helper for the GCP deployment, not part of the platform).
- Owner direction (2026-11-11), build tooling: miscellaneous project tasks use `cargo xtask` and `just`, preferred over Python and shell. Fact (inventory): `tools/` holds 19 bash and 3 Python scripts (2,747 lines; `test-changes` alone is 961 lines of bash); `tools/bench` adds 6 Python and 5 shell files; `deploy/gcp/guard` is Python (a deployed function, not a task script). Rust tool crates already exist (`tools/bench`, `tools/submodule-check`). There is no `justfile` and no `xtask`. Product code runs two of these scripts: the generator calls `tools/guest-rustflags` (tab 40) and `tools/install-kcl` (finding 4). The scripts get their own review tab after the crates.
