# Upgrade environment

Updated through: 2026-10-02, `main` at `d3be6482e`. Accepted by the owner on 2026-10-02 with the amendments of §4.1 and stage 11. Epic 1 of two. Epic 2, `create-environment`, is named in §6 only.

## 1. Goal

One verb, `wamn-ctl upgrade-environment --org <org> --project <project> --env <env> --commit <40-hex commit>`, takes one installed environment to the bytes of one commit. It does what B0 to B13 of `docs/plan/kind-to-type.md` §3.2 did by hand on 2026-10-01 and 2026-10-02. It builds and pushes from the pinned commit, upgrades the schemas, publishes, qualifies, selects, deploys and checks. It writes one run record that the operations page cites.

The cutover took most of two days of wall time for one environment pair (§3.1). Most of its stops had three causes. A person ran the steps by hand in a fixed order. Some tools had never run against a private registry. Some inputs followed the head of `main`. The verb removes those three causes. It adds no new delivery rule: every check it runs exists today in a verb or in the runbook.

## 2. Fixed rules

- One verb, one environment, one commit. `--commit` is a full 40-hex commit. The verb refuses a branch name, a tag and a short hash. No step reads `origin/main`, `HEAD` of another checkout, or the newest of anything.
- The verb builds in its own clean checkout of `--commit` and its own target directory. It does not use the operator's working tree.
- The gates image is a pinned digest, recorded and pulled, not built (`wamn-1s38`). Host, identity and ctl images are built from `--commit` by source identity and compared by `qualify-release` as today.
- The verb calls four test tools: `tools/registry-image-archive`, `tools/journey-image-cache`, `tools/delivery-owned` and `tools/build-components`. Each has a test in kind against a private registry with a login before a live run uses it. The verb calls no tool without such a test.
- The registry stays private. No credential reaches a kind node. No service account key and no HMAC key. Tokens live only in mode 0600 files in private directories and are deleted after use. Passwords are read into variables and never printed (owner rules of the cutover).
- The attestation is the contract, not the release number (owner ruling on `wamn-ld93.21`, 2026-10-02). The verb takes the next free release id of the environment. It never publishes again under an id that holds an attestation of another commit.
- Tags are immutable on the `wamn` registry since 2026-10-02 (B13). A guest file is pushed once under the sha256 hex of its bytes. If that tag exists, the verb reads its digest and does not push again.
- The verb stops only on a refusal that changes what runs: a `publish-qualified-release` bytes refusal, a migration refusal, a failed qualification, or a failed serve check after the hosts change (the stop rule of the cutover). Every other failure is either retried by a defined rule or is a defect of the verb.
- Each step is idempotent. It reads the state that it changes. When the state is already the target, it does nothing and records that. A second run of the verb with the same arguments after a stop resumes at the first unfinished step.
- The run record is the only output that a person reads. It names every step, its start and end in UTC, its inputs and its result, with no credential.
- No verb touches `wamn-dev` until it completed end to end on a kind stack with the same parts as GCP (owner ruling of 2026-10-02 after run 1, `wamn-m511.7`). Those parts are the CloudNativePG operator and a `Cluster`, NATS pods with `WAMN_TAP`, identity with its operator certificate, the control store, and a private registry with a login.
- Preflight is stage 1. Every read-only live check runs before any build (same ruling).
- Every stage has a test against a real Postgres and a real registry. Unit tests of helpers do not count (same ruling).
- A retry covers only a connection reset after a node change (same ruling).

## 3. Current state

Measured on `main` at `d3be6482e` on 2026-10-02.

### 3.1 The cutover run

The run went from B0 at 02:42 UTC to B13 at 17:12 UTC on 2026-10-02, 870 minutes of wall time. The table lists every stop. A stop is any point where B0 to B13 did not go on as planned. The minutes run from the end of normal progress to the resumption. "Estimate" marks a value derived from two logs. The sources are `~/.cache/wamn-f8-cut/drifts.md`, the step logs, the beads and the session transcript.

