# Agent-authoring tooling — work specification

Status: proposal for ruling. Beads become the record once ruled.
Evidence: wamn `69f4281`. Every claim carries a file:line or a bead id; anything
without one is a design choice and says so.

Scope: the tools that let a coding agent author a wamn package and let us measure
it. This document knows no application. Tasks are fixtures that satisfy the
interface in A6; they live in the protocol (`docs/experiments/agent-authoring/protocol.md`).

---

## 0. Platform facts the tools build on

| # | Fact | Evidence |
|---|---|---|
| F1 | Product binary `wamn`, one subcommand `dev`; operator CLI `wamn-ctl`. | `services/ctl/src/bin/wamn.rs:1-23`, `services/ctl/Cargo.toml:21-23`, `services/ctl/src/main.rs:14` |
| F2 | `wamn dev --config FILE --overlay-root DIR [--watch] [--tui]`; `--tui` is a renderer flag on the same command. | `services/ctl/src/dev/command.rs:28-43`, `:285-291` |
| F3 | Session modes live in `DevSession::run_with_observer(observer, hold_after_one_shot)`: watch · once+hold · once+teardown. Plain path hard-codes teardown; the TUI calls `run_until_shutdown` (hold). | `command.rs:249-283`, `:242-247`, `:293`, `dev/tui.rs:440` |
| F4 | Migrate…Gate run on saved bytes; Publish…Activate require a committed source; dirtiness is whole-worktree including untracked files. | `services/ctl/src/dev.rs:95-106`, `:400-415`; `dev/watch.rs:146`, `:194-233` |
| F5 | Plain one-shot tears down before printing `run served: <base_url> host=<route_host>`; watch prints only `watch completed:`. | `command.rs:293-297`, `:422-435`, `:142-149` |
| F6 | The local host binds `127.0.0.1:0`. | `dev/activation.rs:39`, `:454-456` |
| F7 | Failure prints `<kind> at <stage>: <remedy or source>`; success prints `run completed: <stages>`. | `dev.rs:313-323`, `command.rs:437-446` |
| F8 | Apply writes the durable target database; the verification database is fresh per run and removed after. | `dev/coordinator.rs:876-878`, `dev/verification_database.rs:1`, `dev/config.rs:634-639` |
| F9 | Route-caller operation grants reconcile from the package manifest at Apply. | `crates/control/provision/src/operation_grants.rs:1-20`, `services/ctl/src/apply_package.rs:11` |
| F10 | The standup (`wamn-dev-env`) is docker compose plus a provisioning module; it writes `dev.json` and `route-caller-pat.json` and holds the Gate. Its org/project/env/tenant are constants. | `tests/integration/src/bin/wamn-dev-env.rs:26-146`, `tests/integration/src/dev_environment.rs:116-122`, `:322`, `docs/operations/build-and-test.md:1141-1247` |
| F11 | `wamn dev up` is in progress: provisioning moves into `wamn-ctl`, the Gate becomes a spawned child on a fixed port. | bead `wamn-10yt.10.32` (in_progress) |
| F12 | The loop resolves the package under `--overlay-root` and its `base_dependencies`; whether a package with none runs through `wamn dev` is unverified (both existing bases ship via journeys). | `dev/config.rs:1060-1090`; `tools/*-cluster-journey-run` |
| F13 | Platform HTTP contract: `POST` routes with a JSON-array body of items carrying `request_id`; response array of `{request_id, value}` or `{request_id, error:{code,…}}`; permission refusal is 403 `{error:{code:"permission-denied", operation}}`; `auth-policy: pat`, bearer token from the minted PAT file. | `tests/integration/src/route_authentication_live.rs:2728-2748`, `:3462-3470`; F10 |
| F14 | Claim law: idempotent replay returns the immutable original result; a changed request under the same key typed-refuses. Generator half in progress. | `docs/poc/wamn_wms_application_poc_scenario.md:198-209`, bead `wamn-10yt.19` |
| F15 | Package content must carry no host, URL, secret reference, or environment name (admission half in progress). | bead `wamn-10yt.20` |
| F16 | A new component must be a member of the components workspace. Membership costs two shared files: the `members` line in `components/Cargo.toml`, and the `[[package]]` stanza in `components/Cargo.lock`, which `cargo metadata --locked` requires to be current. | `components/Cargo.toml:10-13`, bead `wamn-10yt.10.40` |
| F17 | Beads hooks are guarded by `command -v bd`. `AGENTS.md` is a symlink to `CLAUDE.md`. `CLAUDE.md:107` names the global `rust-guidelines` skill; `:130` names `.agents/skills/beads`. No `.claude/skills` in the repo. | `.claude/settings.json`, `.beads/hooks/post-checkout:4`, `CLAUDE.md` |
| F18 | Test-set execution against a candidate wiring is being built on the management side; no handler yet. | beads `wamn-0h0g.8.5` (in_progress), `.8.5.4` (open) |
| F19 | The loop's read seam and activated-endpoint publish exist; the loop console is the view-only client. | beads `wamn-10yt.10.26`, `.10.27` (closed), `wamn-dggp` |
| F20 | Portfolio rule: simulators drive real routes; never seed the database. | `docs/poc/poc-application-portfolio.md:49` |
| F21 | Authored SQL is verified by a hand-written per-package conformance test (native sibling + `cargo sqlx prepare --check`); there is no generic statement verifier in the generator or the loop. A new package's statements are first proven at runtime. | `tests/conformance/tests/receiving_sqlx_verifier.rs`, `docs/operations/build-and-test.md:236-249`, `docs/sqlx-data-access-spec.md:26-31`; no `prepare` under `crates/schema/*`, `services/ctl/src/dev` |
| F22 | Component unit tests run natively: `cargo test --manifest-path components/Cargo.toml -p <crate> --all-targets`; wasip2 via `cargo check --target wasm32-wasip2`. | `docs/operations/build-and-test.md:239-242` |

