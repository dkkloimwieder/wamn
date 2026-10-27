# Platform install

## How this plan is worked

The rules of `platform-deploy.md` "How this plan is worked" apply unchanged, plus rule 7 (never run `gcloud` or `kubectl` outside `CLOUDSDK_CONFIG=~/.config/gcloud-wamn-agent` and `KUBECONFIG=~/.kube/wamn-dev`).

## 1. Goal

One verb installs the platform layer of a cluster from one document, the same way `env apply` installs an environment:

```
wamn-ctl platform apply <cluster.k>
```

`gcp.md` §4 becomes: owner inputs, `platform apply`, done. The runbook keeps §1 (owner inputs), §2 (project resources, owner-only IAM), §3 (teardown) and §5 (operations). Nothing in §4 stays prose.

One cluster document names one org. Multi-org is out of scope.

## 2. Rulings (proposed)

| # | Ruling | Why |
| --- | --- | --- |
| P1 | **The cluster is a declared document, `cluster.k`.** It names: provider (`gcp`), project id, zone, cluster name, org, owner email, registry, buckets (web, otel), the control route host, and the Resend sender address. Nothing else. `environment.k` owns each environment's route host (R2). The platform version is `deploy/platform/release/Chart.yaml`, which `platform apply` reads (R15). The cluster itself (node pools, network, subnetwork, secondary ranges, node service account, release channel) is declared in `project.k` (§7) and made by OpenTofu (E0). The invite step reads the owner email. Every Kubernetes name, namespace, Service and Secret name derives from it. | Same shape as `environment.k` (R21). `show` reproduces it; `apply` of `show`'s output is a no-op. |
| P2 | **`platform apply` is the one verb.** It runs §4.3–§4.21 in order under one lock, idempotent: every step checks the live state first and does nothing when it already matches. It stops at the first missing owner input and names it. `--dry-run` prints the plan. | One owner for the install; a rerun after a fix continues where it stopped. |
| P3 | **Owner inputs are read, never made.** The quota preference, the Resend key in Secret Manager (the verb writes the cluster Secret from it at 4.12, owner ruling 2026-10-23), the `wamn-blob` Workload Identity binding, DNS records, IAM. The verb reads each at the step that needs it and refuses with the exact command the owner runs. The verb never calls IAM, DNS or billing. The agent, not the verb, holds the custom role `wamnAgentIam` with a condition that admits only the `wamn-edge` service account and the `wamn-dev-web` bucket, plus `iam.serviceAccounts.create` on the project, and it runs those IAM commands one at a time after the owner approves each (owner ruling 2026-10-23). | The agent identity holds IAM only on the `wamn-edge` resources, never on itself; any other IAM step is a runbook bug (standing rule, amended 2026-10-23). |
| P4 | **No shell glue inside the verb.** `kubectl` and `helm` run as subprocesses with typed inputs and JSON outputs, and they are the API. The verb serializes typed objects and pipes them to `kubectl apply`, and it parses `-o json` back into typed structs. `env apply` uses the same `kubectl` mechanism. The verb runs two `gcloud` commands: `Provider::kubeconfig` for `gcp` is `gcloud container clusters get-credentials`, and `Provider::resend_key` for `gcp` is `gcloud secrets versions access` of the Resend key. Nothing else. No Kubernetes, GKE or Google auth client crate. No `sed`, no `jq`, no here-documents. | The runbook's drift was in shell glue. |
| P5 | **The control host and the edge are platform objects.** `platform apply` installs the control host of the document's org and renders the edge values from the control route host and the registry's environment rows. `host-group-control.yaml`, `values-edge.yaml` and `url-map.yaml` stop being hand-edited files. | Added to the runbook on 2026-10-19 by M1.4; a second environment would mean editing three files by hand. |
| P6 | **Teardown stays owner-run.** `wamn-ctl platform delete <cluster.k>` exists, refuses without `--data`, and the runbook §3 says "the owner runs this". | Data loss is the owner's. |
| P7 | **The verb is tested by running it.** A system test installs a kind cluster from a `cluster.k` with provider `kind` (no GKE API, no buckets, no DNS; local registry) end to end, then applies one environment. The GKE path is proved by the greenfield (P8), not by a unit test. One `Provider` trait owns how the kubeconfig arrives, `Provider::kubeconfig`: `gcp` runs `gcloud container clusters get-credentials` (P4), `kind` runs `kind create cluster`. Every step after it is provider-free. | A verb that is never run rots like a runbook. |
| P9 | **No port-forward.** The verb runs outside the cluster. Steps 4.3–4.6 run `kubectl` and `helm` from the verb. Every step that needs Postgres or NATS runs as a Kubernetes Job that the verb creates and waits on: the `mint-pat` pattern, generalized. Today §4.7–§4.17 run through three background `port-forward`s that drop. | Two "command hangs" in one week. |
| P10 | **Secrets are objects the verb writes.** `wamn-system-admin`, the session key, the operator PAT: the verb creates them through the API with owner references set in the object, no `jq`, no file. The Resend key stays an owner input, read by name. | The newline bug and the `jq` ownerReference patch are the same class. |
| P11 | **Qualification is a release-time step on the build machine.** `qualify-release` runs where `build-platform` runs, the machine with Docker, and never in the cluster. `platform apply` applies only qualified releases, and 4.17 refuses an unqualified one with the `qualify-release` command. The kind test runs `qualify-release` on the host for the smallest package before 4.17. The greenfield applies releases that are already qualified and never qualifies. Owner ruling 2026-10-21. | The cases make their own kind clusters through the Docker socket, which a Job pod does not have. |
| P12 | **The agent runs the project layer.** The agent renders `deploy/gcp/project.k`, runs `tofu plan` as `wamn-agent@wamn-dev`, posts the plan, and runs `tofu apply` of that saved plan after the owner approves it, the same approval as a cluster resize. This covers the cluster and pools of §4.2 and the load balancer of §4.20. The verb itself never runs OpenTofu. Owner ruling 2026-10-23. | An owner OpenTofu step stops every greenfield. |
| P8 | **Greenfield on demand.** The job is started by hand until the destroy policy of §7 item 4 exists; P6 stands. Later it is weekly. The job: teardown wamn-dev, `platform apply`, `env apply` both environments, the check, pools to 0. Its report is one line per step with pass or the refusal. The owner reads it Monday. | Drift is found by the schedule, not by the owner waiting on a deploy. |

