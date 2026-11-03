# Web boundaries

This report covers epic `wamn-f94j3`, audit group 4. It takes rows H3-1, H3-3, H3-4, H3-6, H3-8 and H3-9 of [hygiene](hygiene.md) Part 3 and row A2-9 of [code quality](code-quality.md).
The branch is `worktree-web-r2`, on main `003eccd57`.
The epic also prepares the ui-split track. `web/shell/src/sync.tsx` and `status.ts` stay for that track.

## Issues

| Issue | Audit reference | Commit | Result |
| --- | --- | --- | --- |
| `.1` The root install applies the wfe patches | H3-1 | `1d53ae609` | The root `pnpm-workspace.yaml` declares the two patches from `web/wfe/patches`. The root lockfile resolves the patched versions with the patch digests of the wfe lockfile. |
| `.2` Public entries and workspace dependencies | H3-3 | `90d504549` | `@wamn/platform-ui` and `@wamn/vite` declare an entry. Consumers declare `workspace:*` dependencies. The source aliases, the matching tsconfig paths and the Receiving import of the private wfe `vendor-transforms` script go. |
| `.3` web/shell owns the Vite policy | A2-9, H3-4, H3-8 | `6f7d217a7` | `solidApplication` supplies the Solid compiler, resolution, dedupe, one Solid version check and the test defaults. `workerBuild` names the worker `sw.js`. `@wamn/vite/bundle` holds the one bundle check, and it now parses the Receiving worker. |
| `.4` One lint policy, and removable exports go | H3-6, H3-9 | `fe9661d4d` | One ESLint policy covers all authored web code and ignores the three generated client directories. `web/demo`, two export modifiers, the `shellStatus` re-export and the root `@wfe/generator` dependency go. |

## Acceptance

1. Issue 1: before the change, the wfe patch check fails on the root install, and two of three wfe focus tests fail. After it, both pass. The kept test `web/shell/test/wfe-patches.test.ts` fails 3 of 3 without the patches and passes 3 of 3 with them.
2. Issues 2, 3 and 4: the built platform, Receiving and sync-bench outputs are byte-identical across all three issues, 29 files each. The worker `sw.js`, the precache lists and the asset names do not change.
3. Issue 3: if the expected Solid version differs, the shared Solid check stops all three builds.
4. Issue 4: platform, Receiving, sync-bench and shell files get the same 20 Solid rules, and the generated client files are ignored. Lint reports 0 errors.

## Decisions

The owner gave these answers.

1. Cargo builds that only prepare test inputs are allowed. The no-build rule bars platform images.
2. The four browser tests run by name once at epic close. `wamn-w3fft` asks the tiers owner to select them for a web diff.
3. The focus check is a kept test in `web/shell`.
4. The installed-byte check proves the scroll patch. A behavior test for it belongs in the wfe repository.
5. `wamn-p7ey4` is a main defect, so it does not block the merge. The fix of `wamn-p7ey4` runs the four browser tests by name and records the result on `wamn-f94j3`.

Root made these decisions inside the brief.

1. `apps/platform_ui` is the platform web application, and the audit rows name its files, so issues 2 to 4 edit its web files.
2. The root `package.json` changes in one line, because the brief names its `@wfe/generator` dependency.
3. The tsconfig `customConditions` `wfe-source` stays. It is a condition of the wfe package exports, and tsc reads the `types` condition first.
4. The one Solid check allows `web/shell`, because Receiving and sync-bench bundle `web/shell/src/sync.tsx` until the ui split. sync-bench gets the check too.
5. Each application bundle test calls the shared check, so the web suite still runs the platform case.
6. The 28 new Solid warnings in the platform UI stay warnings. Lint passes with 72 warnings, 44 before.

## Tests

The logs are in `/var/tmp/wamn-web-log`.

1. Each issue ran the check, test and build scripts of `@wamn/platform-ui`, `@wamn/receiving-web`, `@wamn/sync-bench`, `@wamn/vite` and `@wamn/web-runtime`, and the root `pnpm run check`.
2. `tools/test-changes --base origin/main --suite web run` passed on issue 1 and on the branch head.
3. `check_client_components` type-checks 7 generated modules against `@wfe/generator` after the root dependency goes.
4. The four browser tests ran by name on `0e1baf3a4`, the same patch before the rebase. All four stop in fixture setup, before any browser step, because the identity-reader grant check refuses the database. The branch changes no Rust, SQL or deploy file, so main fails the same way. This is `wamn-p7ey4`.

## Follow-up

1. `wamn-p7ey4`: the local browser fixtures refuse the identity-reader grant set.
2. `wamn-w3fft`: a web diff does not select the browser suites.
3. `wamn-upv85`: the platform administration test times out under machine load, on main and on the branch alike.
4. wfe: the scroll patch has no behavior test in the wfe repository (noted on `wamn-f94j3.1`).
