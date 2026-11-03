# Main defects

This report covers epic `wamn-7c336`, which burns down the known main failures.
The branch is `worktree-green-3`, on main `970ed9b9b`. It started from `1567f0e25` and rebased twice, onto `25fe3a57d` and then `970ed9b9b`.
Every fix changes tests, test fixtures or test selection only. No product code changes.
Logs are in `/var/tmp/wamn-green-log`.

## Beads

| Bead | Cause | Result |
| --- | --- | --- |
| `wamn-s4uho`, `wamn-fi8oj` | The fake `cargo tree` output in `repo_lint.rs` lacked `wasmtime-wasi`, which the engine now links. | Fixed in `c34e49750`. repo_lint passed 4 of 4. |
| `wamn-s76tz` | Since `wamn-ms1sn.2`, `assign_project_role` requires an active org membership. Two fixtures assigned a role first. | Fixed in `ab7207c8b`. The fixtures activate the membership first. control_generation_live passed 1 of 1, management_live 4 of 4. |
| `wamn-p5msj` | The issuer and the identity reader now read three `org_memberships` columns. The issuer test did not expect them. | Fixed in `d932adca5`. The issuer test passed 2 of 2. |
| `wamn-upv85` | The bundle test runs a Vite build. It ran beside the UI tests and took their CPU under load, so a 1000 ms wait timed out. | Fixed in `66d3f1413`. Two Vitest projects run the UI tests first and the bundle test after them. No wait changed. Four full runs at load 20 to 31 passed. |
| `wamn-efs58` | The caller holds `admin` only through `project-admin`. The fixture deleted an environment permission row that no longer gives the role. | Fixed in `e07f0f9f7`. The fixture revokes `project-admin` in the system authority and restores it. receiving-business passed 5 of 5. |
| `wamn-25f6e` | The chart now mounts the `wamn-oauth` key. The prekind host test had no such Secret and no local key file. | Fixed in `e1d96fd31`. The test adds the Secret and writes the key to a private file. native-host passed 1 of 1. |
| `wamn-km0sr` | Thirteen selected unit targets run no case, because their tests live in other targets. No case was lost. | Fixed in `792750045` for eleven targets: `test = false`, and the rows leave the selection. Two targets under `crates/platform` stay red: `wamn-component-composer` and `docker-credential-wamn`. |
| `wamn-5eivp` | wfe `541109c` made the Default profile report `data-ui-density="default"`. The test asserted `compact`. | Fixed in `53ee7dc08`. The test then reaches a later step and fails there, which is new bead `wamn-h794p`. |
| `wamn-uibzn` | `crates/platform/engine/src/integration.rs` has tests without the `wasm_component_model_implements` feature gate. | Not fixed. The fix is in `crates/platform`, which this epic excludes. |
| `wamn-fszfy` | The waiting worker stays installed because of the wfe gap `wamn-c361p`. | Not fixed. It depends on `wamn-c361p`, and this epic never edits wfe. |
| `wamn-b5wcb` | The login limit is 5 requests per minute per email (`password.rs:451`). One sign-in uses two, so offline check 3 gets HTTP 429. | Not fixed. Owner choice: one account per check, one shared sign-in, or a wait for `Retry-After`. |
| `wamn-h794p` | A user who revokes a role from themselves keeps a session that signs that role. The host refuses such a session with HTTP 401 (`claims.rs:203`). | Not fixed. Owner choice: the UI exchanges the session again, the host drops the role from the session, or the test revokes from a second user. |

## Acceptance

`tools/test-changes --base 25fe3a57d run` ran from 14:35:16Z to 16:54:43Z on the branch before its last rebase (`g10-accept.log`).
Two suites failed. jetstream is the excluded `wamn-6oi30`. root has 16 failures in `wamn-control`, all from `wamn-xj1c1`, which `25fe3a57d` caused and its owner is fixing.
The excluded `wamn-d8x13` is a kind case, so this run does not select it. It is still open, with no answer from three kind runs.
Main `970ed9b9b` changes only kind-tier files, so the result holds after the last rebase.
