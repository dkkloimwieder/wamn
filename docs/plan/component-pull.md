# Component pull

Updated through: 2026-09-30, `main` at `1896f3c99`.

## 1. Goal

A host on GKE pulls its release manifest and its components with a short-lived token. The host gets the token from the metadata server of its Workload Identity, at the time of the pull. No Secret holds a registry credential. The registry token CronJob, its Secret `wamn-registry-pull`, its Role and its Google service account `wamn-registry-reader` go away (finding `wamn-i87m`).

The change needs no fork of wasmCloud and no patch of it. The pinned revision offers a way in: a Docker credential helper (section 3.2).

## 2. Fixed rules

- No fork of wasmCloud and no upstream patch in the tree. wash-runtime stays at `fd2bbc0a`.
- No Secret and no file holds a registry token. Each pull asks for the token it uses.
- No service account key.
- The kind clusters keep the projected docker configuration file for their in-cluster registry. Only the GKE host changes.
- The operator verbs of `wamn-ctl` (`push-component`, `promote`, the delivery verbs) keep `--registry-auth-file`. They run by hand, outside the cluster.
- A host that is set up for the metadata server and cannot get a token refuses the pull with a named outcome. It never falls back to an anonymous pull.

## 3. Current state

Measured on `main` at `1896f3c99` on 2026-09-30.

### 3.1 Our pull path

| Place | Today |
| --- | --- |
| Pull consumers in the host | Two readers in `wamn-runtime`, both on `oci-client` 0.17: the release manifest at startup (`crates/platform/runtime/src/release_manifest_source.rs:193-211`, called from `services/host/src/host.rs:404-423`) and the digest-verified node components (`crates/platform/runtime/src/component_artifact_source.rs:108-115`, set up at `services/host/src/host.rs:936-954`). Neither goes through wash-runtime's pull code. |
| Credential form | `read_registry_credentials` reads the file named by `WAMN_REGISTRY_AUTH_FILE` and takes `auths.<registry>.username` and `password` only (`crates/platform/runtime/src/registry_credentials.rs:152-189`). It refuses a helper, an identity token and a base64 `auth` entry. `docs/architecture/capabilities.md` ("Registry credentials") states this rule. |
| When the file is read | Once, at startup. Both readers turn the file into a fixed `RegistryAuth::Basic` and keep it. |
| When components are pulled | On the first request to an application, not at startup: `released_application` pulls inside `get_or_try_init` (`crates/execution/host/src/operation.rs:453-470`). |
| The consequence on GKE | The host keeps the token that the file held when the pod started. A first request to an application after that token expires fails its pull, although the CronJob wrote a new token into the Secret. The kind clusters do not have this defect, because their file holds a fixed password. |
| The alignment rule | `docs/architecture/native-alignment.md` keeps our own reader until native code "accept[s] that path without global environment changes or broader credential forms". |

### 3.2 wash-runtime's pull path at `fd2bbc0a`

| Place | Today |
| --- | --- |
| Pull consumers | The washlet pulls the components of a `WorkloadDeployment` through `oci::pull_component` (`crates/wash-runtime/src/oci.rs:600-625`). On GKE these are `flow-http`, `materializer`, `wms-flow-http` and `wms-materializer` (`deploy/gcp/*.yaml`). |
| Credential order | `CredentialResolver::resolve_credentials` (`oci.rs:462-495`) tries the explicit pull secret of the workload, then `docker_credential::get_credential(registry)`, then an anonymous pull. Our `WorkloadDeployment`s carry no pull secret, so the second step decides. |
| What `get_credential` accepts | `docker_credential` 1.4.0 reads `$DOCKER_CONFIG/config.json` on every call. For the exact registry key it takes, in order: a `credHelpers` entry, an `identitytoken`, an `auth` entry, a `username` and `password`, and a `credsStore` (`src/lib.rs:109-148`). |
| The helper | For a `credHelpers` entry it runs `docker-credential-<name> get` from `PATH`, writes the registry on standard input and reads `{"Username","Secret"}` from standard output (`src/helper.rs:13-58`). wash-runtime refuses the answer if `Username` is `<token>`, which is an identity token (`oci.rs:503-505`). |
| A callback | None. `OciConfig` takes a fixed username and password only (`oci.rs:260-288`). |
| A token file | None apart from `config.json` itself. The file is read on each pull, so a writer that refreshes it would work, but that writer is the CronJob again. |

The helper is the way in. It runs once per pull, and `config.json` names it without holding a credential.

### 3.3 GKE today

