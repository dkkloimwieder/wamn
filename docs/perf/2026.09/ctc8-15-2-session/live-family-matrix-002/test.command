cwd=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
RUSTC_WRAPPER=sccache
CARGO_TARGET_DIR=<unset; worktree default>
WAMN_DENIAL_MATRIX_PG_URL=<private loopback fixture URL>
cargo test --locked --offline -p wamn-control-provision --test family_denial_matrix -- --include-ignored --nocapture --test-threads=1 
