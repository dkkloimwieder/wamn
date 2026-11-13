# Implementation rulings

As of 2026-11-12. This file records each ruling that the implementation of `review-report.md` needs. The user delegated these rulings to the orchestrator on 2026-11-12. Each ruling has a number, the question, the decision and the reason. A later ruling that changes an earlier one names it.

The controlling principles, from the user, verbatim: "single responsibility, single source of truth, no rewrite, restructure or rename is too large. preference for deletion of all accidental complexity".

The testing rule, from the user: keep tests to a minimum, test only interfaces, never test the presence of a string or a file, never compare strings or files exactly, and avoid complex mocks.

## How the work runs

I1. Branch. All work lands on `worktree-arch-review`, which starts at `5d22e687a` (main at the start). The orchestrator pushes the branch to origin after each integration. Nothing merges to main. Reason: the user asked for frequent pushes for external review, and did not ask for a merge.

I2. Lanes. Each lane is a Codex `gpt-6.1-sol` agent at high effort in its own worktree under `.claude/worktrees/lane-*`, on a branch `lane/<step>-<letter>` from the current head of `worktree-arch-review`. A lane edits files and runs checks. It does not commit, push or run `bd`. The orchestrator reviews the diff, commits it in the lane worktree and merges it into `worktree-arch-review`.

I3. Review. Each finished lane gets a separate read-only Codex `gpt-6.1-sol` review at high effort. The review reads the diff against the brief, the report and these rulings. The orchestrator sends the findings back to the same lane before the merge (findings first).

I4. Checks per lane: `cargo fmt --check`, `cargo clippy --workspace --all-targets` in each Cargo workspace that the diff touches, and the unit tests of the changed packages. No live cluster run, no `kind` run, no deployment and no full `tools/test-changes` selection. A focused Postgres test can run when a SQL change needs proof, and the orchestrator runs it, because the lane sandbox cannot start Postgres.

I5. Build output. Lanes share one Cargo target, `~/.cache/wamn-target/arch`, as the disk rule requires. Cargo serializes on its lock. Before a test that reads files through `CARGO_MANIFEST_DIR`, a lane runs `cargo clean -p wamn-fixture-package`. The target is removed when the work stops.

I6. Tracking. One bead epic holds the work, with one child bead per step. A child closes on the commit that lands its step.

I7. Steps are epics. The user's instruction of 2026-11-12 ("you are not to stop after epic 1") treats the steps as epics. This answers question 28: the steps S0 to S13 are the epics of this work.

I8. Docs. Each step corrects the `docs/architecture/` pages that it changes, as the report requires. `docs/history/` stays as written. The Claude Doc of the review is not edited by this work.

## Open points of the report

R1. Signing-key rotation cadence (ruling 30): 30 days. The identity service keeps the previous public key for verification until the longest token lifetime after the rotation ends. Reason: one fixed schedule, and verification needs no second rule.

R2. Guest memory budget (ruling 35): the host takes one budget value from its configuration, with a default of 256 MiB per instance. S9 measures the fixture applications, and a measured peak above half the default changes this ruling. Reason: a value is needed to enforce, and the measurement belongs to S9.

R3. R-06 timeout between `in-progress` and `outcome-unknown`: the longest call, `MAX_HOST_CALL_DURATION` (30 s). A claim with no result that is older than the longest call is `outcome-unknown`. Reason: one source of truth for the bound, and no new setting.

R4. Question 40: `project_key` replaces `tenant_key`, and `current_project_key()` replaces `current_tenant_key()`.

R5. Question 41: the word rename (step S0) runs after S1 and S1b land, so that it does not rename deleted code.

R6. Question 42: yes. The error convention in `CLAUDE.md` changes from "a kind" to "a type", and error types use `ErrorType`, as `docs/history/kind-to-type.md` decided.

R7. Question 43: yes. "claim" alone means a write log claim, "JWT claim" keeps its standard name, a queue claim of a run becomes "lease", "scope" means the invocation scope record, "environment" means the hierarchy level, and "check" means a SQL `CHECK` constraint.

R8. P15 and P27 (step S1b): accepted. They delete code with no product caller, which the principles require.

## Rulings made during the work

(None yet.)
