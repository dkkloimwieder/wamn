# Scoped native delivery and retention

Source: `7eefdc7ce078f5d87d174a0aab0703519472a09e`.

The native case failed because it waited for a reconnect state after client drain. The pinned client emits a Closed event instead.

The command returned 101 after 57.481 seconds. `record.json` records the command, and `cargo.log` retains the actual output.