Consequences: a script cannot reach the activated release (F5, F6) → A0. The agent
must commit before Activate (F4) → the brief says so. Fixture files stay outside the
worktree (F4). The provider must take identity from the task (F10) → `.10.32`
dependency. Nothing seeds a database (F20) → grading drives routes.

---

## 1. Conventions

- `$WAMN_PILOT_HOME` = `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-pilot` (not `/tmp`;
  `build-and-test.md:1134-1139`). `$RUN` = `$WAMN_PILOT_HOME/runs/<nnn>-<agent>-<task>`.
  `$TARGET` = `$WAMN_PILOT_HOME/target-<short-commit>`, shared per commit.
- One run per machine at a time; never concurrently with a cluster journey.
  Ports: the documented fixed set (PG 54332, registry 5004, NATS 4224, Tempo 3201,
  OTLP 4319).
- Exit codes: 0 ok · 2 usage · 10 environment · 20 driver · 30 grading · 40
  teardown residue. Every script: `set -euo pipefail`, `umask 077`, an EXIT trap
  that always attempts teardown.
- No version suffixes. Run directories are snapshots on the `docs/perf/2026.09`
  pattern; the protocol is the only living document.

---

## A0. Product change: `wamn dev --hold`

The only required product change (F5, F6).

Two independent axes on `wamn dev`, both already present in the session (F3):
session mode = once (default) · once+hold (`--hold`) · watch (`--watch`);
renderer = stdout (default) · `--tui`. `--hold` sets `hold_after_one_shot = true`
on the plain path. No clap conflicts: with `--tui` it is redundant, with `--watch`
a no-op.

Behavior on the plain renderer: run once; on failure print the error (F7), exit
non-zero, no hold; on success print, flushed and in order, `run completed: <stages>`,
`run served: <base_url> host=<route_host>`, `run holding`; hold until SIGINT/SIGTERM;
then the existing cleanup (`runner.shutdown()`) and exit 0.

Change list: `DevCommandArgs.hold` (`command.rs:28-43`) with `with_hold(bool)`;
pass it at `command.rs:293`; a `DevWatchObserver::served(&DevRuntimeEndpoint)`
hook with a default no-op, implemented by `CommandObserver` with the existing
`println!`, called after `run_once_command` succeeds and before
`wait_for_shutdown`; the TUI ignores it (it reads the read handle). Tests: parse
`--hold`, `--hold --tui`, `--hold --watch`; a `[WAMN-DEV-LIVE]`-family live test
asserting the three lines, a TCP connect to the printed port while holding, and no
verification database after SIGINT.

Exit gate: from a clean worktree, `--hold` prints `run served:` with a port that
accepts a connection while the process lives, and leaves no verification database
after SIGINT.

Lands as `wamn-10yt.10.33`, after `.10.32` (same file).

---

## A1. Runner `tools/agent-pilot-run`

Bash. Reads a task manifest (A6). Owns nothing about provisioning or any package.

```
tools/agent-pilot-run up      --run <nnn> --agent claude|codex|stub --task <dir> [--standup dev-env|dev-up]
tools/agent-pilot-run launch  --run <nnn>
tools/agent-pilot-run grade   --run <nnn>          # delegates to tools/agent-pilot-grade
tools/agent-pilot-run down    --run <nnn> [--discard-worktree]
tools/agent-pilot-run all     --run <nnn> --agent … --task …
```

Idempotent per `--run`: `up` on an existing run is exit 2; `down` twice is exit 0.

### Standup providers

Both yield `$RUN/env/dev.json` and `$RUN/env/route-caller-pat.json`; nothing
downstream knows which ran.

- `dev-env` (default until F11 lands): the documented sequence
  (`build-and-test.md:1141-1247`) verbatim, parameterized by run id (compose
  project `wamn-pilot-<nnn>`), package sources and overlay root from the task
  manifest. Limitation: identity is fixed to the module's constants (F10); a task
  whose manifest names a different `org/project/env/schema/tenant` is refused with
  exit 10 naming the field.
- `dev-up` (once `.10.32` lands): `wamn dev up` with the identity and package
  flags from the manifest; the provider records the spawned Gate's port and PID.
  Requirement on `.10.32`: accept `--org --project --env --schema --tenant
  --route-host --package… --overlay-root`.

### Layout of `$RUN`

