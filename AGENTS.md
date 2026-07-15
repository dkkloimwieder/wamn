# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

## How we work here

### 1. Think Before Coding
Don't assume. Don't hide confusion. Surface tradeoffs.

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First
Minimum code that solves the problem. Nothing speculative.
- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### 3. Surgical Changes
Touch only what you must. Clean up only your own mess.

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## Repository structure

- `crates/wamn-host` — production host: the `wash-runtime` washlet embedding + wamn host plugins (`wamn:postgres`, logging) + imperative subcommands (`host`, `dispatch`, `provision-*`, `migrate-catalog`, `publish-catalog`). Thin binary over the lib.
- `crates/wamn-gates` — the gate/bench suite binary (SR1 split); `crates/wamn-gate-harness` — shared measurement helpers.
- `crates/wamn-*` — pure decision crates (no DB/clock/wasm — pure core / effect shell): data model (`catalog`, `ddl`, `schema`, `rls`, `seed`); flow engine + API (`flow`, `runner`, `run-store`, `run-queue`, `node-sdk`, `node-guest`, `nodes`, `node-manifest`, `api`); control plane (`registry`, `provision`, `migrate`).
- `components/` — wasm32-wasip2 guests: production at the root (`flowrunner`, `api-gateway`, `pgprobe`, …), `fixtures/` + `samples/` beneath, `poc-` prefix for POC components.
- `poc/` — POC integration crates (`f1`, `dm1`).
- `deploy/` — Kubernetes manifests + standalone SQL schemas (`postgres-init`, `catalog-schema`, `run-state`, `run-queue`, `system-schema`, …).
- `docs/` — **design source of truth** (`platform-plan.md`, the decision table, WIT contracts, per-subsystem specs). Start here.
- Root `Cargo.toml` pins the `wash-runtime` fork rev in one place (`workspace.dependencies.wash-runtime.rev`).

See `README.md` for a fuller tree and the dev/test/deploy quick commands.

## Build & Test

- Per-bead build + gate-of-record commands: **`docs/build-and-test.md`**.
- Quick dev/test/deploy commands: **`README.md`**.
- Build debug by default (`cargo build` / `cargo test`); use `--release` only when a gate needs it. The in-cluster gate of record uses the two-stage Docker image (`--target host`, `--target gates`).
