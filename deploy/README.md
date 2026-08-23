# deploy/ — tiered by lifecycle (SR8, findings §1.6)

Four tiers. A new file goes in exactly one tier; nothing lands at
the top level. When in doubt, ask which lifecycle owns the file's create/delete.
`mvp/` is the classified exception: its bootstrap scripts remain outside the SR8
lifecycle tiers because pre-tier provisioning runs before any tier exists.

- **`infra/`** — install-once cluster infrastructure, applied by hand at cluster
  standup and rarely touched: operators (CNPG, barman plugin, cert-manager), the
  data-plane NATS, development observability inputs (Tempo/otel/MinIO), kind
  config, and the runtime-operator's own Helm values (`values-wamn.yaml`).
- **`platform/`** — long-lived production/platform manifests the control plane
  or an operator owns: dispatcher, the component+wiring router executor,
  registry, wamn-sysdb, credential `*.example` Secrets, the shared postgres
  fixture, and the per-environment runtime-operator host-tier Helm values
  (`values-host-*.yaml`).
- **`gates/`** — gate/bench Job manifests (`*-job.yaml`) and their support
  Deployments (`serve-echo`, `egress-escape`). Applied per gate run, deleted
  after.
- **`sql/`** — the standalone SQL schemas (`postgres-init`, `app-schema`,
  `catalog-schema`, `system-schema`, `run-queue`, `run-state`).
  Several are `include_str!`'d or read by tests — paths are load-bearing
  (SR13 tracks generating these from Rust instead of hand-maintaining them).

Placement judgment calls, recorded: `postgres.yaml` is platform (the shared
long-lived fixture ~8 gates and the dispatcher point at, despite its bench
header); `serve-echo` is gates (gate support, not product).

cert-manager is base infrastructure, not a gate prerequisite (wamn-ergz).
`infra/cert-manager.yaml` is the upstream static install **pinned at v1.21.0**
— all three `quay.io/jetstack` images (controller, webhook, cainjector) and
every `app.kubernetes.io/version` label carry that tag. It is vendored verbatim,
as `cnpg-operator.yaml` and `barman-cloud-plugin.yaml` are, so a bump is a
re-fetch of the same URL at the new tag and the diff is purely upstream's:

```bash
curl -sSL -o deploy/infra/cert-manager.yaml \
  https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
# sha256 6e499c3f1ab356abe79a7853911f80cb09c213885bfdf81092fdff142ba63c4a
```

Apply it **before** `barman-cloud-plugin.yaml`, which renders `cert-manager.io/v1`
`Issuer` and `Certificate` CRs and cannot be admitted until the CRDs and the
webhook exist. Gate runbooks apply this file rather than an external
cert-manager release URL — the D6 runbook still names the upstream URL, and
swapping it is tracked on wamn-loha:

```bash
kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
```

What it installs: six CRDs, and the controller/webhook/cainjector in namespace
`cert-manager`. What it does **not** do: issue a single certificate, or define
any `Issuer`/`ClusterIssuer` of ours. Issuers belong to their consumers — the
barman plugin ships its own `selfsigned-issuer` in `cnpg-system`, and the two
consumers that make this a bill-of-materials decision rather than a rider on a
TLS change each bring their own: wamn-0h0g.15.152 (the per-environment
ClusterIssuer distributing the chart CA) and wamn-0h0g.15.155 leg B (the
registry's CA `Issuer` plus serving `Certificate`).

One chart, two tiers (wamn-0h0g.15.15, rulings `.13.49` + `.13.50`): the
runtime-operator chart is installed twice with different values, and the tier
split *is* the ruling. `infra/values-wamn.yaml` is the cluster-singleton
operator release — the five CRDs are cluster-scoped and Helm installs them once,
so it is install-once by construction and carries no host groups.
`platform/values-host-<environment>.yaml` is the host tier, one Helm release per
environment, because host images and host groups are per-environment and
per-release values. Install the operator release first; a host release applied
before its CRDs exist has nothing to register into.
