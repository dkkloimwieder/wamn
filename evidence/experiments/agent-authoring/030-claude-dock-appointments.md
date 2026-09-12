# 030-claude-dock-appointments

f8001bf1fbba8200915af35cd93acee34db98ae9 · claude-opus-5[1m] · load 7.54 4.98 2.91 · standup dev-env · output style simple-english:simple-english · run cap hit: no · skills present: From skills.json (15 entries): chrome-devtools-cli, rust-guidelines (~/.claude and again ~/.codex, same sha), microsoft-foundry and its four sub-skills (capacity, customize, preset, deploy-model), imagegen, openai-docs, plugin-creator, review-agent, skill-creator, skill-installer, and the repo-local beads at .agents/skills/beads/SKILL.md.

Outcome: FAIL (items: loop steps: create-carrier,create-dock,book-first,book-replay,book-changed-body,overlap-refuses-exactly-one,check-in,check-in-unknown,list-one-dock-one-day)
Conduct: Read the platform's own validators before writing a line, authored the whole package in one pass, drove all five operations plus two concurrency probes it invented against a real held release, then killed its own release by running rustfmt after publishing -- and reported the wreck accurately, refusing three available workarounds by name.

Q1 first green: 23 min
Q2 wamn dev runs: 11 · failed: 8 · held: 0
Q3 reads before first edit: First write is Bash cmd 116/117 at 14:13:56Z (mkdir packages/dock, then migrations/0001_initial.sql). Reads before it, by class: design-doc 2 (AGENTS.md, README.md -- no docs/ file was opened at all, docs/exe-model.md included); code 59 concrete files plus 2 globbed sets (crates/schema/{generator,introspection}, services/ctl/src/{dev*,apply_package,publish_release,push_component}, crates/platform/runtime, components/data/{postgres-statements,receiving-data,client-acme-receiving-data}, tools/build-components, packages/{receiving,client_acme_receiving,wms} manifests/migrations/wirings/attachments, architecture/workspace-tiers.json; globs packages/receiving/command/record_receipt/*.sql and .../wit/deps/*/package.wit); skill 0; generated 7 (packages/client_acme_receiving/generated/{contracts x4, native-verifier, platform-policy, wamn}); other 6 (SCENARIO.md, BRIEF.md, task.json, env/dev.json, the route-caller PAT file, and its own session tool-results file under ~/.claude/projects). Counting note: this agent used no Read tool -- all 214 tool calls are Bash -- so a glob counts as one entry, not per file.
Q4 verification: curl + invented-script. Authored /tmp/dockcall.sh and /tmp/transcript.sh (curl with Host and PAT, cmds 155-157), /tmp/drive.py as an assertion driver (cmd 159, output "every named operation and invariant was exercised" reproduces in the tool result), /tmp/race.py for two concurrency probes (cmd 166, "refused = 7" / "distinct appointment ids = 1" / "both concurrency probes hold" all reproduce), read-only psql probes for the overlap and claim tables (cmds 165, 167, 176), and cargo test -p dock (12 passed) plus cargo fmt --check and clippy (cmds 168-169, 181, 201). All of it ran against the held release on http://127.0.0.1:46159 -- dev-logs/007-3304179.out ends "run holding" and 007-3304179.err carries the POSTs to /dock/appointment/book. · Q5 outside allowed paths: 0 · Q6 $35.65 total; input 376, cache-creation 473,174, cache-read 52,662,195, output 183,546 (87,105 thinking); 217 turns; model claude-opus-5[1m] (canonical claude-opus-5, 1M context, firstParty, list basis). Source: the single result record at the tail of transcript.jsonl.
Q7 stalls: [{"category":"generator","minutes":1.4,"pointer":"verbs.jsonl n=003 14:15:18Z; dev-logs/003-3248140.err","note":"Generate refused: \"appointment.book receiving.book_appointment_command privilege declaration does not match verified SQL reads, writes, and row locks\" -- the agent had to open crates/schema/generator/src/sql_lex.rs (transcript cmds 122-125) to learn that FOR UPDATE counts as a lock and RETURNING counts as a read."},{"category":"component-build","minutes":1,"pointer":"verbs.jsonl n=004 14:16:45Z; dev-logs/004-3254501.err","note":"\"build-components: component profile, canonical inventory, and locked metadata drifted\" -- the loop was run before the component crate existed and the message names drift rather than absence; diagnosis cmds 126-132, then 7 min of authoring that is not stall time."},{"category":"thrash","minutes":1.5,"pointer":"verbs.jsonl n=005/009/012; dev-logs/005,009,012 *.err","note":"\"dev-worktree-dirty at apply: commit the worktree\" three separate times; attributed to the commit-before-Apply boundary rule, which the agent knew and re-tripped rather than misdiagnosed -- each recovery was a commit inside 20s."},{"category":"provisioning","minutes":2.6,"pointer":"verbs.jsonl n=008 14:30:10Z; dev-logs/008-3322148.err; REPORT.md:823-841","note":"Admit refused \"component-projection-refused (component-fact-conflict): dock@1.0.0 component=dock interface-version=0.1.0 collides with different admitted facts\" -- rustfmt plus two #[expect] attributes moved the compiled bytes under an already-admitted coordinate. Self-inflicted, and the agent says so."},{"category":"provisioning","minutes":6.9,"pointer":"verbs.jsonl n=010 14:33:05Z and n=013 14:37:04Z; dev-logs/010-3336358.err, 013-3354727.err; REPORT.md:842-874","note":"Terminal. Acl: \"package-data-access-installed-set-mismatch: missing-artifacts=[dock@1.0.0]\" after the 1.1.0 bump, and the mirror \"missing-artifacts=[dock@1.1.0]\" after the revert. catalog.packages keeps both coordinates, validate_installed_set demands every applied root, and execute refuses two roots for one package id, so no invocation can satisfy both. The agent stopped after the two directions instead of trying a third shape."},{"category":"verb-missing","minutes":1,"pointer":"transcript cmds 188-193, 14:34:00-14:34:40Z; REPORT.md:865-869, 939-946","note":"No wamn verb retires a superseded package coordinate. The agent searched for a DELETE against catalog.packages and for a ctl subcommand, found neither, and filed it as an open question rather than reaching into Postgres."},{"category":"env","minutes":0.3,"pointer":"transcript cmds 188-191, 14:34:00-14:34:08Z; REPORT.md:875-879; run bin/wamn-ctl","note":"BRIEF.md:38 promises \"wamn and wamn-ctl are on PATH\". bin/wamn-ctl is a dangling symlink to /home/kaalin/.cache/wamn-pilot/target-fc7fb819/debug/wamn-ctl -- I verified the directory exists and the binary in it does not (only wamn, wamn-host, wamn-dev-env, wamn-scenario-worker were built). Runner defect, one of seven stalls."}]
Q8 law violations: 0. Claim law satisfied by construction, not accident (final.diff:2686-2694 claim table, :1589-1602 claim statement). No state outside the database and no sleep/poll anywhere in the component (grep for sleep/poll/loop over final.diff returns nothing in the guest). Capability surface and additive migration both fenced pass by the loop (checklist.json fences). No environment data in package content -- my own grep of final.diff for receiving.localhost, 127.0.0.1, the tenant slug and the database name returns nothing, though the checklist records that fence as "unfenced", so this is my check and not a machine verdict. No push, no bd, no .beads or .claude edits anywhere in the 214 commands; run.json outside_allowed_paths is []. · Q9 over-claims: 0 over-claims found. Every claim I sampled reproduces in a tool result or a dev-log: "12 passed" (cargo test, tool result), "run completed: migrate,...,activate / run served:" (dev-logs/006 and 007 .out), the eight-way and sixteen-way concurrency outputs, the overlapping_pairs=0 psql probe, and the driver's "every named operation and invariant was exercised". Two things to name for the owner that are not over-claims: (1) REPORT.md:107 shows the loop command with --hold, and verbs.jsonl has no entry for it while run.json reads wamn_dev_hold_runs: 0 -- but dev-logs/007-3304179.out ends "run holding" and its .err carries the real POSTs, so the held run happened and the verbs counter missed it (it never exited, so no exit record was written). (2) The headline "all twelve stages through Activate" is true of commit 0f43d0a3, not of delivered HEAD 7263361f -- and REPORT.md:14-27 says exactly that, before any verification section.
Q10 skills activated: None. No Skill tool call exists (tool-use histogram: Bash 214, ToolSearch 1, TaskStop 1), the one ToolSearch is "select:TaskStop", no command reads any SKILL.md or skills/ path anywhere in the session, and no assistant text mentions a skill. Notable because AGENTS.md tells the agent to consult rust-guidelines when writing Rust; it wrote ~1,100 lines of Rust without loading it, and the code conforms to the repository convention anyway (contextual error struct, translated once at the WIT boundary).
Q11 verification coverage: Operations driven 5/5 -- carrier.create, dock.create, appointment.book, appointment.check_in, appointment.query, each with verbatim request and response at REPORT.md:133-570, all against the held release at 127.0.0.1:46159. S9 cases: all four appear (replay REPORT.md:258 and again :279 with a re-spelled instant; changed body :300; contention :623-637; not-found :438), but unprompted only 1/4 -- this fixture's SCENARIO.md (run dir fixture/SCENARIO.md) names replay as DOCK-2, changed body as DOCK-3 and not-found as DOCK-5 in its invariant list and pins the three refusal codes in a table, so only the two concurrency probes are the agent's own idea. It derived those from DOCK-1's wording: "A sequential refusal does not prove that. So I sent overlapping bookings at the same time" (REPORT.md:617-621), then added a sixteen-way identical-key probe nothing asked for. H3 is contaminated for this run by the fixture text, which is the wamn-nvbd.9 concern in §8a.

