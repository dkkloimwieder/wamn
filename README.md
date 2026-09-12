# WAMN

WAMN provides application data, component execution, and generated operator interfaces on wasmCloud.
Applications declare their data and operations in `wamn.json`.
Rust owns platform behavior, and PostgreSQL stores its durable state.

Start with the [documentation index](docs/README.md) or the [architecture overview](docs/architecture/overview.md).

## Repository

| Path | Owner |
| --- | --- |
| [apps/wamn_receiving](apps/wamn_receiving/README.md) | Receiving manifest, migrations, guest, generated code, operator UI, and tests |
| [apps/wamn_wms](apps/wamn_wms/README.md) | WMS manifest, migrations, guest, generated code, example, and tests |
| [apps/client_acme_receiving](apps/client_acme_receiving/README.md) | Acme Receiving overlay and its tests |
| [apps/platform](apps/platform/) | Shared platform guests and guest libraries |
| [services](services/) | Deployable native processes and their service tests |
| [crates](crates/) | Platform libraries, grouped by responsibility |
| [tests](tests/) | Conformance, integration, system, and orchestration test owners |
| [test-support](test-support/) | Shared test functions, fixtures, and infrastructure |
| [deploy](deploy/) | Infrastructure, platform manifests, test Jobs, and SQL |
| [docs](docs/README.md) | Architecture, operations, testing methods, and plans |

## Development and operations

The [operations index](docs/operations/README.md) connects commands to their prerequisites and cleanup rules.
The application READMEs above link each app's scenario, source, generated output, and tests.
Select the relevant instructions:

- [Building](docs/operations/building.md): Pinned toolchains, native binaries, guest selection, and isolated worktrees.
- [Development loop](docs/operations/development-loop.md): Developer sessions, operator interfaces, and the authoring pilot.
- [Running tests](docs/operations/running-tests.md): Test selection, database isolation, generation, SQLx preparation, and live inputs.
- [Cluster tests](docs/operations/cluster-tests.md): Owned Receiving, WMS, and native test fixtures.
- [Deployment](docs/operations/deployment.md): Release qualification, publication, selection, and activation.
