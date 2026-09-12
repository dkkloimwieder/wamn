cwd=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
RUSTC_WRAPPER=sccache
CARGO_TARGET_DIR=<unset; worktree default>
WAMN_SESSION_EXCHANGE_PG_URL=<private loopback fixture URL>
WAMN_SESSION_EXCHANGE_ALLOW_SCHEMA_RESET=1
WAMN_SESSION_EXCHANGE_CTL_BIN=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908/target/debug/wamn-ctl
cargo test --locked --offline -p wamn-identity --test session_exchange -- --include-ignored --nocapture --test-threads=1 