| Place | Today |
| --- | --- |
| The CronJob | `deploy/gcp/registry-token.yaml` runs every 30 minutes as `hosts/registry-token`, which Workload Identity binds to `wamn-registry-reader` (`roles/artifactregistry.reader` on repository `wamn`). It reads the metadata server token and patches the Secret `wamn-registry-pull` with user `oauth2accesstoken` for `us-central1-docker.pkg.dev`. |
| The host pod | Mounts `wamn-registry-pull` at `/etc/wamn/registry` and sets `DOCKER_CONFIG` and `WAMN_REGISTRY_AUTH_FILE` to it (`deploy/gcp/values-host.yaml:19-22`, `:98-100`, `:137-138`, and the same for the second host group). |
| The host's own identity | The host's Kubernetes service account already carries the Workload Identity annotation for `wamn-blob@wamn-dev.iam.gserviceaccount.com` (`deploy/gcp/values-host-base.yaml:131-133`). The host can reach the metadata server today, but `wamn-blob` has no read role on the registry. |
| The host image | `debian:trixie-slim` with `/usr/local/bin/wamn-host` only (`Dockerfile:156-160`). No credential helper is in it. |

## 4. Design

### 4.1 One token source for both paths

The host gets its registry token from the GKE metadata server: `GET http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token` with `Metadata-Flavor: Google`. The token is the `access_token` field. The registry credential is `oauth2accesstoken` and that token, the form Artifact Registry accepts and the CronJob writes today. The token is read for each pull and is never written to a file or a log.

### 4.2 Our readers

`wamn-host` gains a second, explicit credential source beside `--registry-auth-file`: the metadata server. The two are exclusive, and a release-backed host needs exactly one. With the metadata source, `ReleaseManifestSource` and `ComponentArtifactSource` ask for a token at each pull instead of keeping a fixed `RegistryAuth`. A failed token request is a named refusal of the pull. This also removes the stale-token defect of section 3.1.

The accepted forms of `read_registry_credentials` do not change. `capabilities.md` gains one rule: a host set up for the metadata server reads no credential file.

### 4.3 wash-runtime's reader

The host image gains one executable, `docker-credential-<name>`, on `PATH`. It answers `get` with `{"Username":"oauth2accesstoken","Secret":"<token>"}`, where the token comes from the metadata server. A ConfigMap mounted at `DOCKER_CONFIG` holds `{"credHelpers":{"us-central1-docker.pkg.dev":"<name>"}}`. The ConfigMap holds no credential. wash-runtime runs the helper at each pull.

### 4.4 Deployment

- The host's Google service account gets `roles/artifactregistry.reader` on repository `wamn`.
- The GKE host values drop the `wamn-registry-pull` volume, its mount and `WAMN_REGISTRY_AUTH_FILE`, and set the metadata source and the ConfigMap.
- `deploy/gcp/registry-token.yaml` is deleted, with its service account, Secret, Role and RoleBinding. `wamn-registry-reader` and its two IAM bindings are deleted.
- `docs/operations/gcp.md` and `docs/plan/gcp-deployment.md` lose the CronJob steps.

### 4.5 Choices for review

The owner rules on these before any code:

1. The helper: Google's `docker-credential-gcr`, pinned, or a small helper built in this tree. The answer of `docker-credential-gcr` on GKE is not measured here. It must not be `<token>`.
2. The Google service account of the host: add the read role to `wamn-blob`, or give the host its own Kubernetes service account bound to `wamn-registry-reader`.
3. The name of the new host setting, for example `--registry-auth gke-metadata` beside `--registry-auth-file`.
4. The token reuse: one request to the metadata server per pull, or reuse of a token until shortly before its `expires_in`.

## 5. Issues

One branch, after the owner rules on section 4.5. Each issue lands with its tests.

1. The metadata source in `wamn-runtime` and `wamn-host`. Unit tests against a local HTTP server that plays the metadata server: a pull sends the token as the password of `oauth2accesstoken`, a second pull asks again, a failed token request refuses the pull by name, and the two settings together refuse at startup.
2. The helper and the image. The host image carries the helper. A unit test runs `docker_credential::get_credential` against a `config.json` that names the helper and a local metadata server, and gets `UsernamePassword`.
3. The GKE deployment of section 4.4, with the docs. The live change on `wamn-dev` is a GKE step for the peer session `wamn-93`, not a test run of this branch.

## 6. Out of scope

- The kind clusters and their in-cluster registry.
- The operator verbs of `wamn-ctl` and their pushes.
- `wamn-edge`. It pulls nothing from a registry.
- A change to wash-runtime's credential order or to `OciConfig`. That is an upstream matter and needs no action here.
- Registries other than Artifact Registry on GKE.
