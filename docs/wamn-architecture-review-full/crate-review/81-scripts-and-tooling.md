# 81 scripts and tooling

Scope: `tools/` at `epic/dep7` (b94833fd): 22 scripts, `tools/bench`, `tools/submodule-check`, `tools/netsuite`, and the conformance tests that run them. Nothing built or run.

## Earlier work checked first

- Tab 44: tooling moves to `cargo xtask` and `just`; scripts reviewed after the crates. Tab 43: keep the KCL binary.
- Tabs 66, 67: GKE pools and the OpenTofu project go to xtask or just; Cloud Build and spend tooling go. Tab 62 9 A: one component build path.
- Boundary ruling 2026-11-10: the virtualizer goes (`tools/component-virtualization.json` with it).
- CLAUDE.md test rule 3: do not test a test helper.

## Files read

- First lines and header comments of every file in `tools/`; `tools/bench` listing.

## Callers read

- Repository grep for `tools/<name>` in Rust, TOML, YAML, JSON, `Dockerfile`, shell and Markdown (excluding `docs/history`).

## Inventory

- Bash: `cargo-cranelift`, `contract-diff`, `delivery-owned`, `guest-rustflags`, `http-reuse-run`, `install-helm`, `install-kcl`, `install-tofu`, `install-wash`, `kind-cluster-delete`, `kind-kept-delete`, `lane-start`, `local-registry`, `repo-lint` (212), `require-test-result`, `test-changes` (962), `test-revision`, `test-revision-test` (352), `wasmcloud-release-check` (173).
- Python: `check-packages-alone`, `dev-collector`, `repo-lint-dependencies`; `tools/bench` has four Python and three shell scripts.
- Conformance tests that run a script: `test_changes.rs` (2,010), `registry_image_archive.rs` (588), `contract_diff.rs` (315), `repo_lint.rs` (292), `wasmcloud_release_check.rs` (232), `packages_alone.rs` (154), `cranelift_dev.rs` (149), `delivery_native_build.rs` (51).

## Findings

**1. Project tasks are scripts in two languages, and their tests test tooling.**

Fact: the inventory above. Tests of tooling: about 3,800 lines of conformance tests plus `test-revision-test` (352), which tests `test-revision`. `test-changes` selects the tests a branch runs (CLAUDE.md names it); its own tests are 2,010 lines.

Options:

- A: one `xtask` crate holds the commands that stay (guest rustflags, test selection, lint, package checks, release check, collector render, kind cleanup); a `justfile` names them. The scripts go. Tests of the tooling itself go (test rule 3); a command that matters is checked by being used.

Recommend A (tab 44 direction).

**2. Four install scripts pin upstream CLIs by hand.**

Fact: `install-helm`, `install-kcl`, `install-tofu`, `install-wash` each download one pinned version and check its SHA-256 (for example the OpenTofu `SHA256SUMS` value). No code calls them; docs do.

Options:

- A: the four become xtask install commands.
- B: one tool version file read by an existing version manager (for example `mise`), which pins the versions and checks checksums. The scripts go.

Recommend B: existing tooling over hand-written downloaders. With tab 49 and tab 66, `install-tofu` serves only the moved OpenTofu command.

**3. Tools with no caller.**

Fact (grep): `kind-kept-delete` has no reference anywhere; `http-reuse-run`, `local-registry`, `test-revision` and `test-revision-test` are named only in docs, not by code, tests or other tools.

Options:

- A: delete them, and their doc mentions.

Recommend A.

## Follows from earlier rulings

- Boundary ruling 2026-11-10: `tools/component-virtualization.json` goes.
- Tab 67: `delivery-owned` (native artifact builds) follows the one build path; Cloud Build helpers go.
- Tab 72 3 A: script comments that cite beads (for example `guest-rustflags`, "wamn-rie7").

## Fine as is

- `tools/submodule-check` (a Rust crate already).
- `tools/bench`: benchmark runner and result scripts; they move into xtask with finding 1 or stay as data tooling, decided with it.

## Tests deleted, changed or moved

- Finding 1 A: `tests/conformance/tests/test_changes.rs`, `contract_diff.rs`, `repo_lint.rs`, `wasmcloud_release_check.rs`, `packages_alone.rs`, `cranelift_dev.rs`, `delivery_native_build.rs`, `registry_image_archive.rs` (the parts that run scripts), `tools/test-revision-test`.

## Not read

- Script bodies; `tools/bench` bodies; `tools/netsuite/live.sh`.

## Rulings

- 2026-11-12: 1 A (one xtask crate plus a justfile; scripts and tooling tests go), 2 B (one tool version file for an existing version manager), 3 A (tools with no caller deleted).
