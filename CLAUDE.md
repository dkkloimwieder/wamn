CONCISE OUTPUT ONLY!

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

### 4. Findings & Closure

The findings ledger is `docs/archive/findings.md` — the single findings file. Reviews add sections, not new documents.

A finding **closes on a commit that removes or fixes code — never on a decision that plans to.** Decisions change a finding's *priority*; only commits change its *status*. Questions close on **verified evidence, cited to source.** Every closed row carries its commit hash, bead id, or evidence citation.

Close findings in the commit that carries the finding ID (`fix(R13): ...`); a single integration pass then sweeps the status board — evidence first, board second. Do not edit `docs/archive/findings.md` from parallel worktrees.

### Rust
Almost all code here is Rust — consult the `rust-guidelines` skill when writing, reviewing, or refactoring it. At public, persisted, and WIT/wire boundaries, repository-defined WIT-shaped error enums and frozen serialized literals are the controlling contract. Inside implementations, use contextual error structs carrying a kind, source, and relevant context, then translate exactly once at the owning boundary. Do not create a parallel wire taxonomy or mechanically retrofit unrelated errors; retrofit existing code only when a named boundary leak requires it or when that owner is already being changed. This scoped project convention overrides conflicting skill guidance.

## Repository structure

- `services/{cdc-reader,ctl,dispatcher,executor,host,scenario-worker,waker}` — independently deployable binaries and their service-owned integration tests. `node-host` and `builder` were **deleted, not deferred**: `wamn-0h0g.6.3` (`f6bc01eb`, "delete custom component plane") removed both along with `crates/node`, and only the archived `docs/archive/platform/builder.md` survives. There is therefore no in-tree OCI push path to reuse.
- `crates/{authoring,catalog,control,data,events,execution,identity,platform,scenarios,schema}` — bounded-context libraries, organized by domain and then package.
- `components/{execution,ingress}` — production wasm32-wasip2 guests; reusable test and example guests live under `components/{fixtures,samples}`. `components/nodes` went with the demo/POC surface (`wamn-0h0g.12.2`, `3554f140`).
- `tests/{orchestrator,conformance,integration,system}` — proof owners, from orchestration helpers and static conformance through integration and system gates.
- `test-support/{harness,fixtures,infrastructure}` — shared proof support that is not itself a deployable or proof owner.
- `deploy/` — tiered (SR8, `deploy/README.md` holds the rules): `infra/` install-once infrastructure, `platform/` production manifests, `gates/` gate/bench Jobs, and `sql/` standalone SQL schemas. The former `poc/` tier was deleted by `wamn-0h0g.12.2` (`3554f140`); `deploy/mvp/` (bootstrap scripts) exists on disk but is not one of the tiers `deploy/README.md` names.
- `docs/exe-model.md` — the single WIP design authority. `docs/PLAN/PLAN.md` is the non-normative ordering and ambiguity map; Beads and git own status. `docs/archive/` contains provenance and operational ledgers, never competing design authority.
- Root `Cargo.toml` pins the `wash-runtime` fork rev in one place (`workspace.dependencies.wash-runtime.rev`).

See `README.md` for a fuller tree and the dev/test/deploy quick commands.

## Build & Test

- Per-bead build + gate-of-record commands: **`docs/archive/build-and-test.md`**.
- Quick dev/test/deploy commands: **`README.md`**.
- Build debug by default (`cargo build` / `cargo test`); use `--release` only when a gate needs it. The in-cluster gate of record uses the two-stage Docker image (`--target host`, `--target gates`).
<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->

CONCISE OUTPUT ONLY!
