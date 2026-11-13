# Repository tasks

Run `cargo xtask --help` from the repository root. The `justfile` calls these commands.

| Former entry point | Command |
| --- | --- |
| `wamn-ctl build-platform` | `cargo xtask build-platform` |
| `test-changes` | `cargo xtask test-changes` |
| `require-test-result` | `cargo xtask require-test-result` |
| `repo-lint` | `cargo xtask repo-lint` |
| `repo-lint-dependencies` | `cargo xtask repo-lint-dependencies` |
| `compile-packages-alone` | `cargo xtask compile-packages-alone` |
| `contract-diff` | `cargo xtask contract-diff` |
| `wasmcloud-release-tests` | `cargo xtask wasmcloud-release-tests` |
| `dev-collector` | `cargo xtask dev-collector` |
| `kind-cluster-delete` | `cargo xtask kind-cluster-delete` |
| `lane-start` | `cargo xtask lane-start` |
| `cargo-cranelift` | `cargo xtask cranelift` |
| `wamn-bench` | `cargo xtask bench` |
| `bench/local.sh` | `cargo xtask bench-local` |
| `bench/tier.sh` | `cargo xtask bench-tier` |
| `bench/pg-minimum/gcp.sh` | `cargo xtask bench-pg-minimum` |
| Benchmark result scripts | `cargo xtask bench-result`, `bench-pg-result`, `bench-app-result`, `bench-database-delta`, `bench-profile-summary`, `bench-name-guests` |
| Installers for Helm, OpenTofu, and wash | `mise install` |

BuildKit imports and exports the image caches. The build command no longer hashes input files, copies old image tags, scans processes, or submits Cloud Build jobs.
A failed publication stops at that step. Earlier successful pushes remain published.
The package builder owns both role component builds. The HTTP role then uses the existing control-entry composer.

Benchmark commands retain their workloads and result fields. Run output defaults to a temporary directory.
The local benchmark keeps the caller's Cargo target directory. It uses the component builder for its guests.
Tooling tests were removed with the old scripts. Check tasks by using their commands. Live benchmarks and publication need their external resources.