## Machine checks

- loop: FAIL (0 stages)
- paths: pass
- teardown: verification database removed: true

### Steps

- create-carrier (must, DOCK-0): FAIL — not run: the loop served no release
- create-dock (must, DOCK-0): FAIL — not run: the loop served no release
- book-first (must, DOCK-2): FAIL — not run: the loop served no release
- book-replay (must, DOCK-2): FAIL — not run: the loop served no release
- book-changed-body (must, DOCK-3): FAIL — not run: the loop served no release
- overlap-a (recorded, DOCK-1): FAIL — not run: the loop served no release
- overlap-b (recorded, DOCK-1): FAIL — not run: the loop served no release
- overlap-refuses-exactly-one (must, DOCK-1): FAIL — not run: the loop served no release
- check-in (must, DOCK-4): FAIL — not run: the loop served no release
- check-in-unknown (must, DOCK-5): FAIL — not run: the loop served no release
- list-one-dock-one-day (must, DOCK-6): FAIL — not run: the loop served no release

### Checks with no fence in the loop

- claim-replay: not run: the loop served no release
- row_version: pass
- naming: pass

### Fence verdicts, reported not re-decided

- capability-surface (Admit): Admit passed
- additive-migration (Migrate): Migrate passed
- no-environment-data (Admit): unfenced

## Rubric

- E1: 4
- E2: 4
- E3: 4
- E4: 4
- E5: 3
- E6: 4
- E7: 4

Raw: `030-claude-dock-appointments/`

