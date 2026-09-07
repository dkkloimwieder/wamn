# 011-claude-dock-appointments

7a9ab667424a8be93cc17582f6854d030c15b88b · claude-opus-5[1m] · load 0.67 0.85 1.23 · standup dev-env · output style simple-english:simple-english · run cap hit: no · skills present: 15 skills, frozen against run 001 (skills.json); none is a wamn authoring skill

Outcome: PASS
Conduct: The cheapest and fastest of the three. Wrote nothing for its first 99 commands, then reached first green in 22 minutes. It is the run that named the EXCLUDE limit explicitly and explained the row lock it used instead, which is wamn-yk9l.

Q1 first green: 22 min
Q2 wamn dev runs: 6 · failed: 3 · held: 1
Q3 reads before first edit: design-doc 12, code 129, generated 9, skill 0 (approximate: the driver used only Bash, so the classes count the paths cited in the 99 commands before the first file it created, of 164 commands in the run)
Q4 verification: curl: every operation the scenario names was driven against the held release over HTTP · Q5 outside allowed paths: 0 · Q6 output tokens 131365, cost USD 25.10, model claude-opus-5[1m], 165 turns, 29 min
Q7 stalls: [{"category":"generator","minutes":0.6,"note":"Introspect refused DEFAULT 'scheduled'::text on the appointment status column; the whole text default vocabulary is three words","pointer":"wamn-frru"},{"category":"component-build","minutes":5.5,"note":"Virtualize refused: the socket component had no matching imports for the plugs provided, profile std-empty-environment","pointer":"runs/011-claude-dock-appointments/dev-logs/004"},{"category":"rule-unknown","minutes":0.3,"note":"Apply refused with dev-worktree-dirty; the loop requires a committed worktree and nothing the agent read said so","pointer":"runs/011-claude-dock-appointments/dev-logs/005"}]
Q8 law violations: [] · Q9 over-claims: []
Q10 skills activated: []
Q11 verification coverage: operations driven 5/5; all four S9 cases appear, but NONE unprompted: steps.json sits in the agent's own task directory and every run read it (wamn-nvbd.9)

## Machine checks

- loop: pass (12 stages)
- paths: pass
- teardown: verification database removed: true

### Steps

- create-carrier (must, DOCK-0): pass — status=200 present:carrier_id=0d1ff4e0-6c76-4bac-bd24-b9544fa32951
- create-dock (must, DOCK-0): pass — status=200 present:dock_id=9f2fe11a-903d-499d-bf01-fb2d3e3b9114
- book-first (must, DOCK-2): pass — status=200 present:appointment_id=0bf374e3-1731-4e0d-8eee-bba3c4c30e06 present:status=scheduled
- book-replay (must, DOCK-2): pass — status=200 appointment_id=0bf374e3-1731-4e0d-8eee-bba3c4c30e06 vs 0bf374e3-1731-4e0d-8eee-bba3c4c30e06
- book-changed-body (must, DOCK-3): pass — status=200 error_code=idempotency_conflict
- overlap-a (recorded, DOCK-1): pass — status=200 present:appointment_id=a6d1c10e-429a-40bb-97b4-40ecb879bc2b
- overlap-b (recorded, DOCK-1): FAIL — status=200 no value item
- overlap-refuses-exactly-one (must, DOCK-1): pass — concurrent refusals=1 expected=1 code=slot_unavailable
- check-in (must, DOCK-4): pass — status=200 arrived_at=2026-10-01T09:07:00Z want=2026-10-01T09:07:00Z status=arrived want=arrived
- check-in-unknown (must, DOCK-5): pass — status=200 error_code=not_found
- list-one-dock-one-day (must, DOCK-6): pass — status=200 sorted_by:appointments.slot_start=true

### Checks with no fence in the loop

- claim-replay: no replay step in the fixture
- row_version: pass
- naming: pass

### Fence verdicts, reported not re-decided

- capability-surface (Admit): Admit passed
- additive-migration (Migrate): Migrate passed
- no-environment-data (Admit): unfenced

## Rubric

- E1: 4 - rows and operations only, twelve stages through Activate, no state outside the database
- E2: 4 - a command table per create, identities minted on the claim row, no time or random source
- E3: 4 - zero paths outside the allow-list; the column-default wall was worked AROUND, not cleared by editing the platform
- E4: 4 - every must step reproduces, and the platform limits it could not clear are named in its own report
- E5: 4 - each refusal cleared in one or two attempts, no action repeated three times
- E6: 4 - naming and row_version checks pass, the migration is additive, and the concurrency rung it chose is the one the platform actually offers
- E7: 4 - REPORT.md carries verbatim requests and responses and names each platform refusal, so the beads above were opened from it without the transcript

Raw: `011-claude-dock-appointments/`

