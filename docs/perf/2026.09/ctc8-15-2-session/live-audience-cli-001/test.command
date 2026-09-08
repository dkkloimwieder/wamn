cwd=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
RUSTC_WRAPPER=sccache
CARGO_TARGET_DIR=<unset; worktree default>
WAMN_SESSION_AUDIENCE_CLI_PG_URL=<private loopback fixture URL>
WAMN_SESSION_AUDIENCE_CLI_ALLOW_SCHEMA_RESET=1
cargo test --locked --offline -p wamn-ctl --test session_audience_live -- --include-ignored --nocapture --test-threads=1 
