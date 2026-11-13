# 61 wamn-release-control

Scope: `crates/control/release/src` (7,427 lines, 25 files; tests 1,612). Structure, callers and headers; `publish_release.rs` (2,691) by its items. Nothing built or run.

Rechecked at `epic/dep7` (b94833fd) on 2026-11-12:

- Qualification is already deleted. `delivery.rs` is 24 lines: shared release fixture identity and `pub mod sqlx`. The `services/ctl` qualification files are gone.
- `delivery/environment.rs` is gone; environment files are now `deploy/gcp/environments/*.k`. Finding 4 is done.
- Findings 1–3 still stand: the compose variants, `env_policies.rs` and `verification_policy.rs` in both crates, and `publish_release_with_package_manifests` are all still present.

## Earlier work checked first

- Owner 2026-11-10: release qualification is deleted now and added back when there are cases to run.
- Tab 53 finding 7: the SQLx verifier (`delivery/sqlx`) is deleted. Tab 53 finding 4: contract files deleted. Tab 51 finding 2: the build resolves operation references; readers read sealed ids. Tab 42: no freshness checks.
- Tabs 31 finding 3 and 38 finding 2 C: the route table is its own component; the host calls the application component's typed `run`; the two are not composed together.
- Tab 02 finding 5: registry SQL lives only in `wamn-registry`. Tab 58 finding 6: `helm` runs through `wamn-kubernetes`. Tab 60 finding 1: one OCI client constructor.

## Files read

- `lib.rs`, `Cargo.toml`, every module header; `publish_release.rs` items; `environment/src/verification_policy.rs`, `environment/src/env_policies.rs` (heads); `reconcile_run_plane.rs` (policy use).
- Callers of every public module and of the compose and publish functions (grep).

## One purpose per file

- `publish_release.rs` (+ `publish_release/*`): compose and publish a release manifest to OCI.
- `release_composition.rs`: the components of a release from manifests and `build.json`.
- `release_chart.rs`: the Helm values of one application's release chart.
- `release_carrier.rs`: the release identity a workload carries.
- `delivery.rs`, `delivery/plan.rs`, `delivery/qualification_artifact*`: release qualification.
- `delivery/sqlx*`: the SQLx verifier step.
- `delivery/environment.rs`: the environment file `deploy/gcp/environments/<key>.json`.
- `env_policies.rs`, `verification_policy.rs`: reads of `registry.env_policies`.

## Findings

**1. Five composition entry points.**

Fact: `compose_application`, `compose_release_entry` (edge bundle), `compose_covering_entry` (entry publication), `refresh_composed_entry` (dev), `compose_pinned_application` (edge bundle), with `select_application`, `covering_application`, `assemble_local_release` and `publish_local`. They exist to compose the route guest with the application for the cloud, the box and dev.

Options:

- A: one compose function: the route table component and the application component, built once (tabs 31 and 38); publish, dev and the edge bundle call it. The covering, pinned and refresh variants go.

**2. Registry reads in the release crate, with namesakes in the environment crate.**

Fact: `env_policies.rs` (100 lines) and `verification_policy.rs` (85) read `registry.env_policies`; their callers are only `control/environment` and `control/dev`. `control/environment` has its own `env_policies.rs` (29) and `verification_policy.rs` (82) that wrap them.

Options:

- A: the reads move to `wamn-registry`; the projection into a project database stays in `control/environment`. The four files become one read there and one projection.

**3. A test-only publish entry in product code.**

Fact: `publish_release_with_package_manifests` is called only by `environment/src/publish_release_tests.rs` and `effective_release_live.rs`.

Options:

- A: the tests call `publish_release`; the extra entry goes.

**4. The environment file reader sits under release delivery.**

Fact: `delivery/environment.rs` reads `deploy/gcp/environments/<org>--<project>--<env>.json`.

Options:

- A: it moves to `control/environment`, its owner.

Dep7: done (file removed).

## Follows from earlier rulings

- Qualification deleted: `delivery.rs`, `delivery/plan.rs`, `delivery/qualification_artifact.rs` and `plan.rs`, `services/ctl/src/qualification*`, and their uses in `environment/analyse.rs` and dev.
- Tab 53 finding 7: `delivery/sqlx.rs` and `plan.rs` go.
- Tab 51 finding 2: `publish_release/attachments.rs` reads sealed ids; its reference resolution goes.
- Tabs 53 finding 4 and 42: `package_sources.rs` stops reading contract files and calling `check_current`.
- Tab 60 finding 1: `publish_release.rs` uses the one OCI client. Tab 58 finding 6: `release_chart.rs` runs `helm` through `wamn-kubernetes`.
- U2 (SQLx), `hyper` over `reqwest`, tab 02 (`wamn-registry`), tab 01 (`wamn-execution-contract`).

## Tests deleted, changed or moved

- Qualification and SQLx verifier tests go.
- Finding 1: composition tests of the removed variants go; one test of the one function stays.

## Not read

- `publish_release.rs` bodies; `release_chart.rs` values; `package_sources.rs` beyond the contract reader; tests.

## Rulings

- 2026-11-12: all findings A.
