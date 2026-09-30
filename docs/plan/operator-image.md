# Operator image

Updated through: 2026-09-30, `main` at `91a6ffca9`. Findings `wamn-n5d1` and `wamn-lo7z`. The owner reviews this spec before any code.

## 1. Goal

The verbs that call identity run inside the cluster, from an image that holds only the operator tooling. The owner's machine then needs no `/etc/hosts` line and no identity port-forward for them. A commit that changes only test files keeps that image, and only the test image builds again.

The change adds no new verb and no new rule. It publishes an image stage that exists today and gives it the cache identity of the host. It adds one Job manifest for each verb that calls identity.

## 2. Fixed rules

- The operator image carries `wamn-ctl` and `wamn-ctl-ops` and nothing else of the workspace. It is pinned by digest, as the host and identity images are.
- The operator certificate reaches the Job only as a mounted Secret. It never passes through the owner's machine.
- The Job reaches identity by its Service name on port 443, and it trusts only the CA of the serving certificate.
- No credential goes to a Job log. A verb that writes a credential writes it to a mode 0600 file, as today.
- A Job's log is saved to the scratchpad before the Job is deleted.
- The verbs get no Kubernetes client (out of scope in `docs/plan/environment-teardown.md` section 6).

## 3. Current state

Measured on `main` at `91a6ffca9` on 2026-09-30.

| Place | Today |
| --- | --- |
| Verbs that call identity | Two, both in `wamn-ctl` without features. `invite` posts to `<issuer>/invitations` and needs no database (`services/ctl/src/identity_verbs.rs:96-167`). `provision-project-env` calls identity only with `--emit-management-author-pat-secret` or `--emit-operator-pat-secret` (`services/ctl/src/provisioning_verbs.rs:183-199`), and then also needs `--system-database-url` (`crates/control/lib/src/provision_project_env.rs:419-429`). `create-human`, `provision-identity-issuer` and the membership verbs use the database only. No `wamn-ctl-ops` verb calls identity. |
| Identity flags | `--pat-issuer`, `--pat-client-cert`, `--pat-client-key`, `--pat-server-ca`, with the variables `WAMN_PAT_*` (`provisioning_verbs.rs:563-591`). The client is HTTPS only, with no redirect and no retry (`crates/control/lib/src/pat_client.rs:68-116`). |
| Verb output | `invite` prints `invitation for <id>: <status> <body>` to stdout and writes no file. `provision-project-env` writes each PAT Secret as a mode 0600 file, refuses stdout for it (`crates/control/lib/src/provision_project_env/output.rs:10-15, 87-101`), and prints `wrote <path> (<purpose> PAT Secret; kubectl apply)`. Logs go to stderr. |
| Operator certificate | If the client certificate of a connection chains to the operator CA, identity treats the connection as an operator (`services/identity/src/lib.rs:345-410`). Only `POST /pats` and `POST /invitations` need it. The client certificate is the Secret `operator-dkk` in namespace `identity`, from the namespaced Issuer `identity-operator` (`deploy/gcp/identity-operator-ca.yaml`). |
| Identity address | Service `identity` in namespace `identity`, port 443 to container port 8443. The serving certificate names only `identity.identity.svc.cluster.local` (`deploy/gcp/identity-certificate.yaml:12-13`). Its CA is `ca.crt` of the Secret `identity-tls`. |
| Hosts-file shortcut | Sections 3.16 and 4.5 of `docs/operations/gcp.md` add `127.0.0.1 identity.identity.svc.cluster.local` to `/etc/hosts` and forward `svc/identity 8443:443`. They copy `operator-dkk` into mode 0600 files. Then they run `target/debug/wamn-ctl` on the owner's machine with `--pat-issuer https://identity.identity.svc.cluster.local:8443`. |
| `ctl` stage | `Dockerfile:165-170`: `debian:trixie-slim`, `wamn-ctl` and `wamn-ctl-ops` from `build-ctl`, no `pg_dump`. No tool builds it. `tests/conformance/src/repo_policy/docker_provenance.rs:135, 184` checks its contents. Three examples in `deploy/platform` name `wamn-ctl:dev`. |
| `gates` stage | `Dockerfile:194-215`: `FROM host`, with `wamn-gates`, `wamn-ctl`, `wamn-ctl-ops`, `wamn-cdc-reader` and five bench components. The gates cases run `wamn-ctl` and `wamn-cdc-reader` inside it through their executable boundary. `tools/identity-jwks-journey-run` runs `wamn-ctl` in a sleeping Pod of this image and reads its files back with `kubectl exec cat` (lines 235-259). |
| Cache identity | `tools/journey-image-cache:80-93` leaves the test-only paths out of the identity for `host` and `identity` only. `ctl`, `gates` and `cdc-reader` hash the whole copied tree, so a test edit gives them a new identity. |
| Jobs that run `wamn-ctl` | Only the three examples in `deploy/platform`. They take their arguments in `args`, their credentials by `secretKeyRef`, mount no token, and are read with `kubectl logs`. `deploy/gcp` has none. |

