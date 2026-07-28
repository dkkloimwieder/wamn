//! Static provenance guard for Wasm components embedded by the Docker graph.

const DOCKERFILE: &str = include_str!("../../../Dockerfile");
const DOCKERIGNORE: &str = include_str!("../../../.dockerignore");

#[test]
fn every_embedded_component_comes_from_the_locked_builder() {
    let expected = [
        (
            "/component-output/api_gateway.wasm",
            "/bench/api-gateway.wasm",
        ),
        ("/component-output/busyloop.wasm", "/bench/busyloop.wasm"),
        (
            "/component-output/disposition_node.wasm",
            "/bench/disposition-node.wasm",
        ),
        (
            "/component-output/evaluate_specs.wasm",
            "/bench/evaluate-specs.wasm",
        ),
        (
            "/component-output/flow_composed.wasm",
            "/bench/flow-composed.wasm",
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
            "/component-output/flowrunner.wasm",
            "/components/flowrunner.wasm",
        ),
        (
            "/component-output/flowrunner.wasm",
            "/components/flowrunner.wasm",
        ),
        ("/component-output/hello.wasm", "/bench/hello.wasm"),
        ("/component-output/js-sample.wasm", "/bench/js-sample.wasm"),
        ("/component-output/logspewer.wasm", "/bench/logspewer.wasm"),
        (
            "/component-output/materializer.wasm",
            "/bench/materializer.wasm",
        ),
        ("/component-output/memhog.wasm", "/bench/memhog.wasm"),
        ("/component-output/node-ts.wasm", "/bench/node-ts.wasm"),
        ("/component-output/node_rs.wasm", "/bench/node-rs.wasm"),
        (
            "/component-output/normalize_receipt.wasm",
            "/bench/normalize-receipt.wasm",
        ),
        ("/component-output/pgprobe.wasm", "/bench/pgprobe.wasm"),
        (
            "/component-output/poc_webhook_f1.wasm",
            "/bench/poc-webhook-f1.wasm",
        ),
        (
            "/component-output/sample_node.wasm",
            "/bench/sample-node.wasm",
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

    assert!(DOCKERFILE.contains("FROM builder AS component-builder"));
    assert!(DOCKERFILE.contains("COPY components/Cargo.toml components/Cargo.lock ./components/"));
    assert!(DOCKERFILE.contains("rustup target add --toolchain 1.97.0 wasm32-wasip2"));
    assert!(DOCKERFILE.contains("cargo +1.97.0 install wac-cli --version 0.10.1 --locked"));
    assert!(DOCKERFILE.contains("cargo +1.97.0 build --locked --release --target wasm32-wasip2"));
    assert!(DOCKERFILE.contains("@bytecodealliance/jco@1.25.2"));
    assert!(DOCKERFILE.contains("@bytecodealliance/componentize-js@0.21.0"));
    assert!(DOCKERFILE.contains("@napi-rs/lzma-linux-x64-gnu@1.5.1"));
    assert!(
        DOCKERIGNORE
            .lines()
            .any(|line| line == "/components/target")
    );
    assert!(
        DOCKERIGNORE
            .lines()
            .any(|line| line == "/components/samples/node-ts/node-ts.wasm")
    );
}
