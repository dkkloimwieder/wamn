# Deployment files

These files supply infrastructure, platform workloads, test Jobs, and database schemas. The owning Rust libraries compose SQL fragments before installation. Use [deployment operations](../docs/operations/deployment.md) for publication and activation, and [data access](../docs/architecture/data-access.md) for database responsibilities.

- [infra/](infra/): cluster operators, certificates, NATS, and telemetry infrastructure.
- [platform/](platform/): platform workloads, environment overlays, and credential examples.
- [gcp/](gcp/): the Google Cloud project and cluster definitions, environment definitions, workload manifests, Helm values, operator Jobs, and the cost guard function.
- [gates/](gates/): application-independent test Jobs and their support workloads.
- [sql/](sql/): control, catalog, run-state, and test database schema inputs.
- `deploy/mvp/ at 1bff09574`: retired bootstrap scripts that ran before the other deployment groups.
