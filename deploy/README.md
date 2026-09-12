# Deployment files

These files supply infrastructure, platform workloads, test Jobs, and database schemas. The owning Rust libraries compose SQL fragments before installation. Use [deployment operations](../docs/operations/deployment.md) for publication and activation, and [data access](../docs/architecture/data-access.md) for database responsibilities.

- [infra/](infra/): cluster operators, certificates, NATS, and telemetry infrastructure.
- [platform/](platform/): platform workloads, environment overlays, and credential examples.
- [gates/](gates/): application-independent test Jobs and their support workloads.
- [sql/](sql/): control, catalog, run-state, and test database schema inputs.
- [mvp/](mvp/): retained bootstrap scripts that run before the other deployment groups.