| # | UTC | Step | Symptom | Cause | Fix | Bead | Minutes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 02:39 to 02:46 | B0 | Auto mode denied the deploy command. | Session permission mode. | The owner switched to manual mode. | `wamn-ld93.13` | 6.5 |
| 2 | 02:53 to 02:55 | B0 5 to 8 | The forward lost its connection to the pod. | A TLS close by psql reset the forward. | `sslmode=disable` (drift 1). | `wamn-ld93.13` | 1.6 |
| 3 | 03:21 to 04:10 | B0 3 | Gates build E0063, a missing field. | `main` at `0069d6bd3` did not compile. | `f31b33a12`, B0 again at `71fc165f8`. | `wamn-ld93.13` | 50, estimate |
| 4 | 04:18 to 09:55 | B4 | CDC reader rollouts timed out. | The operator machine slept, and the guard set the pool to 0 at 07:00. | Resize in 68 s, tap Job in 22 s. | `wamn-ld93.17` | 337 |
| 5 | 09:57 | B6 | Auto mode blocked the identity upgrade. | Session permission mode. | The owner switched to manual mode. | `wamn-ld93.19` | 0.2 |
| 6 | 09:59 to 10:04 | B7 6 | "the pinned host image differs from the selected source build" | The push used the unlabeled `src-` image. | Push the relabeled image (drift 9). | `wamn-ld93.20` | 4.3 |
| 7 | 10:00 | B10 before | `provision-org` failed. | It needs `--owner-email`. | The flag (drift 10). | `wamn-ld93.23` | 0, in parallel |
| 8 | 10:11 to 11:11 | B7 6 | The kind nodes did not pull the images. | The kind nodes have no registry credential. | `docker save` rewrites the digest, so the owner ruled for an OCI archive: `6c9241c9a`, B0 again. | `wamn-ld93.20` | 60 |
| 9 | 11:15 to 11:21 | B7 6 | The archive loaded with no repository digest. | CRI takes a repository digest only from a digest name. | `37d1fe4c8`. | `wamn-ld93.20` | 6 |
| 10 | 11:24 to 11:33 | B7 6 | "host credential files differ from the declaration" | A patch file beside the Secrets was read as a credential. | `ebc75d513`, gates rebuild in 243 s. | `wamn-ld93.20` | 9.2 |
| 11 | 11:41 to 11:48 | B8 3 | The gate service did not start. | The gate refuses a query on its database URLs. | No query (drift 11). | `wamn-ld93.21` | 7, estimate |
| 12 | 11:52 to 12:50 | B8 4 | "wiring … version 2 is already authored with other facts" | `wirings_definition_key` had no `package_version`. | Owner ruling (a), `ebac92ef4` with migration 0005, B3 again, images again in 37 minutes. | `wamn-ld93.21` | 57.7 |
| 13 | 12:53 to 13:25 | B8 7 | "qualified artifacts changed or disappeared" | The candidate was prepared before the target was built at the same commit. | Prepare again (drift 12). | `wamn-ld93.21` | 32.5 |
| 14 | 13:25 to 15:18 | B8 7 | "the idle materializer reported a refusal, failure, or unsettled delivery", twice | Three replicas pull one consumer with `max_waiting: 1`. | Owner ruling (a), `351f71337` on `cutover`, B0 steps 1 to 4 again. | `wamn-z9cp`, `wamn-e6iw` | 112 |
| 15 | 15:03, seen 15:18 | B7 5 | "Connection reset by peer" | Spot preemption of both nodes. | Tap Job in 19 s, check in 4 s, chain again. | `wamn-qjcd` | 1.4 |
| 16 | 15:36 to 15:45 | B7 7 | `deployment-attestation-content-conflict: dev//2 -> dkk/receiving/dev` | B7 attested Receiving release 2 at `ebc75d513`. | Owner ruling: Receiving release 3. | `wamn-ld93.21` | 9, estimate |
| 17 | 15:47 to 16:12 | B10 4 | Helm timed out after 605 s, "Insufficient cpu". | The new control host group did not fit on 2 nodes. | Owner ruling: pool to 3 nodes in 284 s. | `wamn-ld93.13`, `wamn-ld93.23` | 23, estimate |
| 18 | 16:11 to 16:18 | B10 5 | `JsError::ConnectionUnavailable` on the hosts, workload wait timeout after 437 s. | Spot preemption of two nodes. | Tap Job in 40 s, check in 9 s, helm again. | `wamn-qjcd` | 7, estimate |
| 19 | 16:18 | B10 check | `/pallet/query` answered 404. | The runbook named a route that WMS 2.0.0 renamed. | `/packaging/query` (drift 17). | `wamn-ld93.23` | 1 |
| 20 | 16:20 | B10 `wamn-rjtf` | "Connection reset by peer" on the revoke. | The service forward. | A pod forward. | `wamn-rjtf` | 0.5 |
| 21 | 17:01 to 17:06 | B12 | "replacement control-author generation has no verified live private-pool session" | A retire needs a live session of the replacement generation. | Owner ruling: hold the gate service during the retire. | `wamn-ld93.25` | 4.3 |