```
run.json          A5
task.json         copy of the manifest
env/              provider output (0700)
worktree/         git worktree, detached at the pinned commit, no remote
fixture/          the task directory copied verbatim (brief, scenario, steps)
bin/              wamn (shim, A4) · wamn-ctl → $TARGET/debug/wamn-ctl
transcript.jsonl  driver stream, verbatim, line-buffered
driver.json       A3
verbs.jsonl       A4
dev-logs/         NNN-<hhmmss>.out/.err per wamn invocation
baseline.out      pre-agent wamn dev run on the untouched worktree (if the manifest names a baseline package)
final.diff · final.status · commits.log
skills.json       global and repo skill inventory (F17)
env.log           standup/teardown output, credentials never echoed
grade/            A7 output
REPORT.md         written by the agent, outside the worktree (F4)
```

### `up`

1. Preflight: `docker wash cargo jq psql curl git openssl inotifywait flock`, bash ≥ 5.1,
   and the agent binary; five ports free; source tree clean at the pinned commit;
   `cargo metadata --offline` on both workspaces; manifest validates (A6).
2. Build into `$TARGET` unless cached: `wamn`, `wamn-ctl`, `wamn-host`,
   `wamn-dev-env` (dev-env provider only), `http_route.wasm`; all `--locked
   --offline`, `RUSTC_WRAPPER=`; durations recorded.
3. Worktree: `git worktree add --detach`; `git remote remove origin`; assert clean.
4. Skill inventory → `skills.json`: `~/.claude/skills`, `~/.agents/skills`,
   `~/.codex/skills`, and the worktree's `.agents/skills`, `.claude/skills`, each
   `{name, path, sha256}`. `launch` refuses if it differs from run 001's.
5. Standup via the provider.
6. Baseline, only if the manifest names `baseline.overlay_root`: from the
   worktree, `wamn dev --config $RUN/env/dev.json --overlay-root <it>` →
   `baseline.out`; must print twelve stages, else exit 10 and `down`.
7. `run.json` phase `up: ok`.

### `launch`

Environment exported to the driver only:

```
PATH=$RUN/bin:$TARGET/debug:<system PATH with bd removed>
WAMN_PILOT_REAL_WAMN=$TARGET/debug/wamn
WAMN_DEV_CONFIG=$RUN/env/dev.json
WAMN_ROUTE_HOST=<manifest.route_host>
WAMN_ROUTE_CALLER_PAT_FILE=$RUN/env/route-caller-pat.json
WAMN_PILOT_RUN_DIR=$RUN
WAMN_PILOT_TASK_DIR=$RUN/fixture
CARGO_TARGET_DIR unset
```

`bd` off `PATH` (F17): hooks no-op, the pilot cannot write the task system;
recorded as a known deviation. cwd `$RUN/worktree`; prompt = `$RUN/fixture/BRIEF.md`.

### `down`

SIGINT the Gate (30 s, then SIGKILL); kill any `wamn-host`/`wamn dev` under `$RUN`
(recorded as residue); compose down `--volumes --remove-orphans` or the `dev-up`
teardown; `git worktree prune` (remove only with `--discard-worktree`); verify no
containers with the project label and no listener on the five ports; residue →
exit 40.

Exit gate: `all --run 000 --agent stub --task <any manifest>` produces the full
layout and `down` exits 0 twice.

---

## A2. Worktree contract

Detached at the pinned commit, no remote, clean before launch. The agent commits
locally (F4); nothing is pushed. `.beads/`, `.claude/`, `.codex/` unchanged by
construction; A7 verifies.

---

## A3. Drivers

Raw stream to `transcript.jsonl`, one JSON object per line, line-buffered;
`driver.json` = `{agent, binary, version, args, started, ended, exit, reason,
model_id, session_id}`.

```
claude --print --output-format stream-json --verbose --permission-mode bypassPermissions "$(cat $RUN/fixture/BRIEF.md)"
codex  exec --dangerously-bypass-approvals-and-sandbox --json "$(cat $RUN/fixture/BRIEF.md)"
```

`model_id`/`session_id` from the first stream event carrying them; raw event
stored. Timeouts in order: idle 5 min (no stream line and no worktree change via
`inotifywait -r`) · step 20 min (no stream line) · cap 90 min. SIGTERM the tree,
15 s, SIGKILL; `reason` ∈ `completed | idle-timeout | step-timeout | run-cap | killed`.
`stub` prints a fixed stream and exits 0; `--stub-mode completed|idle|step|cap`
reproduces each exit reason for the tooling tests (T). No skills sync, no follow-ups, no
symlinks in the baseline arm.

Exit gate: "print the working directory and exit" completes under both real
drivers with `reason: completed`.

---

## A4. Shim `$RUN/bin/wamn`

