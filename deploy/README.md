# deploy/ — tiered by lifecycle (SR8, findings §1.6)

Five tiers plus `cred/`. A new file goes in exactly one tier; nothing lands at
the top level. When in doubt, ask which lifecycle owns the file's create/delete.

- **`infra/`** — install-once cluster infrastructure, applied by hand at cluster
  standup and rarely touched: operators (CNPG, barman plugin), the data-plane
  NATS, observability backends (Loki/Tempo/otel/MinIO), kind config, Helm values.
- **`platform/`** — long-lived production/platform manifests the control plane
  or an operator owns: dispatcher, runner, registry, wamn-sysdb,
  api-gateway/trace-relay workloads, credential `*.example` Secrets, the shared
  postgres fixture, `event-reader.example.yaml`, `hello-workload.yaml`.
- **`gates/`** — gate/bench Job manifests (`*-job.yaml`) and their support
  Deployments (`serve-echo`, `serve-node-gate`); `gates/ladder/` holds the
  exec-ladder rung flows. Applied per gate run, deleted after.
- **`poc/`** — POC assets (f1 flow/seed/workloads/provision Job, the
  material-receiving catalog/RLS/seed JSON, `proof-catalog.json`).
- **`sql/`** — the standalone SQL schemas (`postgres-init`, `app-schema`,
  `catalog-schema`, `system-schema`, `run-queue`, `run-state`, `flows`).
  Several are `include_str!`'d or read by tests — paths are load-bearing
  (SR13 tracks generating these from Rust instead of hand-maintaining them).
- **`cred/`** — credproof fixture flows (unchanged by the tiering).

Placement judgment calls, recorded: `postgres.yaml` is platform (the shared
long-lived fixture ~8 gates and the dispatcher point at, despite its bench
header); `serve-echo`/`serve-node-gate` are gates (gate support, not products);
`f1-provision-job.yaml` is poc (f1 asset despite the `-job` suffix);
`publish-catalog-job.yaml` is gates (driven by `wamn-gates`).
