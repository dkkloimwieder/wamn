# 72 wamn-host service

Scope: `services/host` at `epic/dep7` (b94833fd): `main.rs` (83), `host.rs` (3,985, of which about 1,235 are tests), `host/commands.rs` (380), `host/lease_tests.rs` (191), `observability.rs` (329), three test files in `tests/` (1,308). Nothing built or run.

## Earlier work checked first

- Tab 20 finding 5: option B (the host runs one path; dev writes what the cluster path reads) was left open for the dev tab.
- Tab 26 finding 1: the second HTTP server goes; file routes and the OAuth callback are ingress service routes.
- Tab 31 finding 1 B: host routes become native ingress service routes. Finding 3 A: the application route table lives in the guest; the host keeps capabilities only.
- Tab 09: one login per database role; `CredentialGeneration` goes. Tab 62 1 A and tab 66: one config file per component, given to hosts by `envFrom` or a mount.
- Tab 70 1 A (pending the kind measurement): dev runs through the product path; native activation goes.
- Tab 71: `wamn` is the one user CLI; every user verb calls a control route.

## Files read

- In full: `main.rs`, `host/commands.rs` (non-test), `Cargo.toml`.
- `host.rs`: `HostArgs`, the release load and mode branches of `run`, control route wiring, plugin list, function list, test names.
- `observability.rs` (head and functions). Test file heads in `tests/`.

## Callers read

- Who passes host modes: `crates/control/environment/src/platform/control.rs` (`--control`), `crates/control/dev/src/dev/control.rs` (`--local-control`), `dev/activation/plan.rs` (`--local-application`).
- `deploy/platform/control-service.yaml` (the control WorkloadDeployment). `wamn_engine::release_manifest::LoadedRelease::control_root`.

## One purpose per file

- `main.rs`: parse the CLI, set the log filter, run the host.
- `host.rs`: everything else: arguments, release load, credentials, NATS and database connections, plugin composition, control routes, probes, shutdown.
- `host/commands.rs`: workload start and stop policy and drain around the wash-runtime defaults.
- `observability.rs`: the tracing subscriber with unsampled spans dropped.

## Findings

**1. One `run` function composes four host modes.**

Fact:

- `host::run` is about 1,240 lines. It builds: an application host (release from the registry), a control host (`--control`), a local application host (`--local-application`, `--local-application-digest`) and a local control host (`--local-control`).
- Mode branches: `args.control` 8 times, local mode names 29 times, outside tests.
- The local modes have no caller outside `crates/control/dev` (`dev/control.rs`, `dev/activation/plan.rs`).

Options:

- A: delete the two local modes with the native dev path (tab 70 1 A). This settles tab 20 finding 5 as B. Then split what is left of `host.rs` into modules by purpose (settings, release load, connections, composition, shutdown).

Recommend A, after the kind measurement in tab 70.

**2. The control host runs a route guest and a placeholder release to reach native handlers.**

Fact:

- `LoadedRelease::control_root` builds a release with no package, component, route or database; its doc comment says its scope "names no real" org or environment.
- `control-service.yaml` deploys one component, `flow-http` (the http-route guest), on host group `c-<org>`. Its host interfaces are routing, `wamn:operation` and `router-delivery`. Every control handler is native (`HostRouteHandlers::Control`).
- The control host gets no event streams, no project credentials and no application release.

Inference: after tab 31 1 B (host routes are native ingress service routes), the control host runs no guest code at all.

Options:

- A: keep the control host as a wasmCloud host. Control routes register as native ingress service routes; `control_root` and the control WorkloadDeployment go.
- B: the control routes become a plain native HTTP service (hyper), deployed by the platform chart as a Deployment. `--control`, `control_root`, the control host group and every control branch leave `wamn-host`; the runtime operator no longer schedules it.

Recommend B. Inference: a wasmCloud host with no guest adds operator scheduling, a NATS heartbeat and a wash runtime for no use.

**3. Code comments cite beads and plan sections.**

Fact:

- `host.rs` cites bead ids 56 times (21 distinct, for example `wamn-0h0g.22.31`) and `docs/plan/` 4 times; several comments tell history ("CUT THE EXECUTOR-PLATFORM CLASS OVER").
- The `main.rs` header lists `wamn-ctl` verbs that tab 71 removes, and says "MVP outcome: crash floor".
- Repository-wide: at least 872 bead ids with a digit in 211 Rust files (grep; ids without a digit are not counted).

Options:

- A: comments state the rule the code keeps. Bead ids, plan section labels (R5, S5) and history go; fix the `main.rs` header.

Recommend A.

## Follows from earlier rulings

- Tab 62 1 A, tab 66: settings arrive four ways today: 67 `HostArgs` fields (39 with environment names), 10 direct `std::env::var` reads in `host.rs`, plugins reading their own variables (`WamnLogging::from_env`, `WAMN_PG_*`), and chart `extraArgs` where `--environment` is passed twice and "the last value wins". `--nats-connect-timeout` is accepted and not used. All become one config file.
- Tab 09: the "provisioned generation" logic for the executor-platform credential class goes with `CredentialGeneration`.
- Tab 26 1: `--files-addr` and its listener go.
- Tab 71: the `main.rs` header no longer names `wamn-ctl` verbs.

## Fine as is

- `host/commands.rs`: start and stop policy and drain; the runtime keeps pulls, compilation and stop races.
- `observability.rs`: drops spans of unsampled traces.
- The shutdown sequence (`stop_combined_after`, one drain budget).

## Tests deleted, changed or moved

- Finding 1 A: `local_materializer_requires_its_service_and_no_components`, `local_workload_preserves_limits_and_refuses_missing_or_changed_bytes`, `local_application_arguments_require_an_exact_digest_and_exclude_publication_inputs`, `local_control_uses_the_control_authority_without_an_application_release`.
- Finding 2 B: control wiring tests move to the new service; `crates/control/environment::platform::control` host group tests change.
- Tab 62 1 A: `chart_lifecycle_flags_parse_with_native_defaults`, `a_release_environment_argument_replaces_the_chart_namespace`, `the_host_namespace_is_the_chart_variable_and_never_the_environment` change to config file tests.
- CLAUDE.md test rule 3: `the_removed_runtime_flags_are_not_accepted` checks that deleted flags stay deleted; it goes.

## Not read

- `host.rs` bodies of the credential, NATS and plugin setup in full; `observability.rs` body.
- Whether a host with no release at all (`a_host_given_no_release_pair_carries_no_release`) runs anywhere in production.
- `tests/native_lifecycle_live.rs`, `tests/scheduler_nats_move_live.rs` bodies.

## Rulings

- 2026-11-12: finding 1 A (after the kind measurement; settles tab 20 finding 5 as B), finding 2 B (control routes become a native HTTP service; control leaves wamn-host), finding 3 A (comments state the rule; bead ids, plan labels and history go).
- 2026-11-12 (owner): finding 1 A no longer waits. The host's local modes go; local development runs on `kind`. The `kind` loop measurement is a step of the implementation plan.