```bash
#!/usr/bin/env bash
# bash >= 5.1: `wait` covers process substitutions (preflight checks the version).
set -u
real="$WAMN_PILOT_REAL_WAMN"; dir="$WAMN_PILOT_RUN_DIR"
n=$(flock "$dir/verbs.lock" bash -c \
     'c=$(( $(cat "$1/verbs.counter" 2>/dev/null || echo 0) + 1 )); echo "$c" > "$1/verbs.counter"; printf "%03d" "$c"' _ "$dir")
ts=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ); t0=$(date +%s%3N)
out="$dir/dev-logs/$n-$$.out"; err="${out%.out}.err"
"$real" "$@" > >(tee -a "$out") 2> >(tee -a "$err" >&2); rc=$?
wait                                   # both tee substitutions have flushed
ms=$(( $(date +%s%3N) - t0 ))
flock "$dir/verbs.lock" jq -cn --arg ts "$ts" --arg n "$n" --argjson exit "$rc" --argjson ms "$ms" \
   --args '{ts:$ts,n:$n,argv:$ARGS.positional,exit:$exit,ms:$ms}' -- "$@" >> "$dir/verbs.jsonl"
exit "$rc"
```

Rows `{ts, n, argv[], exit, ms}`; `n` is allocated under a lock at start, so a
backgrounded `--hold` and a concurrent `wamn dev` never share a number or a log
file; the row is appended under the same lock when the call ends. `wait` closes
both `tee` substitutions before the row, so `.out` is complete at the join.

Exit gate: `wamn --version` through the shim yields one row and identical stdout;
two concurrent invocations yield two rows, two distinct log pairs, and `.out`
files that end with the binary's last line.

---

## A5. `run.json`

```json
{"run":"…","commit":"…","task":"…","agent":"…","standup":"dev-env|dev-up",
 "machine":{"load_at_launch":"…","cores":0},
 "build":{"cached":true,"seconds":0},
 "up":{"ok":true,"seconds":0,"baseline_stages":12},
 "launch":{"started":"…","ended":"…","exit":0,"reason":"completed","model_id":"…","driver_version":"…"},
 "verbs":{"wamn_dev_runs":0,"wamn_dev_failed":0,"wamn_dev_hold_runs":0,"first_green_minutes":0},
 "git":{"commits":0,"files_changed":0,"outside_allowed_paths":[]},
 "skills":{"present":[…]},
 "down":{"ok":true,"residue":[]}}
```

`outside_allowed_paths` from `final.diff` + `final.status` against
`manifest.allowed_paths`. `first_green_minutes` = first `verbs.jsonl` row whose
`.out` contains `run completed:` with twelve stages, minus `launch.started`.

---

## A6. Task manifest — the interface a task plugs into

A task is a directory. The tools read only `task.json`; everything else in the
directory is handed to the agent or the grader verbatim. `SCENARIO.md` and
`steps.json` are an application brief (protocol §4.5) split into its domain
sections and its exit gate; a brief produced by S0 lands here unchanged once a
human ratifies it.

```
<task-dir>/
  task.json      the manifest below
  BRIEF.md       the prompt (protocol owns its text)
  SCENARIO.md    what to build, in domain terms
  steps.json     grader HTTP steps (schema below)
```

```json
{
  "task": "<name>",
  "identity": {"org":"…","project":"…","env":"…","schema":"…","tenant":"…","route_host":"…"},
  "package_sources": ["packages/<existing-base>", "…"],
  "overlay_root": "packages/<package-under-development>",
  "baseline": {"overlay_root": "packages/<existing>"},
  "allowed_paths": ["packages/<x>/**", "components/application/<x>/**", "components/Cargo.toml", "components/Cargo.lock"],
  "grade": {"steps": "steps.json", "checks": ["claim-replay","row_version","naming"], "fence_reports": ["capability-surface","additive-migration","no-environment-data"]}
}
```

`baseline` is optional and absent for greenfield tasks. `overlay_root` is the path
the agent must create for a greenfield task; it need not exist at the pinned
commit. The brief names it.

`steps.json` — HTTP steps as data, executed in order against the held base URL.
`must: true` steps decide PASS/FAIL (protocol §5); others are recorded:

```json
[{"id":"…","must":true,"invariant":"<brief-invariant-id>","route":{"from":"attachments","operation":"<domain>.<action>"},
  "body":{…}, "reuse":{"<field>":"<step-id>.value.<path>"},
  "expect":{"status":200,"item":"value|error","error_code":"…","equals":{"<path>":"<step-id>.value.<path>"}}},
 {"id":"…","must":true,"sql":"select count(*) …","expect":{"rows":1}},
 {"id":"…","must":true,"concurrent":["<step-id>","<step-id>"],"expect":{"exactly_one":{"error_code":"…"}}}]
```

`route.from: attachments` means the grader resolves the path by registered
operation from the package's `publication/attachments.json`, so an agent's
naming choice is recorded, not failed. Steps only drive routes (F20); the `sql`
step is read-only.

Validation at `up`: identity fields non-empty; `allowed_paths` non-empty and
covering `overlay_root`; every `package_sources` path exists at the pinned commit
(`overlay_root` is not required to); `steps.json` parses, has at least one
`must` step, and passes V3's traceability and predicate-floor checks.

---

## A7. Grader `tools/agent-pilot-grade --run <nnn>`

Generic. Writes `$RUN/checklist.json` and `$RUN/grade/*`.

1. **Loop** — from the worktree, `wamn dev … --overlay-root <manifest> --hold`
   (grader-owned) → `grade/dev.out`; PASS iff twelve stages and `run served:`.
   Keep alive for step 3.
2. **Paths** — `outside_allowed_paths == []`; `git diff <pinned>..HEAD --stat --
   .beads .claude .codex` empty.
