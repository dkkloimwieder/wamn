Package `wamn_receiving` uses component `receiving`, data crate `wamn-receiving-data-access`, generated library `wamn-generated-receiving-tui`, and UI crate `wamn-receiving-tui` with binary `wamn-receiving`.

The manifest is `wamn.json`. SQL migrations live in `migrations/`, and the data crate uses the generated code in `generated/`.
The `component/` directory contains the application guest, and `ui/` contains its operator application.
Application tests live in `tests/` and beside the source that they exercise.

From the repository root, run the local tests:

```bash
cargo test --locked --offline -p wamn-receiving-tui
cargo test --manifest-path apps/Cargo.toml --locked --offline -p wamn-receiving-data-access --all-targets
cargo test --locked --offline -p wamn-receiving-tests --test receiving_command_histories_live -- --include-ignored --nocapture
```

The history test requires `WAMN_RECEIVING_CORRECTNESS_DOCUMENT` to run its live cases.
The local model tests run without that input.