## 3. What `platform apply` owns (from gcp.md §4)

| Step | Today | Under the verb |
| --- | --- | --- |
| 4.1 binaries | shell | the verb is the binary |
| 4.2 cluster, pools | gcloud | tofu, from `project.k` (E0) |
| 4.3 cert-manager, edge certificate | helm, kubectl | `helm`, `kubectl`; wildcard certificate of the control host's domain |
| 4.4 namespaces, operator, CA, collector | helm, kubectl | `helm`, `kubectl`; values rendered from the document |
| 4.5 PostgreSQL | kubectl, CNPG | CNPG CR from the document's policy; pd-balanced |
| 4.6 packages, platform build | cargo, wamn | unchanged commands, invoked by the verb; the verb runs `--stamp-only` and refuses a version that the registry does not hold. An operator builds each new version once with `build-platform` (owner ruling 2026-10-20) |
| 4.7 system database, org, issuer | wamn-ctl verbs | same verbs, called in order |
| 4.8 event NATS | helm, kubectl | Helm SDK |
| 4.9–4.11 platform inputs, file, first apply | wamn-ctl | `env apply` for each environment of the registry's environment rows, `release = none` |
| 4.12 identity, session key | kubectl, Secrets | Secrets written by the verb; Resend Secret read (P3) |
| 4.13–4.17 packages, releases, qualification, apply | wamn verbs | same verbs; package versions come from the environment files, not from §4.13 |
| 4.18 CDC readers | kubectl | objects from the document |
| 4.19 web clients, control host, platform UI | wamn web upload, helm | the verb; P5 |
| 4.20 edge, load balancer, url-map | helm, gcloud | the verb; DNS stays owner (P3) |
| 4.21 check | curl, kubectl | the verb's last step; non-zero exit names the failing check |

