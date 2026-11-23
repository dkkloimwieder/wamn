# Crate structure

These rules apply to every crate in the root workspace and in `apps/`. Each review checks a change against them. Tests and `xtask` are outside the class rules, but the naming and code rules apply to them too.

## Classes

Every crate has one class: domain, implementation or binary.

- A domain crate holds types, traits and logic that calls only domain traits. It depends only on domain crates and plain value libraries, for example serde, uuid or ring.
- A domain crate links no client or runtime crate, directly or through another crate. Examples are sqlx, kube, k8s-openapi, wasmtime, `wasmtime-*`, wash-runtime, reqwest, hyper, async-nats, object_store and oci-client.
- An implementation crate implements domain traits for one technology: Postgres, Kubernetes, the wasmCloud host, HTTP, NATS, OCI or object storage. It depends on domain crates and on its own technology. It gets other technologies through domain traits.
- A domain crate never depends on an implementation crate.
- A binary (`services/*` and the CLI) holds wiring and process setup only. It creates the implementations and passes them to the domain logic. It depends on no other binary.
- Build support (`wamn-build`, `wamn-codegen`) links no Postgres, Kubernetes or wasm runtime. The caller supplies results or a trait.
- A guest links only plain domain crates, the guest WIT and guest support.

## Traits

- A trait lives in a domain crate. Its implementations live in implementation crates.
- A trait has a reason to exist: two implementations, or a technology on the other side. If a binary trait has one implementation and one caller, call the code directly.
- A test double is not a reason for a trait. A test that needs Postgres or NATS starts a real container.

## SQL and rows

- SQL lives only in `wamn-postgres` and `wamn-provision`.
- No domain type derives or implements a sqlx trait.
- At the query, bind and read plain values, for example `String`, `Uuid` or arrays. Make the domain value with its own constructor.
- If a record maps often or the mapping can fail, use a private row struct in `wamn-postgres`.

## Jobs and names

- A crate has one job. Its name fits everything that the crate holds.
- A crate name is singular and in plain words, with no abbreviations. A test crate can end in `-tests`.
- A directory name equals the crate name without the `wamn-` prefix. Inside `test-support/`, it also drops the `test-` prefix.

## Code

- Do not copy code. If two crates need the same code, it goes to the crate that owns the job.
- A crate declares no dependency that it does not use. `cargo xtask machete` finds them. An ignore entry carries a comment that gives the reason.
- No public item exists only for tests.
- `cargo xtask repo-lint-dependencies` names the crates of each rule and refuses a link that breaks one.
