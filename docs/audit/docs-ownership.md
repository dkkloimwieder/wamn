# Documentation ownership

This report covers epic `wamn-ggdxf`, which is audit hygiene Part 2 (H2, see [hygiene](hygiene.md)).
The branch is `worktree-docs`, on main `40be5b96d`.

## Issues

| Issue | Audit reference | Commit | Result |
| --- | --- | --- | --- |
| `.1` Completed plans out | H2-2, H2-3 | `2329b9d54` | Ten completed plans and `docs-audit.md` are deleted. `bd show` confirmed each bead closed. Seven mixed plans keep their unresolved design and every rule that no reference page states. |
| `.2` One reference owner | H2-1, H2-4 | `bdc8c5530` | The standing rules of `platform-deploy.md` are in `CLAUDE.md`, the platform version procedure is in `operations/building.md`, and the cloud prerequisites are in `operations/gcp.md`. Rules of built work moved to eleven architecture and operations pages. `platform-deploy.md` and `integrations.md` are deleted. |
| `.3` Links and evidence | H2-5, H2-6, H2-7 | `12ec0f74c` | The five `wamn-sync.md` links and all other missing local links resolve. Twelve evidence links are removed, and each claim keeps its bead and the note "evidence not retained". The three snapshot pages are deleted, and git holds them at `bdc8c5530`. |
| `.4` Navigation | H2-8 | `875753ec9` | The plan index lists all sixteen retained plans. `docs/README.md` names `audit/`. The history index maps every page to its successor, bead or revision. |
| `.5` Auth cache ignores | `wamn-kov6v.6` part | `feaf20322` | The three `auth_cache.rs` cases use `requires: env:WAMN_NATIVE_C_NATS_BIN` and check it before setup. Their exclusion rows are removed. |

The branch deletes 15 files and changes 118 files, with 718 lines added and 6,842 lines removed.

## Decisions

Root took these decisions inside the brief.

1. `platform-deploy.md` was deleted in issue 2 and not in issue 1, because its standing rules had to move first.
2. The first cut of the mixed plans removed rules that no reference page stated. Root sent the lane back, and the lane restored each rule word for word. Issue 2 then moved the rules of built work to their owners.
3. The ten deleted plans held 193 rules that no reference page stated. A word-for-word check after issue 2 found 67 in owner pages. The lane resolved the other 126 against the source: 11 copied to owners, 19 already stated with other words, 66 contradicted by current code or a later ruling, and 31 undecided. The table is [raw/docs-ownership-rules.md](raw/docs-ownership-rules.md).
4. The dated audit reports under `docs/audit/` keep the retired tool names and line pins. They are records, like quotations in `docs/history`. Root reverted the lane's renames there.
5. Comment-only edits in the `web/wfe` submodule were reverted. That repository is outside this epic.
6. `docs/plan/platform-ui-app.md` keeps server paging as written. Its bead `wamn-n8q9m.20` is closed (2026-10-23), but the brief names it as kept.

## Rule comparison

The moved standing rules are in `CLAUDE.md` under "Standing repository rules". Each rule matches its source word for word, except for self-references. For example, "a §2 invariant" became "a plan invariant" and "§17" became "the owning plan". The lane report lists each change: `/var/tmp/wamn-docs/one-owner-report.txt`.

## Test results

- Each issue: the link scan `/var/tmp/wamn-docs/linkscan.py` found no new missing link. After issue 3 it found zero missing paths and anchors outside `vendor/`. The baseline was 61. The 13 that remain are in `vendor/pg-walstream/README.md`, which is third-party.
- Issue 5: `cargo test -p wamn-runtime --lib auth_cache -- --ignored` passed 3 of 3 cases. `test_tiers` passed 5 of 5 after its protected count changed from six to three.

The logs are in `/var/tmp/wamn-docs`. The runs built no image and did not use kind.

## Open items

- `wamn-kov6v.6`: the three ignores in `platform/broker.rs` and `platform/edge.rs`. This session makes that change after `wamn-pvsoo.1` step B lands, and then the `test_tiers.rs` count becomes zero.
- Two runtime strings still cite deleted pages: `tools/repo-lint-dependencies:63` cites `platform-deploy.md §9.2`, and `crates/control/lib/src/platform/steps.rs:329` cites a snapshot page. The brief allowed comment changes only.
- 31 undecided rules from deleted plans, listed as D in the resolution table. Git holds their text at `40be5b96d`.
- 66 rules that current code contradicts, listed as C. Some of them can be code defects and not retired rules. An example is `environment-teardown.md:75`: a refusal after uninstall can leave earlier delete steps applied (`crates/control/lib/src/environment/delete.rs:179`).
- The moved rule "An epic is at most three issues" in `CLAUDE.md` conflicts with this five-issue epic. The owner decides whether the rule stays.