## 4. What stays out

- IAM, DNS, billing, quotas, API enables: owner, by hand, named in §1.
- The first org admin's invitation: `platform apply` runs the invite Job for the owner address in the document (today's §5.3), so sign-in works on first install. Other users come from the platform UI.
- Multi-cluster, multi-region, non-GKE providers beyond `kind`.
- Backup and point-in-time recovery: unchanged, `backup-and-recovery.md`.

## 5. Epics

One at a time, E0 first; each ≤3 issues; build+clippy+fmt+unit per issue; the kind test (P7) at the merge of E2 onward.

| # | Epic | Done when |
| --- | --- | --- |
| E0 | Project layer: `project.k` schema (cluster, pools, network, subnetwork, ranges, node service account, release channel), `.tf.json` rendering by the generator, the state bucket, `tofu plan` and `tofu apply` run by the agent as `wamn-agent@wamn-dev` after the owner approves the posted plan (P12). | wamn-dev's cluster and pools are under tofu state, and `tofu plan` shows no diff. |
| E1 | `cluster.k` schema, `show`, `--dry-run`. The document for wamn-dev is committed. | `platform apply --dry-run` prints every step of §4 with its inputs and the owner inputs it will read. |
| E2 | The kubeconfig `Provider` and the lock; steps 4.3–4.5 and 4.8 (cert-manager, operator, collector, PostgreSQL, NATS) through `helm` and `kubectl`. Provider `kind` for the test. | The kind test reaches a ready PostgreSQL and NATS from `cluster.k`. |
| E3 | Steps 4.6–4.7, 4.9–4.17 (build, system database, environments, identity, packages, releases, apply). | The kind test applies one environment with a release and passes the check. |
| E4 | Steps 4.18–4.21, P5 (CDC, web, control host, platform UI, edge, url-map, check). Delete `host-group-control.yaml`, `values-edge.yaml`, `url-map.yaml`. | A GKE greenfield of wamn-dev runs `platform apply` once and the owner signs in at the control host. |
| E5 | P6 delete; P8 the greenfield job (manual trigger); gcp.md §4 reduced to one command; §1 lists only owner inputs. Plan index updated. | Two consecutive greenfield runs pass with no owner intervention. |

## 6. Acceptance

- [ ] `gcp.md` §4 is one command and no shell.
- [ ] `platform apply` of the committed `cluster.k` on an installed cluster is a no-op.
- [ ] The kind test runs in the per-epic budget from E2.
- [ ] Two greenfield runs pass.
- [ ] No file under `deploy/gcp/` is hand-edited for a second environment or org.

## 7. The project layer

Item 1 starts in E0 with the cluster and its pools. The rest is not part of this plan, and is recorded so P3 is designed for it.

The owner inputs of §1 are project-level resources that need IAM. They leave interactive hands in this order:

1. `deploy/gcp/project.k` declares the project layer: the cluster and its pools (E0), then GSAs, Workload Identity bindings, DNS zone and records, buckets and lifecycle, quota preferences, Artifact Registry. The generator renders it to `.tf.json`, which OpenTofu reads natively; nobody writes HCL. OpenTofu is an execution engine here, as Helm is: it holds the state file in a bucket and computes the diff. CI runs `tofu plan` with its own short-lived identity; the owner approves a diff when one exists, not a run.
2. Secrets from Secret Manager through External Secrets Operator. The Resend key is one Secret Manager secret; rotation is a new version.
3. DNS from the cluster through external-dns with a DNS-scoped GSA. The edge Service carries the hostname; the record is derived from `cluster.k`.
4. Teardown by policy: `env_policies.destroy` is `auto` for dev and `never` for prod. Prod data loss stays a human act.

Authored formats stay one: KCL. `wamn.k`, `environment.k`, `cluster.k`, `project.k`. JSON, YAML and `.tf.json` are derived and never committed (platform-deploy R10). Config Connector was considered and set aside: it puts a project-level identity inside the cluster.

After that the human acts are: approve a project-layer diff, and prod-destructive operations. `platform apply` reads these resources by name (P3) whether the owner or OpenTofu made them, so this plan needs no change when they arrive.