The stops took about 731 of the 870 minutes. Without the five estimate rows, the measured sum is about 635 minutes. Row 4 alone took 337 minutes. The owner waits that blocked the run took about 63 minutes, and row 14 had the longest at 20 minutes. The B11 check of the owner took 39 minutes, but it is a planned step. The deployment agent left two failed chains without a reaction for 30 and 39 minutes, inside rows 13 and 14.

Eleven stops came from code or tools on their first live run. They are rows 3, 6, 8 to 14, 16 and 21. Five came from the environment (rows 4, 15, 17, 18 and 20). Three came from the runbook text (rows 2, 7 and 19). Two came from the session permission mode (rows 1 and 5).

### 3.2 Where the steps live

| Step of §3.2 B0 to B13 | Today |
| --- | --- |
| Checkout | B0 step 1 runs `git checkout --detach origin/main` (`docs/plan/kind-to-type.md:508`). The cutover commit was "the head of `main` on the day", so every fix moved it. Six cutover commits followed: `6c9241c9a`, `37d1fe4c8`, `ebc75d513`, `ebac92ef4`, `351f71337`, and the records `da18d7ea0`, `de06e7feb`. |
| Images | `tools/journey-image-cache ensure` builds by source identity, the hash of the tree entries that the Dockerfile copies at HEAD (`tools/journey-image-cache:9`). The push of the relabeled image was a by-hand rule (drift 9). |
| Gates image | `compare_built_image` rebuilds the gates image from the qualified commit and requires byte equality (`crates/control/lib/src/delivery/qualification.rs:592`). The build took 1049 s at `71fc165f8` and 1373 s at `ebac92ef4` (`wamn-1s38`). |
| Qualification | `qualify-release --revision` accepts any clean selected revision (`qualification.rs:229-248`). It rebuilds the delivery target and then compares artifact hashes with the candidate (`qualification.rs:291`). A candidate prepared before the target was built at the same commit fails (drift 12). It deletes its case results on exit (`qualification.rs:87-90`, `wamn-e6iw`). |
| Kind images | `tools/registry-image-archive` fetches a pinned manifest and its blobs into an OCI archive for each kind node (`6c9241c9a`, `37d1fe4c8`). It has no test. Its first runs were the live qualify runs of the cutover. |
| Host values | `HOST_IMAGE` is a source constant of `test-support/infrastructure/examples/host_values_files.rs:48`, edited for each deploy. The release digests are arguments. |
| Workloads | `test-support/infrastructure/examples/workload_files.rs` renders the four workloads from four guest digests. |
| Deploy | `deploy-release` exists (`crates/control/lib/src/delivery/deployment.rs:109`). It applies a rendered native Deployment and checks one request. Only the kind cases call it. The cutover used `helm upgrade` and `kubectl apply` by hand (B10). |
| Edge | `wamn web upload`, `helm upgrade wamn-edge` and `gcloud compute url-maps import` by hand, after a manual digest edit of two private files (B9). |
| Broker | `WAMN_TAP` is memory storage. Every broker restart needs the tap-stream Job and the check of `gcp.md` §3.5 again. Two Spot preemptions restarted the broker during the run (`wamn-qjcd`). |
| Capacity | No step compares host requests with node capacity. The new control host group did not fit on 2 nodes (B10). |
| Gate credentials | Retiring a generation needs a live session of the replacement (`crates/control/lib/src/provision_project_env/workload.rs:942`). The gate service runs only for one request at B8, so B12 started it again by hand. |
| Record | `gcp.md` §7 and `kind-to-type.md` §3.2 were written by hand from 17 drifts and the logs, at B12. |

### 3.3 Post-mortem of run 1

Run 1 of the verb on `wamn-dev` ran on 2026-10-02 at `abaac6ce8` and `c180d88bd`. It failed three times for Receiving and never reached WMS. Before the third failure, it changed one project database. Pool `main` went to 0 nodes at 22:13 UTC by owner order. The owner then parked the deploy work. Failings 4 and 5 are `wamn-m511.8` and `wamn-m511.9`.

| UTC | Step | Result |
| --- | --- | --- |
| 20:42 to 20:47 | Full check at `abaac6ce8` | fmt, workspace clippy and the tests of the two changed crates passed. `main` went to `abaac6ce8`. |
| 20:48:57 to 21:31:45 | Attempt 1, 2568 s | Stages 1 to 4 passed: build 1021 s, images 1538 s, guests 6 s. Stage 5 stopped because no node had 500m CPU for the surge pod of a host group. Nothing on `wamn-dev` changed. |
| 21:54 to 21:56 | Resize | Pool `main` went to 4 nodes by owner ruling. |
| 22:03:08 to 22:04:20 | Attempt 2, 72 s | Stage 5 stopped on "relation catalog.tenant_environments does not exist". Nothing on `wamn-dev` changed. |
| 22:04 to 22:06 | Fix | `b1ea822a4`, then `main` went to `c180d88bd`. |
| 22:06:41 to 22:07:27 | Attempt 3, 46 s | Stage 5 passed with release id 4. Stage 6 applied project migration 0006. Stage 7 applied the package, then failed twice on "registry-credentials-incomplete". |
| 22:07:47 to 22:13:19 | Resize, 332 s | Pool `main` went to 0 nodes. |

