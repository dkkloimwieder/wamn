//! Static provenance guard for Wasm components embedded by the Docker graph.

const DOCKERFILE: &str = include_str!("../../../Dockerfile");
const DOCKERIGNORE: &str = include_str!("../../../.dockerignore");

#[test]
fn every_embedded_component_comes_from_the_locked_builder() {
    let expected = [
        ("/component-output/busyloop.wasm", "/bench/busyloop.wasm"),
        (
            "/component-output/connection_http_standard.wasm",
            "/bench/connection-http-standard.wasm",
        ),
        ("/component-output/flow_http.wasm", "/bench/flow-http.wasm"),
        (
            "/component-output/flowrunner.wasm",
            "/bench/flowrunner.wasm",
        ),
        (
            "/component-output/flowrunner.wasm",
            "/components/flowrunner.wasm",
        ),
        (
            "/component-output/materializer.wasm",
            "/bench/materializer.wasm",
        ),
        ("/component-output/sockprobe.wasm", "/bench/sockprobe.wasm"),
    ];

    let mut actual = Vec::new();
    for line in DOCKERFILE.lines().map(str::trim) {
        if !line.starts_with("COPY ") || !line.contains(".wasm") {
            continue;
        }
        assert!(
            line.starts_with("COPY --from=component-builder "),
            "embedded Wasm bypasses component-builder: {line}"
        );
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        assert_eq!(
            fields.len(),
            4,
            "component COPY must have one source: {line}"
        );
        actual.push((fields[2], fields[3]));
    }

    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected, "embedded component inventory drifted");

    let host_stage = DOCKERFILE
        .split_once("FROM debian:trixie-slim AS host")
        .expect("host stage exists")
        .1
        .split_once("FROM debian:trixie-slim AS ctl")
        .expect("ctl follows host")
        .0;
    assert!(
        !host_stage.contains("flowrunner.wasm"),
        "admission-only host image must not carry execution bytes"
    );

    let run_worker_stage = DOCKERFILE
        .split_once("FROM debian:trixie-slim AS run-worker")
        .expect("run-worker stage exists")
        .1
        .split_once("FROM debian:trixie-slim AS scenario-worker")
        .expect("scenario-worker follows run-worker")
        .0;
    assert!(run_worker_stage.contains(
        "COPY --from=component-builder /component-output/flowrunner.wasm /components/flowrunner.wasm"
    ));

    let gates_stage = DOCKERFILE
        .split_once("FROM host AS gates")
        .expect("gates stage exists")
        .1;
    assert!(gates_stage.contains(
        "COPY --from=component-builder /component-output/flowrunner.wasm /bench/flowrunner.wasm"
    ));

    assert!(DOCKERFILE.contains("FROM component-cook AS component-builder"));
    assert!(DOCKERFILE.contains("COPY components /build/components"));
    assert!(DOCKERFILE.contains("rustup target add --toolchain 1.97.0 wasm32-wasip2"));
    assert!(DOCKERFILE.contains("cargo +1.97.0 build --locked --release --target wasm32-wasip2"));
    assert!(
        DOCKERIGNORE
            .lines()
            .any(|line| line == "/components/target")
    );
}

#[test]
fn dependency_caches_are_keyed_per_workspace() {
    assert!(DOCKERFILE.contains("cargo install cargo-chef --version 0.1.77 --locked"));
    assert!(DOCKERFILE.contains("COPY Cargo.toml Cargo.lock ./"));
    assert!(
        DOCKERFILE.contains("COPY --from=root-planner /build/root-recipe.json ./root-recipe.json")
    );
    assert!(DOCKERFILE.contains(
        "COPY --from=component-planner /build/components/component-recipe.json ./component-recipe.json"
    ));
    assert!(DOCKERFILE.contains("COPY .cargo/config.toml /build/.cargo/config.toml"));
    assert!(DOCKERFILE.contains("COPY --from=component-planner /build/crates /build/crates"));
    assert!(DOCKERFILE.contains("cargo chef cook --locked --release"));
    assert!(DOCKERFILE.contains("cargo +1.97.0 chef cook --locked --release"));
    assert!(DOCKERFILE.contains("id=wamn-root-target,target=/build/target"));
    assert!(DOCKERFILE.contains("id=wamn-component-target,target=/build/components/target"));
    assert!(DOCKERFILE.contains("install -d /native-output"));
    assert!(!DOCKERFILE.contains("COPY --from=builder /build/target/release/"));
}
