# 67 wamn-environment-control, part 6

Scope: the platform build, `platform_build.rs` (2,662), `platform_build/inputs.rs` (698) and the two `plan.rs` files (508), at `epic/dep7` (b94833fd). Tab 62 finding 9 A already moves it to xtask and moves its two role component builds to the one component build path (tab 40). This part asks what moves and what goes. Nothing built or run.

## Earlier work checked first

- Tab 62 9 A; tab 40 (one component build path); tab 44 (tooling in xtask and just); tab 07 (the component-policy build script goes). Owner: use existing tooling, do not emulate it; preference for deletion.

## Files read

- `platform_build.rs`: header, constants, function list, `build_platform`, `process_list`, `agent_configuration`, the Cloud Build and counter section heads.
- `platform_build/inputs.rs` header. `crates/schema/generator/build.rs` directives. `ctl` `delivery_verbs.rs` arguments.

## What it does (facts)

- Builds the `host`, `ctl`, `identity` and `edge-web` images and two role components from the root `Dockerfile`, pushes them only when all succeed, and stamps their digests into a copy of the release chart. `stamp_only` (owner ruling R15) stamps digests of an existing version without building.
- `inputs.rs` hashes, per target, the Dockerfile, build arguments and every build-context file in its Cargo dependency closure that a `COPY` can reach and `.dockerignore` keeps. A target whose hash a pushed tag already carries is copied, not built. Build scripts (generator, component policy) carry literal `rerun-if-changed` directives so this hash can read them.
- `build_platform` reads every process's command line from `/proc`; if other builds run locally, it sends the build to GCP Cloud Build instead. A daily spend counter in a Cloud Storage bucket (`read_counter`, `add_cloud_minutes`, priced at `0.0156` USD per minute) feeds \`build\_location\`, called with \`0.0\` and \`3.0\` (inference: a daily cap of 3 USD). It authenticates with a separate gcloud configuration (`~/.config/gcloud-wamn-agent`).

## Findings

**1. Input hashing reproduces build caching.**

Options:

- A: delete `inputs.rs` and its plan (about 1,000 lines) and the literal directives kept for it. Docker BuildKit's registry cache (`--cache-from` / `--cache-to type=registry`) reuses unchanged layers; the xtask build always runs and lets the cache skip work.
- B: keep the input hashes in xtask.

Recommend A: existing tooling does this.

**2. Cloud Build offloading with a process scan and a spend counter.**

Options:

- A: delete the process scan, the Cloud Build path and the spend counter. The xtask build runs where it is called; a cloud build, if wanted, is a separate `just` recipe that calls `gcloud builds submit`.
- B: keep them in xtask.

Recommend A: they manage concurrent local builds and cloud cost, not the platform.

**3. What remains.**

- An xtask command: build the images with BuildKit cache, push, and stamp digests into the chart (`stamp_chart`, `stamp_only`). Role components come from the one component build path.

## Follows from earlier rulings

- Tab 62 9 A: the code leaves the environment crate. `platform/operations.rs` takes the stamped chart (tab 66). Dev `source_deploy.rs` keeps only its `loopback` check, which moves beside the registry settings.
- Tab 07: the component-policy build script and its directives are already deleted.

## Tests deleted, changed or moved

- Finding 1 A: `inputs` tests. Finding 2 A: Cloud Build, counter and `build_location` tests.

## Not read

- `build_then_push`, `stamp_from_registry`, `prepare_payload` bodies.

## Rulings

- 2026-11-12: findings 1 and 2 A (input hashing deleted for BuildKit registry cache; process scan, Cloud Build path and spend counter deleted). What remains is one xtask build, push and stamp command.
