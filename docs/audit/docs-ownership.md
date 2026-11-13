# Documentation ownership

This report covers epic `wamn-ggdxf`, which is audit hygiene Part 2 (H2, see [hygiene](hygiene.md)).
The branch is `worktree-docs`, rebased onto main `779cbf125`.

## Issues

| Issue | Audit reference | Commit | Result |
| --- | --- | --- | --- |
| `.1` Completed plans out | H2-2, H2-3 | `64933dbf8` | Ten completed plans and `docs-audit.md` are deleted. `bd show` confirmed each bead closed. Seven mixed plans keep their unresolved design and every rule that no reference page states. |
| `.2` One reference owner | H2-1, H2-4 | `82df826ce`, `fe812ec01` | The standing rules of `platform-deploy.md` are in `CLAUDE.md`, the platform version procedure is in `operations/building.md`, and the cloud prerequisites are in `operations/gcp.md`. Rules of built work moved to eleven architecture and operations pages. `platform-deploy.md` and `integrations.md` are deleted. |
| `.3` Links and evidence | H2-5, H2-6, H2-7 | `93926c34b` | The five `wamn-sync.md` links and all other missing local links resolve. Twelve evidence links are removed, and each statement keeps its bead and the note "evidence not retained". The three snapshot pages are deleted, and git holds them at `82df826ce`. |
| `.4` Navigation | H2-8 | `7821b47d8` | The plan index lists all sixteen retained plans. `docs/README.md` names `audit/`. The history index maps every page to its successor, bead or revision. |
| `.5` Auth cache ignores | `wamn-kov6v.6` part | `31d54aa71` | The three `auth_cache.rs` cases use `requires: env:WAMN_NATIVE_C_NATS_BIN` and test it before setup. Their exclusion rows are removed. |

The branch deletes 15 files and changes 118 files, with 718 lines added and 6,842 lines removed.

## Decisions

Root took these decisions inside the brief.

1. `platform-deploy.md` was deleted in issue 2 and not in issue 1, because its standing rules had to move first.
2. The first cut of the mixed plans removed rules that no reference page stated. Root sent the lane back, and the lane restored each rule word for word. Issue 2 then moved the rules of built work to their owners.
3. The ten deleted plans held 193 rules that no reference page stated. A word-for-word test after issue 2 found 67 in owner pages. The lane resolved the other 126 against the source: 11 copied to owners, 19 already stated with other words, 66 contradicted by current code or a later ruling, and 31 undecided. The table is [raw/docs-ownership-rules.md](raw/docs-ownership-rules.md).
4. The dated audit reports under `docs/audit/` keep the retired tool names and line pins. They are records, like quotations in `docs/history`. Root reverted the lane's renames there.
5. Comment-only edits in the `web/wfe` submodule were reverted. That repository is outside this epic.
6. `docs/plan/platform-ui-app.md` keeps server paging as written. Its bead `wamn-n8q9m.20` is closed (2026-10-23), but the brief names it as kept.

## Rule comparison

The moved standing rules are in `CLAUDE.md` under "Standing repository rules". Each rule matches its source word for word, except for self-references. For example, "a §2 invariant" became "a plan invariant" and "§17" became "the owning plan". The lane report lists each change: `/var/tmp/wamn-docs/one-owner-report.txt`.

## Test results

- Each issue: the link scan `/var/tmp/wamn-docs/linkscan.py` found no new missing link. After issue 3 it found zero missing paths and anchors outside `vendor/`. The baseline was 61. The 13 that remain are in `vendor/pg-walstream/README.md`, which is third-party.
- Issue 5: `cargo test -p wamn-runtime --lib auth_cache -- --ignored` passed 3 of 3 cases. `test_tiers` passed 5 of 5 after its protected count changed from six to three.

The logs are in `/var/tmp/wamn-docs`. The runs built no image and did not use kind.

## Owner rulings

The owner accepted the epic on 2026-11-02 and ruled on the open items.

1. The two runtime strings that cited deleted pages now cite their owners: `tools/repo-lint-dependencies:63` cites `architecture/naming.md`, and `crates/control/lib/src/platform/steps.rs:329` cites `operations/gcp.md` section 2.
2. The 31 undecided rules (D) stay as recorded. They are not copied into any page. Git holds them at `40be5b96d`, and a person who needs one files a bead.
3. The 66 contradicted rules (C) get no beads. The resolution table is the list for a later owner pass. One bead covers the teardown example: `wamn-e70ys`.
4. The rule "An epic is at most three issues" is deleted from `CLAUDE.md`. It contradicts the 2026-09-25 ruling that an instruction can target more work.

## Open items

- `wamn-kov6v.6`: the three ignores in `platform/broker.rs` and `platform/edge.rs`. `wamn-pvsoo.1` step B landed at `6382199f3`. The change waits for an owner brief, and then the `test_tiers.rs` count becomes zero.
- `wamn-e70ys`: a refusal after uninstall leaves earlier environment delete steps applied.
