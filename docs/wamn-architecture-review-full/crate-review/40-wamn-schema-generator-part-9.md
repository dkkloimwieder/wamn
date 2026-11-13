# 40 wamn-schema-generator, part 9

Scope: step 7 of `wamn build`, which compiles a package's guest components, virtualizes and composes them, and records them in `build.json`. Nothing built or run.

## Files read

- `src/components.rs` (non-test part, in full).

## Callers read

- `crates/platform/component-composer/src/{lib.rs (function list), main.rs}`; `crates/platform/component-virtualizer/src/main.rs` (head).
- The `Cargo.toml` of the generator, `control/dev`, `control/release`, `control/package` (composer dependency).
- `wamn_catalog::wiring::WiringDocument`; `wamn_catalog::manifest::ComponentDeclaration` (names).
- Upstream `wash` `cli/component_build.rs` (head).

## One purpose per file

- `components.rs`: build each guest with its own `cargo build`, check it (no local path, matches its pin), virtualize it, compose an overlay with its bases, copy it to the build output, and record it.

## Findings

**1. The build runs its own libraries through `cargo run`.**

Fact:

- `virtualize` and `compose` start `cargo run --locked --offline -p wamn-component-virtualizer` and `-p wamn-component-composer` (`tool`), then move a partial file into place (`run_tool`).
- The composer is a library: `wamn_component_composer::compose(overlay, bases) -> bytes`. The generator already depends on it (its `Cargo.toml`) and calls the library only in a test. `control/dev` calls the library directly (`compose_control_entry`).
- The virtualizer is a 162-line command around `wasi_virt::WasiVirt`, with no library.

Options:

- A: the build calls `compose` and the virtualizer function on bytes, in process. The virtualizer becomes a library function (in the composer crate, or a library crate of its own). Delete `tool`, `run_tool`, the partial files, the virtualizer binary, and the composer binary if nothing else runs it (to check in the composer tab).

**2. Cargo output and `Cargo.toml` are read by hand.**

Fact: `compile_guest` reads cargo's JSON messages as untyped `Value` (`reason`, `target.crate_types`, `filenames`, `manifest_path`). `crate_package` finds the package name by taking the first line of `Cargo.toml` that starts with `name = "` (the workspace already has the `toml` crate).

Options:

- A: read the messages with the `cargo_metadata` crate's typed `Message` (its `package_id` names the package). Delete `crate_package` and the `Value` reads.

**3. Wirings and palette declarations are read untyped.**

Fact: `wiring_components` reads each wiring as `Value` (`nodes`, `component`, `operation-dependency`); `wamn_catalog::wiring::WiringDocument` already types these documents. `palette_declaration` scans `platform/*/*/declaration.json.in` and reads each as `Value` to find a component by name.

Options:

- A: parse wirings with `WiringDocument` and declarations with the typed component declaration. Delete the `Value` reads.

**4. One step, three writes and a second job.**

Fact: `build_components` writes `build.json` before compiling, `compile_components` writes it again, then `build_components` writes it a third time. Between the writes it copies TypeScript clients to their consumers (`materialize_client_consumers`), which is not part of compiling components.

Options:

- A: write `build.json` once, at the end. The client copy moves to the materialize step that owns clients.

## Checked, no change

- Upstream `wash build` runs a configured build command and fetches WIT dependencies. It does not virtualize, compose, or check pins or local paths, so it does not replace this step (fact, from `cli/component_build.rs`).
- One `cargo build` per guest is deliberate (Cargo merges features within one call, which can change bytes). Kept.

## Tests deleted, changed or moved

- Finding 1: the build tests that need the tool binaries run the library calls instead; the composer library test at `components.rs` stays.
- Findings 2–4: tests that write fixture `Cargo.toml` lines or untyped wiring files change to typed documents.

## Not read

- `components.rs` tests (names only); the composer library bodies (composer tab).
- `tools/guest-rustflags`, `tools/component-virtualization.json`.

## Rulings

- Findings 1–4: A (owner, 2026-11-11).
