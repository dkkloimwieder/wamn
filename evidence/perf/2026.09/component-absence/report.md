# Missing component diagnostics

Issue: `wamn-10yt.40`.
Base: `b214506aa6cda434202e22d22941ff19390c37c8`.

A package can declare a component before its crate belongs to a component workspace.
The tool previously reported this case as inventory drift after Cargo returned valid metadata.
The tool now names the absent component crates and tells the author to create and register them.
The message states absence from the declared workspaces, so it also covers a crate that exists outside those workspaces.
The tool still refuses inventory drift and undeclared crates.
Its package allowlist and build arguments stay unchanged.

The regression test invokes the real shell tool with stub Cargo responses.
The metadata responses start from the actual component workspaces.
The fixture adds a package declaration without its component crate.
It then adds the declared crate to the metadata, changes the inventory count, and removes the declaration.
The fixture tests absence through `m1`, `proof`, `build-only m1`, and `watch-roots m1`.
It also makes sure that refusals run no build command.

The focused harness compiles the exact `profile_selectors.rs` file and the real `package_inventory` module.
It uses cached Rust libraries whose `serde_core` dependency fingerprints match.
It does not build the complete conformance crate or run a cluster.
Run it from a worktree with the cached `serde` and `serde_json` libraries:

```bash
python3 docs/perf/2026.09/component-absence/tools/prove.py \
  --tree "$PWD" \
  --evidence-dir "$PWD/docs/perf/2026.09/component-absence/focused-next"
```

The accepted run is `focused-003`.
All seven tests passed in 1.14 seconds, with no ignored or filtered tests.
The helper and test compilation took 0.64 seconds together.
The mutation disabled only the new absence refusal.
The new test failed because the tool returned the old inventory drift message.
After restoration, the same test passed and all source hashes matched.
`bash -n`, focused `rustfmt --check`, and `git diff --check` passed.

The earlier receipts remain available.
`focused-001` records the normal Cargo command, which was stopped after 45.6 seconds because the copied cache required a runtime dependency rebuild.
`focused-002` records a harness compilation failure caused by incompatible cached `serde_core` dependencies.
Neither earlier run counts as a passing test run.
No Cargo manifest, inventory file, component artifact, or production runtime file changed.
