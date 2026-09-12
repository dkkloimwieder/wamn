cwd=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
RUSTC_WRAPPER=sccache
CARGO_TARGET_DIR=<unset; worktree default>
WAMN_IDENTITY_ISSUER_PG_URL=<private loopback fixture URL>
WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1
cargo test --locked --offline -p wamn-control-provision --test identity_issuer_live -- --include-ignored --nocapture --test-threads=1 
