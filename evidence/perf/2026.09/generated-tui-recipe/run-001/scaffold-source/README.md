This crate is developer-owned Rust over the generated receiving screens.
Edit the copied functions in `src/screens/` to add composition.
Keep the direct calls in `src/lib.rs` for screens that you do not override.
To remove an override, call its function under `generated::screens` again.

Typed API incompatibilities fail this crate's build.
This scaffold must pass its declared interaction tests against regenerated bindings.
The initial tests cover the selected operation kind and session reset.
Add assertions for your custom workflow.
An additive field that no assertion reads can pass.

After Generate completes, run `cargo test --manifest-path packages/receiving/ui/Cargo.toml`.
Supply `WAMN_BASE_URL`, `WAMN_HOST`, `WAMN_TOKEN`, and `WAMN_TARGET_INSTANCE` from the active development session before launch.
