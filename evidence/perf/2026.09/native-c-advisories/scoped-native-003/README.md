# Scoped native broker test

Source: `ba4abf99`.

The diagnostic run failed at its 90-second deadline during runtime stream deletion. Native broker logs showed ten-second gaps between expected permission refusals.

The command returned 101 after 96.169 seconds. Source files stayed unchanged during the run.

`record.json` names the exact command and native binary. `cargo.log` contains the actual output.
