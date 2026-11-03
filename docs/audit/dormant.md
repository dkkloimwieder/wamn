# Dormant code

This report covers epic `wamn-wv9hl`, which takes audit rows A1-1 to A1-10 of [code quality](code-quality.md) and B1-6, B3-4 and B3-5 of [coupling](coupling.md).
The branch is `worktree-dormant3`, on main `6fbf2aa3f`.
The branch changes 224 files, with 921 insertions and 3,661 deletions.
Every citation was pinned again against main before the change. No target was already gone, and none was created again.

## Issues

| Issue | Audit reference | Commits | Result |
| --- | --- | --- | --- |
| `.1` Dormant code | A1-1 to A1-5, A1-7 to A1-9 | `a8df36c0d` to `e5773a12d`, one per crate | The startup-burst and operator-recovery modules, the router wiring cache with its OpenTelemetry dependency, the uncalled pool, authority and wiring APIs, `typed_context`, five generator and catalog methods and `K8sSecretProvider` go. `connection_refused` and its test move to test infrastructure. 1,891 lines go. |
| `.2` Unused dependencies | A1-6, A1-10 | `f104a11f0`, `bc8bed205` | A1-6: 39 declarations deleted, 15 kept with a cited use. A1-10: 9 deleted, 43 kept. The fixture entries that the audit names stay. |
| `.3` Canonical WIT | B1-6 | `654f23d2e` | Ten WIT packages move byte for byte to `wit/`. Hosts and guests read the same files, and the package versions do not change. The guest-workspace closure test refuses a guest WIT path into a root workspace member. |
| `.4` Guest folds | B3-4, B3-5 | `94434368b` to `cd019f50c` | Five application data crates become private `data` modules of their components. The SQLx driver folds into `sqlx-command`, and `label-template` into `label-render` with its golden vectors. Modbus moves to `apps/platform/no-std/modbus`. `http-fixture` becomes a named binary of `wamn-integration-tests`. The connector fixture was already gone. |

## Repin

One rebuild covers issues 2, 3 and 4. Twelve packages take a patch version with the old version as `predecessor_version`.
These are `client_acme_receiving`, `edge_device`, `edge_samples`, `modbus_reader`, `netsuite`, `platform_fixture`, `platform_fixture_overlay`, `rest_connector`, `rest_consumer_fixture`, `wamn_integration`, `wamn_receiving` and `wamn_wms`.
The HTTP route build equals its existing pin. Generated files outside the twelve packages did not change.
Each new digest is in `/var/tmp/wamn-docs/dormant4-items.md`.

## Decisions

The owner gave these answers.

1. Issue 3, option A: the one path line in `wamn_jetstream.rs` moves in the same commit as the WIT files, so all ten packages move together.
2. Issues 3 and 4 take one rebuild and one qualification cycle.

Root made these decisions inside the brief.

1. `wit/` is at the repository root. The old directories keep symbolic links, so readers that walk `wit/deps` work without a change.
2. The scope fence `platform/**` means `crates/control/lib/src/platform/**`.
3. The guest commit of issue 2 waited for the repin, because its changed digests broke application preparation.
4. The closure rule refuses only guest WIT paths into root workspace members. A wider rule refused 20 valid paths into generated output.
5. `canonical_base_url` and `canonical_proxy_url` stay, because production code calls them.

## Tests

Each issue ran `cargo check --all-targets` on each owning package, Clippy and `tools/test-changes --base origin/main --tier unit,contract run`.
The final run on `cd019f50c` has only these failures, each a known main failure:

1. Three `repo_lint` cases (`wamn-s4uho`).
2. Three engine `integration::tests` cases, with "the `cm-implements` feature is not active" (`wamn-uibzn`).
3. Seven targets with zero cases (`wamn-km0sr`).
4. `administration.test.tsx:210`, which times out under load (`wamn-upv85`). It passes 40 of 40 when it runs alone.

Clippy with `-D warnings` passes in the root, apps and no_std workspaces. The logs are in `/var/tmp/wamn-docs`.

## Not run

`qualify-release` did not run. It needs a published release digest, a stamped chart, registry credentials, images and a kind cluster for each case, and the brief forbids all of these.
No image build tests the three new `COPY wit` lines in the Dockerfile.

## Follow-up

1. Main records `wamn_receiving-2.2.9`, but the web epic changed its bundle. This branch records `2.2.10`, which closes the gap when it merges.