The deployment agent recorded eight failings.

1. The verb reached the live environment before stages 5 to 12 ran once. The kind run was to test stages 1 to 9. Three rounds of owner questions ended with a fixture that does not have what stage 5 needs. The agent found that by reading the source after the owner chose the fixture. The agent did not map the needs of each stage against the fixture before the first question.
2. The verb reads the environment file and the deploy inputs from the checkout of `--commit`. So no commit before the verb can be its target, and the ordered no-op run at `351f71337` was impossible. The agent found this when the run was ordered, not when it wrote the stage.
3. The capacity stop of attempt 1 was correct, but it came after 43 minutes of build. The pool size was known from the cutover, and the agent did not compare it with the surge need before the run. The verb also runs the cheap live checks after the expensive build.
4. The verb read `catalog.tenant_environments` and `catalog.deployment_attestations` from the project database. Both live only in the control store of `wamn_system`. No unit test touched a database, so only the live run found it.
5. `Registry::login` writes a Docker configuration with only an "auth" field. The component push of stage 7 reads that file through `registry_credentials.rs`, which requires "username" and "password". Stages 3 and 4 passed because docker and wash accept "auth". No test covered the second reader. This is not fixed.
6. The retry rule of §4.3 retried each deterministic failure once. That cost only seconds, but the rule cannot tell a defect from a preemption.
7. The agent merged the verb into `main` with only unit tests of its helpers. The full check was green, but it tests nothing of stages 5 to 12.
8. The pool went from 4 nodes straight to 0. The ruling said back to 3 before the spin-down. The node count to resume is 3.

The run left this state.

- On `wamn-dev`, project migration `migrations/project/0006_administration_release_head.sql` is applied to `wamn-db-dkk--receiving--dev--4pqjfmli`. The Receiving package was applied again, with only "already exists" notices. No release 4 was published or selected. No gate generation, PAT, Job, host or workload changed. WMS was not touched.
- In the Artifact Registry repository `wamn`, attempt 1 pushed `wamn-host`, `wamn-identity` and `wamn-ctl` under `src-405087ccabc0e606`, and the flow-http and materializer guests under their sha256 tags. Tags are immutable.
- Pool `main` has 0 nodes. The pending pods are the standing workloads that wait for nodes. No Job of the run is pending.
- `main` at `c180d88bd` holds the verb with failing 5 open.

The root cause is the test method. The agent wrote the verb stage by stage with unit tests of pure helpers. The only test of the stages against real state was the live run. Each failure was in the code between the verb and a real system: the pool, the control store and the registry credential reader.

## 4. Design

### 4.1 The environment file

`deploy/gcp/environments/<org>--<project>--<env>.json` holds only what `registry.project_envs` does not hold. It names the cluster, the kubeconfig context and the registry. It names the web client bucket and prefix, the route host and the host group. It names the edge, the URL map, the guest workloads and the gates image digest. The tenant and the database names come from `registry.project_envs`. The file does not repeat them, because two copies drift (owner ruling, 2026-10-02). It holds no credential. The verb refuses an environment without this file. The `wamn-dev` files for Receiving and WMS are written from the B12 record.

The environment file also names `package_workloads`, a map from package IDs to WorkloadDeployment names.
This map records deployment topology. Qualification reads each actual schema from the live workload, not from this file.
Receiving maps `wamn_receiving` to `flow-http`. WMS maps `wamn_wms` to `wms-flow-http`.

### 4.2 The stages

The owner requires original package inputs from the pushed artifact of each installed version in `catalog.package_artifacts`.
Table owns `push-package` and the artifact payload.
The payload includes `generated/contracts/`, `generated/sql/`, and `generated/package-identity.json`.
Routes first renames the current `package-weld.json` file through `wamn-o7ay`.
Packaging compares installed identities and serving SQL with the frozen bytes in the artifact.
Packaging does not run the current generator to prove predecessor inputs.
Table lands its artifact commits on main. Packaging rebases from main without cross-branch pulls.
The upgrade fetches that artifact by digest and refuses a manifest or migration mismatch with the installed database.
The refusal names the mismatch. An attested Git checkout is not a fallback.
The operator pushes artifacts once for the two existing `wamn-dev` versions from their attested commits.
Later installed versions require their pushed artifacts.

