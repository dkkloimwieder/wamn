You are working in a git worktree of the wamn repository, detached from main, on
a disposable development environment. Nothing you do here reaches main or any
shared service. Read AGENTS.md first.

TASK: SCENARIO.md in "$WAMN_PILOT_TASK_DIR" describes what to build, in domain
terms. Build it as a wamn package.

DONE MEANS ALL OF:
1. `wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root <overlay_root>` completes
   every stage through Activate, where <overlay_root> is the path named in
   "$WAMN_PILOT_TASK_DIR/task.json". Your package lives there; create it if it
   does not exist.
2. You have exercised every operation the scenario names against the running
   release and recorded the exact requests and responses in your report.
3. REPORT.md exists at "$WAMN_PILOT_RUN_DIR/REPORT.md" in the format below.

WHAT IS TRUE OF THIS LOOP:
- Publish through Activate refuse a worktree with uncommitted or untracked
  changes. Commit locally first. Never push.
- `wamn dev … --hold` runs once and keeps the activated release reachable until
  you stop it; it prints `run served: <base_url> host=<route_host>`. Send requests
  to <base_url> with `Host: $WAMN_ROUTE_HOST` and `Authorization: Bearer <token>`,
  where <token> is `.stringData.token` in "$WAMN_ROUTE_CALLER_PAT_FILE".
- There is no reference data. Create it through your own operations.

CONSTRAINTS:
- Create or edit files only under the paths named in
  "$WAMN_PILOT_TASK_DIR/task.json" `allowed_paths`. Nothing else in the
  repository is yours to change. If the task cannot be completed inside those
  paths, say so in the report and stop.
- Do not push, do not create remote branches, do not create or edit beads.
- Do not weaken or bypass any permission, policy, RLS, or gate to make a stage pass.
- Follow the repository's naming and versioning rules. No version suffixes.
- After three attempts at the same failure, stop and write the blocker in the
  report. Do not work around the platform.
- Nobody will answer questions. Decide, record the decision, continue.

TOOLS: `wamn` and `wamn-ctl` are on PATH. `curl` reaches the route host.
Postgres is reachable at the URLs in "$WAMN_DEV_CONFIG" for inspection only.

REPORT.md (these headings, this order):
# Summary · # Changes · # How I verified · # Decisions · # Where I got stuck
# Rules I relied on · # Open questions

"How I verified" must show, with commands and outputs verbatim: your component's
own tests; the loop; every operation the scenario names exercised against the
running release; and what you did not verify, and why.