3. **Steps** — execute `steps.json`: bearer from the PAT file, `Host:
   <route_host>`, one-item array bodies; every request/response verbatim to
   `grade/http.jsonl`; `concurrent` steps fire in parallel and count results.
4. **Checks and fence reports.** The grader is not a second policy. Where the
   loop already fences a rule, the grader reports the loop's verdict (the stage
   passed or the `<kind> at <stage>` line) and runs nothing of its own:
   - `capability-surface`: Admit's registry decision (`2a-capability-registry.md:56-68`).
   - `additive-migration`: the migration validator's decision at Migrate.
   - `no-environment-data`: the admission fence per the re-specified
     `wamn-10yt.20` (host quarter and secret-shaped values; environment slugs
     are indistinguishable from legitimate values and are not grepped). Until
     that fence lands the field reads `unfenced` and nothing is checked.

   The grader runs only checks with no fence in the loop:
   - `claim-replay`: the first create-shaped step repeated with the same key
     returns an equal `value`; with a changed body returns a typed refusal (F14).
   - `row_version`: the concurrency column is spelled exactly so.
   - `naming`: singular snake_case wire identifiers, closed action set, no
     version suffixes (`docs/poc/poc-work-order.md:10-16`). Removed the day Gate
     fences it.
5. SIGINT the held loop; assert no verification database remains.
6. `--replay <dir>`: runs steps 2–4 against a fixture `attachments.json`, a
   recorded `grade/http.jsonl`, and a worktree snapshot, with no environment;
   the tooling tests (T) use it.
7. `checklist.json`: `{loop, paths, steps:[{id,must,pass,evidence}], checks:{…}, fences:{…},
   human:{…:null}}`. Human fields (protocol §5) refuse `null` downstream.

Exit gate: two graders on one run produce identical machine fields.

---

## A8. Report `tools/agent-pilot-report --run <nnn>`

Fills protocol §10 from `run.json`, `verbs.jsonl`, `driver.json`,
`checklist.json`, `skills.json`, and `$RUN/human.json` (`{Q3, Q7, Q8, Q9,
Q10.activated, Q11, E1..E7}`; the grader tags activation from the transcript per
S8 until B5 measures it); exits 30 on any `null`.
Writes `docs/experiments/agent-authoring/<nnn>-<agent>-<task>.md` and the raw
directory beside it minus `env/` and `worktree/`.

Exit gate: the first report regenerates byte-for-byte from the raw directory.

---

## S. Skills

Comparison basis: golem's catalog at `d667fee` — ~110 skills (31 common, 39 per
language), each self-contained with a trigger-phrased description, a Related
Skills table, numbered Steps, a Complete Example, and Key Constraints
(`golem-skills/skills/rust/golem-add-http-endpoint-rust/SKILL.md:1-25`); 75
harness scenarios assert which skills loaded (`golem-skills/tests/harness/scenarios/`).
wamn needs about a fifth of that count, for the right reason: its guest surface is
`wamn:node` plus effect WITs, one language, no guest-visible durability API. It
needs the same shape.

### Shape rule

- One task per skill; the skill carries the **procedure and one complete,
  correct example** for the current tree. It restates the *what*; the authority
  document keeps the *why* and is linked, not copied. Drift is caught by
  execution, not by prose discipline: every skill's example runs in the harness
  (T, B5).
- `description` is a trigger: "Use when the user asks to …", naming the
  synonyms an agent would use. Both agents select skills by description (F17).
- A **Related Skills** table: which neighbour to load, and when.
- Sections, fixed order: Overview · Related Skills · Steps · concept sections
  with code · Complete Example · Key Constraints · What has no verifier.
- Frontmatter `name`, `description` (the `.agents/skills/beads` shape). One
  root, `skills/<name>/SKILL.md`; `.agents/skills` and `.claude/skills` are
  symlinks to it, the same shape as `AGENTS.md → CLAUDE.md` (F17). No sync
  stage: two directories would be two emitters. The existing
  `.agents/skills/beads` moves under `skills/` in the same commit. No language
  split until a second guest language exists.
- No skill for an unlanded surface: the component-SDK skill waits on
  `wamn-0h0g.13.7`, the flow-test skill on `.8.5.4`.

### Catalog

Authoring (the skills arm ships these):

