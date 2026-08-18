# deploy/ — tiered by lifecycle (SR8, findings §1.6)

Four tiers. A new file goes in exactly one tier; nothing lands at
the top level. When in doubt, ask which lifecycle owns the file's create/delete.

- **`infra/`** — install-once cluster infrastructure, applied by hand at cluster
  standup and rarely touched: operators (CNPG, barman plugin), the data-plane
  NATS, development observability inputs (Tempo/otel/MinIO), kind config, and
  the runtime-operator's own Helm values (`values-wamn.yaml`).
- **`platform/`** — long-lived production/platform manifests the control plane
  or an operator owns: dispatcher, production executor (`runner`), registry, wamn-sysdb,
  credential `*.example` Secrets, the shared postgres fixture, runner
  NetworkPolicy + environment connection-policy example, and the per-environment
  runtime-operator host-tier Helm values (`values-host-*.yaml`).
- **`gates/`** — gate/bench Job manifests (`*-job.yaml`) and their support
  Deployments (`serve-echo`, `egress-escape`). Applied per gate run, deleted
  after.
- **`sql/`** — the standalone SQL schemas (`postgres-init`, `app-schema`,
  `catalog-schema`, `system-schema`, `run-queue`, `run-state`).
  Several are `include_str!`'d or read by tests — paths are load-bearing
  (SR13 tracks generating these from Rust instead of hand-maintaining them).

Placement judgment calls, recorded: `postgres.yaml` is platform (the shared
long-lived fixture ~8 gates and the dispatcher point at, despite its bench
header); `serve-echo` is gates (gate support, not product);
`publish-catalog-job.yaml` is gates (driven through production `wamn-ctl`).

One chart, two tiers (wamn-0h0g.15.15, rulings `.13.49` + `.13.50`): the
runtime-operator chart is installed twice with different values, and the tier
split *is* the ruling. `infra/values-wamn.yaml` is the cluster-singleton
operator release — the five CRDs are cluster-scoped and Helm installs them once,
so it is install-once by construction and carries no host groups.
`platform/values-host-<environment>.yaml` is the host tier, one Helm release per
environment, because host images and host groups are per-environment and
per-release values. Install the operator release first; a host release applied
before its CRDs exist has nothing to register into.
