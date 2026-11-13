# 70 wamn-dev-control, part 2

Scope: how `wamn dev` runs and provisions, at `epic/dep7` (b94833fd): `dev/activation.rs` (2,121) and `plan.rs` (504), `dev/environment.rs` (1,214), `dev/up.rs` (292), `dev/config.rs` (1,860) and `plan.rs` (588), `dev/pat_issuer.rs` (567), `dev/control.rs` (263), `dev/cdc_reader.rs` (136), `dev/edge_bundle.rs` (255), `dev/watch.rs`, `dev/observations.rs`, `dev/read.rs`, `dev/session.rs`, `git_source.rs`, `owned_command.rs`, `source_deploy.rs`. Nothing built or run.

## Earlier work checked first

- Owner: one uniform build and runtime method; use wasmCloud and existing tooling as they are; a fast dev loop; preference for deletion. CLAUDE.md: a journey test drives the product's own deploy path.
- Tab 64 1 A: the control route is the one execution path; kubeconfig only for bootstrap and dev. Tab 65 3 A: provisioning verbs become internal steps. Tab 62 1 A: one config file per component. Tabs 31 and 38: the route table is its own component, not composed with the application.

## Files read

- Heads of every module; `activation.rs` constants and function list; `environment.rs` function list; `up.rs` and `control.rs` heads; `services/ctl/src/dev/` file list.
- Callers of `git_source` and `owned_command` (grep).

## Findings

**1. Dev runs a second runtime topology with its own provisioning.**

Fact: `activation.rs` starts and supervises a native `wamn-host` process and drives the wash-runtime workload API through the scheduler NATS. `pat_issuer.rs` runs a separate identity process, `control.rs` the org control host, `cdc_reader.rs` the CDC reader, and activation stages the materializer. `environment.rs` and `up.rs` provision this environment by calling provisioning pieces directly in their own order (`provision_journey_control`, `install_journey_platform_floor`, `reconcile_journey_run_plane`, `prepare_journey_credentials`, `provision_project_env`, credential generation actions, template preparation). `config.rs` validates and preflights the externally supplied services. Together about 7,500 lines. The product runs the same parts on Kubernetes through charts, the runtime operator, Jobs and the control route.

Options:

- A: dev targets a local kind cluster through the product path. The platform is installed once on kind (tab 66); `wamn dev` builds, publishes to the local registry and deploys through the control route; the watch loop repeats. Native activation, process supervision (host, identity, control, CDC reader, materializer), the dev provisioning order and the service preflight go. Inference, not measured: an application change publishes a release and updates workloads without restarting host pods, so each loop costs a push and a reconcile.
- B: keep the native topology, but provision it through the same internal steps as the executor (tab 64 1 A, tab 65 3 A) and read the same config file (tab 62 1 A). The dev-only provisioning order goes; process supervision stays.

Recommend A: one runtime and one provisioning path, and dev then exercises what production runs. Measure the loop on kind before deleting the native path.

**2. Two helper modules have only dev callers.**

Fact: `git_source.rs` says it serves "the callers that qualify a revision"; qualification is deleted, and its callers are now `dev.rs` and `dev/watch.rs`. `owned_command.rs` is used by the same two. Both are public modules of the crate.

Options:

- A: make them private modules of `dev`, and fix the header.

Recommend A.

## Follows from earlier rulings

- Tabs 31 and 38: `edge_bundle.rs` composes the http-route guest with the application into one entry; it writes the route table component and the application component separately instead.
- Tab 69: the loop's six stages. Tab 62 7 A: `claim_environment_instance` goes. Row 7: `generation_args` and the generation actions in `environment.rs` go.
- Tab 68 1 A: `reconcile_journey_run_plane` goes; identity rows come from the identity crate's fill.

## Fine as is

- `source_deploy.rs`: builds and publishes one application's source before the deploy route (client side, tab 64 1 A).
- `watch.rs`, `session.rs`, `read.rs`, `observations.rs`: the dev loop's file watching, session and read-only trace view.

## Tests deleted, changed or moved

- Finding 1 A: activation, `pat_issuer`, `control`, `cdc_reader` and `environment` tests; journey setup in tests moves to the kind path.

## Not read

- Bodies of `activation.rs`, `config.rs`, `environment.rs`, `watch.rs`.

## Rulings

- 2026-11-12: finding 1 A, pending a measurement of the loop on kind. Owner: local dev overrides or CLI tools may still be needed for developer ergonomics. Finding 2 A.
- 2026-11-12 (owner): finding 1 A no longer waits. Local development moves to `kind`. The `kind` loop measurement is a step of the implementation plan; local overrides or CLI tools are added only if it shows a need.
