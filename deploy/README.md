# Deployment files

These files supply infrastructure, platform workloads, test Jobs, and database schemas. The owning Rust libraries compose SQL fragments before installation. Use [deployment operations](../docs/operations/deployment.md) for local platform deployment, publication, and activation, and [data access](../docs/architecture/data-access.md) for database responsibilities.

- [infra/](infra/): cluster operators, certificates, NATS, and telemetry infrastructure.
- [platform/](platform/): platform workloads, environment overlays, and credential examples.
- [kind/](kind/): the local cluster document and registry Service.
- [gcp/](gcp/): Google Cloud project and cluster definitions, application inputs, and the cost guard function. Deployment integration remains unfinished.
- [sql/](sql/): control, catalog, run-state, and test database schema inputs.
- `deploy/mvp/ at 1bff09574`: retired bootstrap scripts that ran before the other deployment groups.

Platform deployment consumes a prepared chart with pinned artifacts and bundled resources.
It needs no application build, UI bundle, or source checkout.
[Local platform deployment](../docs/operations/deployment.md#local-platform) covers registry preparation, deployment, status, access, and cleanup.
