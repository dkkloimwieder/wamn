# Scoped native delivery and retention

Source: `b0a3f0451c2026a8d3959b890650b4c34b6f0276`.

The native case passed. A replacement client received the same unacknowledged sequence, acknowledged it, and received a later message. The broker delivered 65 payloads of 1,047,552 bytes. Each pull stayed at 4,190,208payload bytes under the 4,194,304 byte limit. It stopped at 64 pending acknowledgements, then delivered the final message after acknowledgement. Real source expiry left the termination advisory readable with its payload reported unavailable. All prior environment authority assertions also passed.

The command returned 0 after 30.76 seconds. `record.json` records the command, and `cargo.log` retains the actual output.
