# 012-claude-dock-appointments

e3b1449e9816342545764f00184a8283814f7c19 · claude-opus-5[1m] · load 1.00 0.69 0.78 · standup dev-env · output style simple-english:simple-english · run cap hit: no · skills present: 15 skills, frozen against run 001 (skills.json); none is a wamn authoring skill

Outcome: PASS
Conduct: Two stalls rather than three, and it never hit the dirty-worktree refusal. It flagged the EXCLUDE trade under Decisions in its own report without being asked, and drove five concurrent overlapping pairs rather than the one the fixture names.

Q1 first green: 21 min
Q2 wamn dev runs: 6 · failed: 2 · held: 1
Q3 reads before first edit: design-doc 8, code 169, generated 5, skill 0 (approximate: the driver used only Bash, so the classes count the paths cited in the 125 commands before the first file it created, of 192 commands in the run)
Q4 verification: curl: every operation the scenario names was driven against the held release over HTTP · Q5 outside allowed paths: 0 · Q6 output tokens 142627, cost USD 28.95, model claude-opus-5[1m], 193 turns, 32 min
Q7 stalls: [{"category":"generator","minutes":0.7,"note":"Introspect refused DEFAULT 'scheduled'::text on the appointment status column; the whole text default vocabulary is three words","pointer":"wamn-frru"},{"category":"env","minutes":0.9,"note":"Build refused: the target directory is set to an empty string in CARGO_TARGET_DIR, which the runner exports empty instead of unsetting","pointer":"wamn-nvbd.8"}]
Q8 law violations: [] · Q9 over-claims: []
Q10 skills activated: []
Q11 verification coverage: operations driven 5/5; all four S9 cases appear, but NONE unprompted: steps.json sits in the agent's own task directory and every run read it (wamn-nvbd.9)

## Machine checks

- loop: pass (12 stages)
- paths: pass
- teardown: verification database removed: true

### Steps

- create-carrier (must, DOCK-0): pass — status=200 present:carrier_id=15bed841-9e0e-43ba-ace5-ce439d554c22
- create-dock (must, DOCK-0): pass — status=200 present:dock_id=6f5531ed-857d-4fba-a827-7907362c96c8
- book-first (must, DOCK-2): pass — status=200 present:appointment_id=606ed30a-2751-48a1-9608-982b2f675ddd present:status=scheduled
- book-replay (must, DOCK-2): pass — status=200 appointment_id=606ed30a-2751-48a1-9608-982b2f675ddd vs 606ed30a-2751-48a1-9608-982b2f675ddd
- book-changed-body (must, DOCK-3): pass — status=200 error_code=idempotency_conflict
- overlap-a (recorded, DOCK-1): pass — status=200 present:appointment_id=c02b4caf-5013-437b-ae62-48e07d821fff
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

Raw: `012-claude-dock-appointments/`