| id | name | teaches | links | verb / gate |
|---|---|---|---|---|
| S0 | `wamn-application-brief` | intake: from a short ask, interview to fill the application-brief skeleton (protocol §4.5); pin nouns, commands with invariants, queries, ingress, external services from the admitted interfaces only, permissions, UI screens, non-goals, exit gate; emit the brief, a draft `steps.json` from the exit gate, and a list of asks that need an unadmitted capability, flagged as platform work | protocol §4.5, `2a-capability-registry.md:56-68`, `.20` | none (produces the fixture) |
| S1 | `wamn-dev-loop` | twelve stages, saved-bytes vs committed boundary, `--hold`, reading `run completed` / `<kind> at <stage>`, log locations | `dev.rs:95-106`, A0 | `wamn dev` |
| S2 | `wamn-package-manifest` | `wamn.json`: package identity, models (`server_owned_fields`, `enum_fields`), connections, components; additive migrations; `row_version`; never hand-edit `generated/` | `docs/sqlx-data-access-spec.md` | Migrate → Generate |
| S3 | `wamn-custom-operation` | a `custom_operations` entry (kind, permission, connection, transaction, input/result, errors, relations with lock flags, statement paths), the SQL file, the claim law | manifest schema, F14 | Gate |
| S4 | `wamn-component-operation` | exporting an operation from a wasip2 component at the package's interface version, `wamn:node` inbound, node-error taxonomy, admitted imports, workspace membership | `docs/architecture/2a-capability-registry.md:56-68`, `wamn-node/package.wit`, F16 | Build → Admit |
| S5 | `wamn-wiring-and-route` | wiring JSON, attachment JSON (`route.path`, `input-schema`, `auth-policy`), publication template ports, permission tokens (no grant step, F9) | F13, F9 | Gate → Apply |
| S6 | `wamn-verify-route` | `--hold`, `Host` + bearer from the PAT file, the array envelope, `value` vs `error.code`, 403 shape, writing a verification section | F13 | `curl` |
| S9 | `wamn-test-package` | the author procedure (protocol §4.6): native unit tests, wasip2 check, loop to Gate on saved bytes, commit, `--hold`, route steps incl. replay / changed-body / contention / not-found, verbatim recording; what has no verifier today | F21, F22, A0 | `cargo test`, `wamn dev`, `curl` |
| S10 | `wamn-query-projection` | authored `query/*.sql`: bounded lists, keyset pagination, the sort/filter vocabulary, `get`-style refusals, how the corpus is welded | `docs/sqlx-data-access-spec.md:26-31` | Generate |
| S11 | `wamn-overlay-package` | `base_dependencies`, ownership at definition level, the additive-base rule, what an overlay may not touch | `docs/poc/poc-work-order.md` slice iv | Gate |
| S12 | `wamn-connection-binding` | connection kinds, `bind-connection`, what a guest never sees (credential hiding), `wamn:connection` request shape incl. the idempotency key field | `exe-model.md:97-100`, `connection_http.rs:596-600` | `wamn-ctl bind-connection` |
| S13 | `wamn-attachment-kinds` | http · stream · cron attachment definitions, input schemas, ack and retry posture per kind, which kinds have no firer today | `catalog/model/src/lib.rs:550-630` | Gate |

Operational (verbs that already exist; golem's `troubleshoot-build`,
`view-agent-logs`, `debug-agent-history`, `rollback`, `local-dev-server`,
`profiles-and-environments` have these analogs):

| id | name | teaches | links | verb |
|---|---|---|---|---|
| S14 | `wamn-troubleshoot-stage` | per-stage failure kinds and fixed remedies; which stage owns which file; the dirty-worktree refusal | `dev.rs:313-323`, `DevRunError::remedy` | `wamn dev` |
| S15 | `wamn-environments` | org/project/env/schema/tenant, `wamn dev up` (when landed), `dev.json` fields, what is disposable | `dev/config.rs:523-`, `.10.32` | `wamn dev up` |
| S16 | `wamn-read-traces` | Tempo query URL, the per-invocation and per-effect spans, correlating a request id to a trace | `exe-model.md` observability, `.24.12` | Tempo |
| S17 | `wamn-dead-letters-and-effect-uncertain` | reading dead letters, the `EffectUncertain` state, terminalizing with an evidence basis | `run-state/src/operator_action.rs`, `status.rs` | `wamn-ctl dead-letters`, `terminalize-effect-uncertain` |
| S18 | `wamn-release-and-promote` | publish, promote, the wiring pointer flip and instant rollback, what a release pins | `.18.6`, `.25` | `wamn-ctl publish-release`, `promote` |

Experiment operators:

| id | name | teaches | links |
|---|---|---|---|
| S7 | `agent-pilot` | `up/launch/grade/down`, evidence layout, stall taxonomy, `human.json` | this spec, protocol §6–§10 |
| S8 | `agent-pilot-transcript-tagging` | tagging a stream transcript with stall categories and pointers; agent claim vs grader finding | protocol §7.3 |

Rule paragraphs the authoring skills link and that must exist first
(`docs/exe-model.md`): continuation-as-data, joins-as-data, multi-row invariants
in one transaction with a lock order, idempotency from the claim / `node-context`.

Skills arm order: S1–S6, S9 first (T1 needs nothing else); S10–S13 with T2;
S0 with T3 (intake is its own rung, kept out of the authoring arms so the
authoring variable stays clean); S14–S18 when the stall table shows operational
stalls. Nineteen for the product, two for the experiment, against golem's ~110.

What not to take from golem: its harness checks build, deploy and HTTP only. The
grader's law checks (A7) are the safety half and stay.

---

## V. Verification methods

What the specs require beyond the stage gates: deterministic simulation at the
layers that own wamn's guarantees, fuzzing of the pure parsers, machine checks
that a test set is not trivial, and an assertion ladder for invariants. Golem's
determinism is a by-product of its oplog; wamn gets it from seams that already
exist, without replay.

### V1. Deterministic simulation (DST)