The verb runs these stages in order. Each stage is a function over the run record. Each one reads the state first and skips work that is done.

1. Source. Make a clean checkout of `--commit` in a private work directory. When `--commit` is not a full hash or the checkout is not clean, refuse.
2. Build. Build the programs, the native delivery binaries and the guests into a target directory under the work directory, before any candidate exists. Record the sha256 of each guest.
3. Images. Build host, identity and ctl by source identity and push the relabeled image under the `src-` tag. Read the gates digest from the environment file. If a tag exists, read its digest and do not push. Record the four pinned references.
4. Guests. Push each distinct guest file once under its sha256 hex. If the tag exists, read its digest.
5. Preflight. Before any change to the environment: compare the CPU and memory requests of the target host groups with the allocatable capacity of the pool. Make sure that the broker runs and that `WAMN_TAP` exists. When `WAMN_TAP` is missing, run the tap Job. Read the attestations of the environment and choose the next free release id.
6. Schema. Run `upgrade-schema` on the system database and on each project database of the environment. A migration refusal stops the run.
7. Packages. Apply the packages, reconcile data access, push the components, author the wirings and publish the release under the chosen id. The gate service runs for the whole stage and for the retirement of stage 11.
8. Qualify. Prepare the candidate from the stage 2 target and run `qualify-release` with `--revision <commit>`. Keep the case results of a failed case (`wamn-e6iw`). A failed qualification stops the run.
9. Publish and select. Run `publish-qualified-release`, then one `select-release` for every environment that the run changes. A bytes refusal stops the run.
10. Deploy. Upload the web client. Render the edge values and the URL map. Render the host values from the stage 3 host image and the workloads from the stage 4 digests. Upgrade the hosts and apply the workloads. Wait for every host group and workload to be Ready.
11. Check and retire. Run the serve check: each released route of the manifest that the check names answers 401 without a credential, and an unknown path answers 404. A failed serve check stops the run. Then retire the old gate generation while the gate service holds its session. Revoke every management-author PAT of the environment that is not the current one. The verb revokes only what upgrades issue. A PAT of a user is not the verb's to revoke (owner ruling, 2026-10-02).
12. Record. Write the run record. The operations page cites it, and no person writes the record by hand.

### 4.3 Waits and retries

A port-forward that closes is opened again. A node replacement can reset a connection. A step that fails that way waits for Ready nodes and platform pods. Then it runs again once. A second failure stops the run, and the run record names the step. The broker check of stage 5 runs again before stages 8 and 10. These rules cover the Spot preemptions of §3.1. They do not replace `wamn-qjcd`.

### 4.4 Test tools in kind

Before any live run, a kind test starts a private registry that requires a login. It pushes a host image and a guest by digest. It runs `tools/registry-image-archive` and `tools/journey-image-cache` against that registry. The test makes sure that each kind node imports the image under its tag and its digest. It makes sure that CRI reports the pinned digest and that no node holds a credential.

## 5. Issues

Epic 1, one branch, each issue with its tests. No issue runs against wamn-dev until issue 9.

1. Gates image pinned (`wamn-1s38`): `compare_built_image` for host and identity only, the gates digest from the environment file.
2. Test tools in kind against a private registry (§4.4).
3. Failed case results kept (`wamn-e6iw`).
4. The environment file and its two `wamn-dev` instances (§4.1). `host_values_files.rs` takes the host image as an argument.
5. The run record and the resume rule (§2, §4.2 stage 12).
6. Stages 1 to 5: source, build, images, guests, preflight.
7. Stages 6 to 9: schema, packages, qualify, publish and select, with the next free release id.
8. Stages 10 to 12: deploy, serve check, retirement, record, with the waits of §4.3.
9. A kind run of stages 1 to 9 against a disposable stack and a private registry, with no switches, no MinIO and no second edge. Stages 10 to 12 have no kind test. The first live run on wamn-dev, for Receiving and WMS at one commit, is their test (owner ruling, 2026-10-02). The live run records its times beside §3.1.

## 6. Out of scope

- Epic 2, `create-environment`: provision a new environment from nothing with the same environment file and run record. Named here only.
- Non-Spot nodes for the broker and the database (`wamn-qjcd`).
- A long-lived gate service. Until it exists, the verb runs the gate service for stages 7 and 11.
- A CI provider that calls the verb (`docs/plan/delivery.md`).
- Migrations outside the accepted additive [package upgrade path](package-upgrade.md).
