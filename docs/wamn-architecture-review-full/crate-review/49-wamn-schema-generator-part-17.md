# 49 wamn-schema-generator, part 17

Scope: the OpenTofu renderer (`tofu.rs`) and the platform document schemas the generator carries. Nothing built or run; nothing deployed.

## Files read

- `src/tofu.rs` (non-test part, in full).
- `src/authoring.rs::compile_schema_file`, `compile_environment` (read in tab 43).

## Callers read

- `services/ctl/src/project_verbs.rs` (the one product caller of `render_project_tofu`).
- `crates/control/environment/src/platform/project.rs` (`ProjectDocument` and its parts), `platform/document.rs`, `environment/document.rs` (the callers of `compile_schema_file` and `compile_environment`).
- `docs/plan/platform-install.md` (P12, E0: the project layer rendered to `.tf.json`).

## One purpose per file

- `tofu.rs`: render a validated project document (`project.k`) as OpenTofu JSON (`main.tf.json`) for the GCP cluster, node pools, registry, DNS zone and the cost guard's state binding.

## Findings

**1. Project infrastructure is rendered inside the schema generator, with a copy of the project types.**

Fact:

- `render_project_tofu` has one product caller, the `wamn-ctl` project verb. Nothing in package generation uses it.
- `tofu.rs` declares `ProjectInput`, `KindRegistry`, `DnsZone`, `GuardState`, `Edge`, `NodePool`. `control/environment::platform::project` declares `ProjectDocument` and the same five parts, field for field. The comment in `tofu.rs` gives the reason: decode the control crate's JSON "without adding a dependency from the generator back to control".

Options:

- A: move the renderer next to `ProjectDocument` (`control/environment::platform`) and render from that type. Delete `tofu.rs` from the generator and its five private types.

**2. The generator carries the KCL schemas of platform documents.**

Fact: besides the package manifest schema, the generator embeds the `environment`, `cluster` and `project` KCL schema modules (`kcl/environment`, `kcl/cluster`, `kcl/project`) and exposes `compile_environment` and `compile_schema_file`. Their callers and their document types are all in `control/environment` (`platform/document.rs`, `platform/project.rs`, `environment/document.rs`).

Options:

- A: each schema module moves to the crate that owns its document type. The KCL run itself (binary lookup and `kcl run`) moves to one small shared function both crates call, so it is written once.

## Tests deleted, changed or moved

- Findings 1–2: `tofu.rs` tests and the document compile tests move with their code to `control/environment`.

## Not read

- The KCL schema modules under `kcl/` (content).

## Rulings

- Findings 1–2: A (owner, 2026-11-11).
