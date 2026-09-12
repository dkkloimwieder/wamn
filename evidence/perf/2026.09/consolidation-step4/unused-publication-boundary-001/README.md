The focused schema-control test passed after deletion of the unused SQL function and its Rust helper.
Source commit: `8265633e6a8c9839f5112b6844d14ba6012ebec2`.
The source diff deletes 19 lines from two files.
A search of maintained source found no remaining helper or fault-setting reference.

The command in [result.json](result.json) exited 0 after 7.033431350020692 seconds.
The [raw log](cargo.log) records one passing test, no failures, no ignored tests, and 108 filtered tests.
The existing unused-function warning remains in that log.
The test used Rust 1.98.0, two build jobs, and this worktree's own target directory.
No database or live service ran.

The [source record](source.json) names the parent commit because the deletion was uncommitted during the test.
Both recorded file hashes match the source commit, and both files remained unchanged during execution.
Rust formatting and the Git whitespace check passed.
The [publication map](publication-map.json) records the four original files with their exact hashes and local file modes.
This result covers only the focused schema-control test and source inspection.
