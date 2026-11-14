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

W1. `wamn-execution-contract` and `wamn-pg-core` move from S1 to S2. Tab 01 sends the digest code to the new `wamn-digest` and the scalar format to the new `wamn-scalar`, and tab 06 sends quoting to `postgres-protocol`. These are S2 owners, so the deletion lands with them in one change. S1 keeps the other ruled deletions.

W2. `MAIN_PORT` and `ERROR_PORT` (tab 01, open point): one definition in `wamn-catalog`, which describes node ports. The workflow crate reads it from there. The `jsonata` guest uses the catalog constant if the guest can depend on the catalog without a new host dependency. Otherwise the guest reads the port names from its WIT, and the copy goes. The agreement test `port_constant_agreement.rs` goes.

W3. Read query (tab 01): strictness is dropped, as the owner ruled. The decoder accepts any spelling that it can parse, and the encoder writes one canonical form.

W4. S1 runs as two lanes: S1-A (scenario worker, authoring model, `wamn-gates`, `deploy/gates`, `wamn-gate-harness`, `wamn-client`, P27) and S1-C (user PATs, flow columns, `stream_seq`, the `ServingManifest` strip, P15, the unused WIT world and dependencies, `vendor/pg-walstream`). S1b runs inside these two lanes.

W5. Build output and concurrency (owner, 2026-11-12). This ruling replaces I5. Each lane builds in its own Cargo target, `~/.cache/wamn-target/arch-<lane>`, and kache supplies the reuse. Root removes the target, the worktree and the branch of a lane when the lane merges. The S1 lanes keep the shared target `~/.cache/wamn-target/arch`, and root removes it when both S1 lanes merge. At most five Codex agents run at one time, reviews included. S2-A and S2-C start before S1 merges, and root resolves the conflicts at merge.

W6. `wamn_authority.author_login_tenants` stays in S1 (S1-A question). Tab 84 counted it as a store of the scenario worker, but the connection readers in `deploy/sql/system-schema.sql` and the workload provisioning in `provision_project_env/workload.rs` read it. S0 renames it, because "tenant" is a banned word. The step that rewrites connection authority decides whether it goes.

W7. `vendor/pg-walstream` stays for now (S1-C question). The fork branch `wamn/0.8.0` carries the Keepalive patch on 0.8.0, but the vendored copy is 0.9.0 and differs in 26 paths, among them snapshot and copy support. The newest release is 0.10.0 (crates.io, 2026-11-02), and it still has no public Keepalive event, so the patch stays necessary. The vendor directory goes when the fork has a `wamn/0.10.0` branch, which is upstream 0.10.0 with the Keepalive patch. Root needs owner consent to push that branch to the fork.

W8. Dev loop credentials (owner, 2026-11-13, S2-C review item 1). Ruling 19 deleted the file credential backend, and the dev loop starts `wamn-host` as a local process outside the cluster. The local host runs without connection credentials until S10 moves dev to `kind`. Nobody adds kubeconfig support or restores the file backend. A connection call on a local host fails with the clear error "no credential store in local mode" and never panics. Before S2 merges, root lists the selected test targets that make a connection call on a local host. Each such failure gets an open bead that points at S10. Nobody edits or ignores those tests to make them pass.

W9. Step S0 runs as four lanes by word, because one lane cannot hold about 21,000 matches. S0-K renames `kind` to `type`. S0-T renames `tenant`, `env_key`, `floor`, `gate` and the non-accounting `ledger`. S0-L applies the rules for `claim` and `scope`. S0-C applies the rule for `check`. Root merges them one at a time, and each lane resolves its conflicts in its own worktree. Part 5.3 proposed `project_key` for `env_key`, but ruling R4 already gives `project_key` to the derived key that replaces `tenant_key`, and that function takes `env_key` as its input. So `env_key` becomes `project_env_key`, the name that `wamn-registry` already uses for the same key. The `--tenant` flag stays until the ruling 39 work deletes it.

W10. Generated history migrations (S6 plan question). The compiler emits each history change as a migration in the same SQLx version sequence as the authored migrations of the package. A released generated migration never changes. A change to a history declaration alone produces a new generated migration. SQLx version rules replace the custom gap-free numbering, and its check goes.

W11. Platform helper DDL that `physical_schema.rs::render` marks as `ExecutionOwner::Platform` runs in platform initialization. Package history migrations then run under the restricted migration login.

W12. Before S6-B deletes `wamn-schema-control`, its surviving functions move to their owners: `exposure.rs::normalize_http_route` to the catalog, `replica_identity.rs::reconcile_replica_identity` to the CDC owner, and `converge_project_env_key_identity` to identity.

W13. S6 runs as two serial lanes after S4 merges: S6-A (migration execution) and S6-B (schema lifecycle and the deletion of `wamn-schema-control`). S7 runs beside them. In each shared file, S6 lands its migration removals first, and S7 then owns the whole file.

W14. Palette descriptor ownership (S5-D question): S7 defines the platform ownership of palette guests and connects `palette.json` to release and dev resolution. The old delivery path stays deleted.

