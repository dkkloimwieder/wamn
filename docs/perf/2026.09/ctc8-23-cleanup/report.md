# Identity worktree cleanup

The owner authorized this cleanup on 2026-09-08 under `wamn-ctc8.23`.
All six named worktrees were removed, including their disposable targets.
Their local branches remain available for recovery.

Git remote `main` contained `badb2a739afce279acd75cb9e55c43907b00754e` before removal.
The Beads transaction also reached its configured Git-backed remote.
Five worktree commits were ancestors of that main commit.
All three divergent query commits were patch-equivalent to main.
All 151 untracked query evidence files matched committed main blobs.

| Removed worktree under `/home/kaalin/.cache/wamn-lanes/` | Preserved branch | Allocated KiB before removal |
| --- | --- | ---: |
| `ctc8-12-fresh-auth-20260907` | `lane/ctc8-12-fresh-auth-20260907` | 20064184 |
| `ctc8-12-query-20260907` | `lane/ctc8-12-query-20260907` | 109856 |
| `ctc8-13-native-http-20260907` | `lane/ctc8-13-native-http-20260907` | 13421408 |
| `ctc8-14-wasi-http-20260907` | `lane/ctc8-14-wasi-http-20260907` | 3554964 |
| `ctc8-auth-integration-20260908` | `integrate/ctc8-auth-probes-20260908` | 25109284 |
| `identity-auth-plan-20260907` | `plan/identity-auth-proposals-20260907` | 31072960 |

The six `du -sk` measurements total 89.009 GiB.
This measures removed tree allocation, not an isolated change in filesystem free space.
Other work continued on the same filesystem.

The retained copies passed [all four SHA-256 comparisons](retained.sha256).
The three exact probe binaries occupy 956228072 bytes in `retained-binaries/` beside this report in the main checkout.
They remain local build artifacts and are excluded from Git.
The original probe source, receipts, commands, and binary hashes remain committed in their existing evidence directories.
The [historical membership build log](membership-build.log) is retained as found, not as a new gate run.

| Retained filename | Original worktree-relative path |
| --- | --- |
| `retained-binaries/native-http` | `ctc8-13-native-http-20260907/tools/probes/ctc8-13-native-http/target/debug/ctc8-13-native-http` |
| `retained-binaries/wasi-http-host` | `ctc8-14-wasi-http-20260907/tools/probes/ctc8-14-wasi-http/target/debug/ctc8-14-wasi-http-probe` |
| `retained-binaries/wasi-http-guest.wasm` | `ctc8-14-wasi-http-20260907/tools/probes/ctc8-14-wasi-http/target/wasm32-wasip2/debug/ctc8-14-wasi-http-guest.wasm` |
| `membership-build.log` | `identity-auth-plan-20260907/target/membership-build.log` |

No matching process command lines appeared in the host-visible inspection.
Some process working directories were unreadable, so that inspection is not a complete process-ownership proof.
The pilot host, ten pilot run worktrees, and UI worktree retained their original paths and commits.
No branch, shared cache, pilot resource, or UI edit was removed.