The superuser URL of `wamn_system` exists only on the owner's machine, built from the Secret `wamn-pg-superuser` in namespace `platform` (`gcp.md` section 3.6).

## 4. Design

### 4.1 The image

The operator image is the `ctl` stage, published as `wamn-ctl:src-<identity>` in `us-central1-docker.pkg.dev/wamn-dev/wamn`. `tools/journey-image-cache` gives `ctl` the identity rule of `host` and `identity`, and `tests/conformance/tests/journey_image_cache.rs` covers it. A commit that changes only test files then keeps the operator image.

The `gates` stage keeps its contents, because its cases run `wamn-ctl` inside it. It keeps the full identity, and it is the only image that a test edit builds again. The build time of `gates` after a test edit is not measured here. Issue 1 measures it.

### 4.2 The Jobs

Two manifests under `deploy/gcp/operator/`, both in namespace `identity`, where `operator-dkk` and `identity-tls` live:

- `invite.yaml` runs `wamn-ctl invite --principal <principal id>`.
- `mint-pat.yaml` runs `wamn-ctl provision-project-env` with the PAT flags of section 3.16.

Each Job has `backoffLimit: 0`, `restartPolicy: Never` and `automountServiceAccountToken: false`. It mounts `tls.crt` and `tls.key` of `operator-dkk` and `ca.crt` of `identity-tls` read-only at mode 0400, and sets `WAMN_PAT_ISSUER=https://identity.identity.svc.cluster.local`.

### 4.3 Arguments and output

The fixed flags are in the manifest. The run values are placeholders in `args`: the principal id, or the triple and the tenant. The operator fills them in a private copy of the manifest before `kubectl apply`, as for the `deploy/platform` examples.

`invite` writes no credential. The operator waits for the Job to complete, saves `kubectl logs job/<name>` to the scratchpad, and deletes the Job.

`mint-pat` writes a PAT Secret file. The file must reach the owner's machine. The bench PAT is used on the client VM, and the management-author PAT is not applied (`gcp.md` section 3.16). The container writes it to a memory `emptyDir`, then waits. The operator copies it with `kubectl exec cat` into a mode 0600 file, as `tools/identity-jwks-journey-run` does, and deletes the Job. The verb also needs the superuser URL of `wamn_system`. The operator makes the Secret `wamn-system-admin` in namespace `identity` from `wamn-pg-superuser` before the run, and deletes it after.

### 4.4 What the hosts-file shortcut becomes

It goes. Sections 3.16 and 4.5 of `gcp.md` become the Job runs of section 4.3. The `/etc/hosts` line and the identity port-forward on 8443 are no longer needed for these verbs, and the owner removes the line from the machine. The port-forward stays for other checks until nothing reads it.

## 5. Issues

One branch. Each issue lands with its tests.

1. The image. `ctl` gets the host identity rule in `tools/journey-image-cache` and its test. Measure the `gates` rebuild after a test-only commit, before and after.
2. The Jobs. `deploy/gcp/operator/invite.yaml` and `mint-pat.yaml`, and sections 3.16 and 4.5 of `gcp.md` rewritten for them.
3. The run on wamn-dev. Build and push `wamn-ctl:src-<identity>`, pin it in both manifests, and run each Job once.
4. Closeout. Close `wamn-n5d1` and `wamn-lo7z` with the commits and the runs.

## 6. Out of scope

- A Kubernetes client in the control verbs.
- A non-root user in the images. No stage sets `USER` today.
- The `copy-project-env` verb, which needs `pg_dump` and `pg_restore`.
- The other verbs of `gcp.md` that run from the owner's machine through the Postgres port-forward. They call no identity.
