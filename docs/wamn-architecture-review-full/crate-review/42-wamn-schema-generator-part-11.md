# 42 wamn-schema-generator, part 11

Scope: the build record (`build.json`) and how the build decides that output is current. Nothing built or run.

## Files read

- `src/build_record.rs` (non-test part, in full).
- The generator's `build.rs` (in full); `materialize.rs::{GENERATOR_ID, TOOLCHAIN_ID}`.

## Callers read

- Count of `check_current` / `read_current` call sites outside this file: 37 (grep).
- `crates/control/environment/src/platform_build/inputs.rs` (`declared_reads`, by name and its tests).

## One purpose per file

- `build_record.rs`: write and read `build.json` (package, input digests, dependencies, generator identity, output digests) and tell whether a build still matches its source.

## Findings

**1. The build tracks Rust and WIT inputs by hand.**

Fact:

- The input digest hashes every file of the package tree, every file of three hand-listed platform trees (`PLATFORM_INPUTS`), the workspace `Cargo.lock`, and the WIT paths of guests.
- To find those WIT paths, `insert_guest_wit` reads every `.rs` file of the workspace, parses each that mentions `wit_bindgen` with `syn`, and pulls the `path:` argument out of each `wit_bindgen::generate!` call (`WitBindings`, `binding_paths`).
- Cargo already records every file a compile reads in its dep-info (`.d`) files. Inference: wit-bindgen makes each WIT file a compile input (it emits an `include_bytes!` of it so Cargo rebuilds on change), so the `.d` file of a guest lists its WIT files.
- 37 call sites check that a build is current, and each recomputes the whole digest.

Options:

- A: take the Rust and WIT inputs of each guest from its cargo dep-info file after the build step; hash the package's own non-Rust source as today. Delete `insert_guest_wit`, `WitBindings`, `binding_paths`, `PLATFORM_INPUTS` and the `syn` dependency of this file.
- B: drop component staleness from `build.json`: Cargo decides guest rebuilds, and the record keeps only generation inputs and output digests. Consumers then cannot tell a component built from older source.
- Recommend A (keeps the protection, uses Cargo's own record).

**2. The generator's identity is written by hand, and its input list three times.**

Fact:

- `GENERATOR_ID = "wamn-schema-generator/0.1.13"` (the crate version exists as `CARGO_PKG_VERSION`), `TOOLCHAIN_ID = "rust-1.99.0"` (the source is `rust-toolchain.toml`), `SQLX_VERSION = "0.9.0"` (the source is `Cargo.lock`).
- The generator's `build.rs` lists its fingerprint inputs in `INPUTS` and again as literal `cargo:rerun-if-changed` lines. Its comment says the lines must be literal because `platform_build::inputs::declared_reads` parses build scripts' text.

Options:

- A: the build script derives the identity: crate version from `CARGO_PKG_VERSION`, toolchain from the compiler it runs under, SQLx version from the lock. One input list: the script emits `rerun-if-changed` from `INPUTS`. Delete the three literals. The text parse in `platform_build` goes with the control tab (carried).

**3. A compatibility path and a self-check.**

Fact:

- `EXCLUDED_DIRECTORIES` skips `generated` because "a checkout from before then can still hold it" (old in-tree output).
- `read` refuses a `build.json` that is not canonical JSON, though only this generator writes it.

Options:

- A: delete both (greenfield rule: no compatibility paths).

## Carried

- `platform_build::inputs::declared_reads` finds build-script inputs by parsing build script source for literal `rerun-if-changed` lines. Cargo's dep-info and `cargo metadata` hold this. Control tab.

## Tests deleted, changed or moved

- Finding 1: tests of guest WIT discovery and binding path parsing go; staleness tests read a fixture dep-info file.
- Finding 2: tests that pin `GENERATOR_ID` or `TOOLCHAIN_ID` read the derived values.
- Finding 3: the canonical-bytes refusal test and any old `generated` directory test go.

## Not read

- `build_record.rs` tests (names only); the 37 call sites (count only).

## Rulings

- Finding 1 (owner, 2026-11-11): neither option as written. The owner asked why freshness is checked at all. Fact: `PACKAGE_TAR_LAYERS` takes the authored `wamn.json`, `command/` and `query/` from the package root and the rest from the build output, so a stale build could ship new authored SQL beside old generated output; the freshness check guards that mix. Ruling: **no freshness checks**. The build copies the authored files it used into its output, and the package artifact takes every file from that one output, so an artifact is consistent by construction; a stale build only publishes the older build (a dev-loop matter). Delete `check_current`, `read_current` and their 37 call sites, the input digests (`digest`, `generation_digest`, `files`), `insert_guest_wit`, `WitBindings`, `binding_paths`, `PLATFORM_INPUTS` and this file's `syn` use. `build.json` keeps the package, dependencies, generator identity and output digests. Alternative considered: keep one check at push, from Cargo's dep-info files.
- Findings 2–3: A.