W15. S7 runs as four serial lanes after S4 merges: S7-A (invocation authority), S7-B (typed HTTP invocation), S7-C (workflow and palette invocation) and S7-D (edge switch and the final deletion). C30 is corrected: `native_policy.rs::finish_result` creates signing authority after commit, so the invocation record of S7-A keeps that authority. Validation moves into the typed exports before the codecs go. S7-A changes `audit_retention.rs::WRITE_LOG_RETENTION_SQL` so that unfinished claims expire after 30 days (R-06 C).

W16. Claim commands (S7-A question). Ruling 1 applies: an operation with an external effect commits its claim before the effect. The compiler stops requiring `explicit_per_input` for a claim command that has no SQL. An operation that mixes SQL work with an external effect is split into separate operations, and the compiler refuses the mixed form.

W17. Platform migration logins (S6-A question). Ruling 6 applies to platform scopes as well. Each platform migration role gets its own CNPG managed login with one basic-auth Secret, like the S4 roles. The owner roles stay `NOLOGIN`, and the migration login holds their privileges through membership.

W18. JSON values in WIT (S7-B question). A JSON column or field gets its own WIT type in the shared wamn WIT package, apart from `string`. The host converter recognizes that type by its WIT shape. It decodes a JSON request value of any kind into it, and it encodes the result back as JSON, not as a string. The compiler maps JSON to this type in `generate/wit.rs::wit_type`. No second type source, such as the publication schema, takes part in the conversion.

W19. S10 runs as four lanes: S10-A (Helm installation), S10-B (environment and publication), S10-C (native control and CLI) and S10-D (dev on `kind`). A and B start together, C follows B, and D needs A to C and S7-C and S7-D. Decisions on the plan questions:
- The applied environment configuration has one owner: one `configuration jsonb` column on `registry.environment_projects`, typed by `wamn-registry`. The ConfigMap reader `environment/document/store.rs::read_target` goes.
- Rollback follows ruling 32 target B: after a failed head, rollback restores the latest successful revision, also when earlier parts of the same rollout succeeded.
- `release_chart.rs::upgrade` drops `--rollback-on-failure` before pod-report polling goes.
- C27 stays with `broker::authorization::update_authorization`.
- `operation_job.rs::Operation::needs` mounts the credentials of the owning role, never superuser credentials.
- The S10-D `kind` measurement needs owner consent before it runs.

W20. S12 runs as three serial lanes after S7-C: S12-A (bindings and storage), S12-B (clients, tokens and logging) and S12-C (capability crates). Decisions on the plan questions:
- P3: publication seals the connection mode in `ComponentConnection`, and then `CONNECTION_EFFECT_SNAPSHOT_SQL` goes.
- C34: the blob handle records the connection generation, and a changed generation is refused before the backend call.
- P5: wamn consolidates the clients that it owns. The upstream transport inside `oci-client` stays.
- Logging uses the public binding hook of the pinned wash runtime and the host invocation span. Nobody recreates the guest context parser.
- C33 is fixed at `18829c35c`, and the cache replacement keeps that fix.

W21. Blob store aliases on palette nodes (S7-C question). A blob node keeps any store alias per node. The alias comes from the invocation configuration of the node. The sealed palette descriptor keeps the declaration, and nobody seals the `__STORE_ALIAS__` placeholder as a literal alias.

W22. CNPG managed roles (C22, S10-A question). The registry facts own the role list. The installer derives the org and project roles from `wamn-registry` and the fixed platform roles from code, and it writes them into the Helm values on each install. Helm values hold no role list that another source does not derive. Bootstrap installs the platform roles alone, and partial provisioning derives what the registry holds at that time. No new table or column.

W23. S10-D `kind` measurement (owner, 2026-11-13). S10-D runs one bounded `kind` measurement: it publishes one fixture change through the product commands, deploys it through control and times each phase. It runs only when no other build, test or cluster load runs on the machine. If the run fails more than once, the lane stops the measurement and does not diagnose it. Local development uses `kind` in either case, and the local host modes go.

W24. S8 runs as three serial lanes after S7-D: S8-A (run persistence), S8-B (native events) and S8-C (CDC identity and lifecycle). Decisions on the plan questions:
- P2: `wamn-run-state` owns run persistence as functions over a connection that the caller already authorized. `production_lease.rs::RunStore` goes, and the persistence does not move into workflow.
- Workflow already depends on runtime. Workflow orchestrates matching over the plugin transport, and runtime never depends on workflow.
- Reset keeps `reset.rs::stop_users` to stop the reader, and the per-commit lock goes after that.
- The CDC event identity uses the timeline that contains the commit LSN. Transactions that replay after a promotion keep their identity.

W25. S10-D questions. Package coordinates stay immutable: a publish never changes the bytes at an existing coordinate. The dev loop publishes each changed build at a prerelease version taken from its content digest, for example `0.1.0-dev.<first 12 hex of the digest>`, so repeated saves need no manual version change. Surviving tests that call the deleted local assembler or provisioner move to the product `kind` path in S10-D where the product can drive the step. Each step that the product cannot drive becomes a bead against the deploy routes, and its test code does not compensate.
