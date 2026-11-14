# Logic belongs to xtask.
set positional-arguments
default:
    @just --list

build-platform *args:
    cargo xtask build-platform "$@"

test-changes *args:
    cargo xtask test-changes "$@"

repo-lint *args:
    cargo xtask repo-lint "$@"

compile-packages-alone *args:
    cargo xtask compile-packages-alone "$@"

contract-diff *args:
    cargo xtask contract-diff "$@"

wasmcloud-release-tests *args:
    cargo xtask wasmcloud-release-tests "$@"

dev-collector root:
    cargo xtask dev-collector "$@"

kind-cluster-delete cluster:
    cargo xtask kind-cluster-delete "$@"

lane-start *args:
    cargo xtask lane-start "$@"

cranelift *args:
    cargo xtask cranelift -- "$@"

bench *args:
    cargo xtask bench "$@"

submodule-pins *args:
    cargo xtask submodule-pins "$@"

netsuite-live *args:
    cargo xtask netsuite-live "$@"