| layer | seam | harness | invariants asserted |
|---|---|---|---|
| router walk | pure: `next(&mut walk, now_ms) -> Step`, `apply(outcome)` (`crates/execution/router/src/walk.rs:328, 425`) | scripted outcomes, controlled `now_ms`, property tests over random wirings and outcome sequences | termination under the hop limit; merge once per token; retry bounded; cycles refused; no invocation after Done |
| run-state decisions | "only decisions and parameterized SQL; Postgres, clocks, doorbells remain adapter effects" (`crates/execution/run-state/src/lib.rs:5-8`) | seeded scheduler over admissions, claims, crashes, lease expiry, janitor sweeps; statements against a throwaway PG; seed recorded, replayable | no lost run; at most one active lease per run; redelivery bounded by `max_attempts`; terminal rows immutable; janitor orphans exactly the exhausted |
| guest execution | WASI clocks and random sources; NaN canonicalization and deterministic relaxed-simd as engine flags | deterministic WASI context per test run; **probe**: whether wash-runtime 2.8.0 exposes the context build hook (the host never constructs one; no `WasiCtxBuilder` under `crates/platform/runtime`, `crates/execution/host`) | same inputs, same outputs, byte for byte |
| guest logic without a database (option, ruling needed) | a `wamn:postgres` plugin variant serving rows recorded from one live run; test-support only, not runtime capture | record once in the live gate, replay in unit tests and under mutation | guest behavior fixed to recorded responses; fixture re-recorded by the live gate on drift |

The run-state harness will supersede the one-off kill gate: crash points become
scheduler events, and the at-least-once proof falls out of the seed. That
harness is D2 in `docs/poc/deterministic-testing-spec.md`, Phase 2, gated on
the `$now` bead (D2b). Until D2 lands, the kill gate stays in this spec and
the pilot runs with it: one gate per ingress path, executor killed between
node invocations, redelivery to completion, the idempotent write absorbing
the duplicate. The handover is explicit: D2's exit gate closes the kill gate's
bead.

### V2. Fuzzing

Targets are pure and cheap; order by policy weight:

1. `crates/schema/generator/src/sql_lex.rs` (816 lines, two tests) — decides
   relation authority; a policy parser with two tests is the first target.
2. Manifest and wiring parsers (`wamn.json`, wiring and attachment JSON).
3. The route envelope parser (request array, `request_id`, typed items).
4. Node-error taxonomy mapping in the driver.

`proptest` on the stable pin now; libFuzzer on nightly only on its Phase 3
trigger (`deterministic-testing-spec.md` §0). Corpora committed; a crash is a
bead.

### V3. Test-set usefulness — machine checks

A test set (`steps.json` today, B4 cases later) is accepted only if it passes all
four; the grader and `up` enforce them:

- **Kill matrix.** Mutate the application, not the platform: drop the lock, drop
  the constraint, remove the replay branch, skip a status check, invert a
  permission. Every `must` step fails on at least one mutant; a step that kills
  none is refused. Extends the repository's exit-code mutation discipline
  (`docs/operations/build-and-test.md:313, 1665`) to authored packages.
- **Traceability.** Every pinned invariant in the brief carries an id; every
  `must` step names one (`"invariant": "<id>"`); an invariant with no step, or a
  step with no invariant, is refused at `up`.
- **Predicate floor.** A `must` step asserts a `value` field, an `error_code`, or
  an `equals`, never status alone; each command has at least one refusal-path
  step.
- **Executed and deterministic.** Zero executed steps is red (the self-skipping
  rule); the set runs twice against identical state and must agree.

### V4. Assertion ladder for invariants

Strongest first; a rule sits at the highest rung that can hold it:

1. Database constraints: CHECK, UNIQUE, FK, EXCLUDE, `row_version`, RLS. Hold
   under concurrency and across code versions. H-3 grades this rung.
2. Typed refusals for business rules (node-error taxonomy).
3. Generated contract tests: the generator knows the claim law and emits, per
   create-shaped command, the replay and changed-body cases. Every command gets
   both by construction; this is also the proof shape for `wamn-10yt.19` that
   needs no package to declare a create.
4. Invariant functions over pure state (`fn check(&Walk) -> Result<(), Violation>`;
   likewise for run-state decisions), run after every step in property tests and
   DST; optionally compiled into the pilot executor behind a feature as
   tripwires while agents run.
5. `assert!` in guest code only for should-never-happen. A trap becomes a node
   failure, redelivery repeats it, the delivery dead-letters: right for a broken
   invariant, wrong for a user error.

---

## B. Not required to run

| id | item | seam | parent (proposed) | arm |
|---|---|---|---|---|
| B1 | S1–S6 + S9 with the shape rule (procedure + one executed example, trigger descriptions, Related Skills), rule paragraphs, the `skills/` root with two symlinks; S10–S18 later | `skills/`, `.agents/skills` → `skills/`, `.claude/skills` → `skills/`, `docs/exe-model.md` | new epic `[AGENT-PILOT]` | skills |
| B2 | `wamn dev --format json` on the plain renderer: `DevRunReceipt`/`DevRunError`/`DevStageFailure` (`dev.rs:233-346`) as `{verb, stage, status, code, message, remedy, pointer}` | `wamn-10yt.10` (read seam `.10.26/.10.27`) | receipts |
| B3 | `wamn invoke --wiring <id> --payload <json> --path …` over the driver outcome (`router_driver.rs:916-923`) | `wamn-10yt.10` child | invoke |
| B4 | flow tests in the loop over `AuthoringCommand::TestSetRun` once `.8.5.4` lands; case types `single` · `redeliver` (same delivery twice, observable unchanged) · `concurrent` (N deliveries in parallel, exactly-one assertion); a set is accepted only under V3 (kill matrix, traceability, predicate floor, executed and deterministic); the test-set rename sweep | `wamn-0h0g.8.5` | flow tests |
| B5 | Rust harness replacing A3–A5; YAML scenarios; skill activation from `tool_use` blocks named `Skill` | `[AGENT-PILOT]` | consolidation |
| B6 | bounds: clamp `retry-after-ms` (`router_driver.rs:2737`, `walk.rs:499-502`); refuse over-budget retry at Gate; enforce or drop deadlines (`run-state.sql:459,492-493,541-542`); derive the HTTP idempotency key when `None` (`connection_http.rs:596-600`) | `.16`, `.19`, `.24` | safety |

---

## P. Fit with planned work

| item | existing | relationship |
|---|---|---|
| A0 | `wamn-10yt.10`, `.10.32` | same command surface; `.10.33`, after `.10.32` |
| A1 provider `dev-up`, identity parameters | `.10.32` | consumer; identity flags are a requirement on that bead |
| A1 base-only loop (F12) | `wamn-10yt.10` | one probe before greenfield tasks; a refusal is a finding on `.10` |
| statement verifier (F21) | `wamn-10yt.10`, `wamn-0h0g.22` | finding filed before any greenfield run: a generic verifier at Generate, or the loop names the gap; an agent hits it as `generator` |
| A6/A7 checks and fence reports | `wamn-10yt.19`, `.20` | claim replay is checked through the steps; `.20`'s fence is reported, not re-decided |
| B2 | `.10.26/.10.27`, `wamn-dggp` | plain-renderer serialization of the seam the console reads |
| B4 | `wamn-0h0g.8.5/.8.5.4` | consumer |
| S4 | `wamn-0h0g.13.7` | interim until the SDK skill |
| B6 | `.16`, `.19`, `.24` | three small beads |
| A1–A8 | `.11` proof floor | outside it by design; reuses its standup and verbs |
| all | open application lanes | disjoint files; shared machine only |

Nothing touches the deployment boundary, the durable tier, or RLS.

---

## X. Execution — one session, subagent lanes

One integrator session writes `main`. Parallel work runs as subagent lanes cut
from `origin/main` into worktrees; the integrator merges. The file partition
is what makes lanes safe, not the number of agents.

| lane | items | files | conflicts |
|---|---|---|---|
| L-hold | A0 as `.10.33` | `services/ctl/src/dev/command.rs`, one live test | `.10.32` edits the same file: sequenced after it, same integrator |
| L-pilot | A1, A3, A4, A5, A6 (schema and validation), A7, A8, S7, S8 | `tools/agent-pilot-*`, `docs/experiments/agent-authoring/` | none; all new files |
| L-rules | the four rule paragraphs (B1 prerequisite) | `docs/exe-model.md` | owner prose; lands whenever |
| L-runs | the protocol's runs | evidence directories only | serial on one machine; never with a cluster journey |

L-pilot is buildable now against `origin/main` with the `dev-env` provider and
the stub driver; A7 step 3 and the `--hold` line of any brief wait for L-hold
to land. L-runs opens when L-hold is on `main` and L-pilot has merged.

Relay points, all inside one session: `.10.32` lands → L-pilot switches the
default provider, L-hold files `.10.33`; A0 on `main` → L-pilot unmasks A7 step
3; L-pilot merged → L-runs; stall table filed → B items open, one bead each.

Bead filing (proposed): epic `[AGENT-PILOT] agent-authoring tooling` with
A1–A8, S7, S8, later B1, B5. A0 as `wamn-10yt.10.33`. B2, B3 as `.10.x`. B4 under
`.8.5`. B6 as three beads under `.16`, `.19`, `.24`.

---

## Sequence and size

A0 (small) ∥ A6 schema (small) → A1 (medium, ~300 lines bash) with A3/A4/A5
(small) → A7 (medium) → A8 (small) → T pure tests → S7/S8/S9 → selftest run `000`
→ the statement-verifier and base-only probes (F12, F21) → the protocol's runs.

## Rulings requested

1. A0 as specified, `.10.33` after `.10.32`.
2. Identity parameters added to `.10.32`'s scope.
3. The task interface (A6) as the only application-shaped thing in the tooling.
4. `bd` off the agent's `PATH`; global skill inventory frozen across runs.
5. Bead filing and lane partition as in X.
6. The statement-verifier gap (F21) filed as a finding before greenfield runs; whether a generic verifier at Generate is in scope for `.10` or waits on `.22`.
7. V1's recorded-response `wamn:postgres` test plugin: in scope as test-support, or refused as capture by another name.
8. Generated contract tests (V4 rung 3) as the proof shape for `wamn-10yt.19`.
