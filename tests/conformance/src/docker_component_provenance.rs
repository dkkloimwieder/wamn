//! Static provenance guard for Wasm components embedded by the Docker graph.

const DOCKERFILE: &str = include_str!("../../../Dockerfile");
const DOCKERIGNORE: &str = include_str!("../../../.dockerignore");

fn stage(name: &str) -> &str {
    let marker = format!(" AS {name}\n");
    let (_, contents) = DOCKERFILE
        .split_once(&marker)
        .unwrap_or_else(|| panic!("Dockerfile must define stage {name}"));
    contents
        .split_once("\nFROM ")
        .map_or(contents, |(stage, _)| stage)
}

fn selected_packages(contents: &str) -> Vec<&str> {
    let words: Vec<_> = contents.split_ascii_whitespace().collect();
    words
        .windows(2)
        .filter_map(|pair| (pair[0] == "-p").then_some(pair[1]))
        .collect()
}

fn assert_shared_locked_caches(contents: &str, owner: &str) {
    let mounts: Vec<_> = contents
        .lines()
        .filter(|line| line.contains("--mount=type=cache"))
        .collect();
    assert_eq!(mounts.len(), 3, "{owner} must mount exactly three caches");
    for id in [
        "id=wamn-root-cargo-registry",
        "id=wamn-root-cargo-git",
        "id=wamn-root-target",
    ] {
        assert!(
            mounts
                .iter()
                .any(|line| line.contains(id) && line.contains("sharing=locked")),
            "{owner} lost shared locked cache {id}"
        );
    }
}

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

    assert!(DOCKERFILE.contains("FROM component-toolchain AS component-builder"));
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
fn retained_native_images_have_package_scoped_cook_and_build_stages() {
    let packages = [
        ("host", "wamn-host", "host", &["wamn-host"][..]),
        (
            "executor",
            "wamn-executor",
            "run-worker",
            &["wamn-run-worker"][..],
        ),
        (
            "scenario-worker",
            "wamn-scenario-worker",
            "scenario-worker",
            &["wamn-scenario-worker"][..],
        ),
        ("ctl", "wamn-ctl", "ctl", &["wamn-ctl", "wamn-ctl-ops"][..]),
        (
            "dispatcher",
            "wamn-dispatcher",
            "dispatcher",
            &["wamn-dispatcher"][..],
        ),
        ("waker", "wamn-waker", "waker", &["wamn-waker"][..]),
        (
            "cdc-reader",
            "wamn-cdc-reader",
            "cdc-reader",
            &["wamn-cdc-reader"][..],
        ),
    ];

    assert_eq!(
        DOCKERFILE.matches("cargo chef prepare").count(),
        1,
        "the native graph must have exactly one shared planner recipe"
    );
    assert_eq!(
        DOCKERFILE.matches("cargo chef cook").count(),
        packages.len(),
        "only the seven retained native package cooks may exist"
    );

    for (stage_name, package, image_stage, outputs) in packages {
        let cook_name = format!("cook-{stage_name}");
        let cook = stage(&cook_name);
        assert!(
            DOCKERFILE.contains(&format!("FROM root-recipe AS {cook_name}")),
            "{cook_name} must consume the one shared recipe"
        );
        assert_eq!(
            selected_packages(cook),
            [package],
            "{cook_name} must select exactly its top-level package closure"
        );
        assert_shared_locked_caches(cook, &cook_name);

        let build_name = format!("build-{stage_name}");
        let build = stage(&build_name);
        assert!(
            DOCKERFILE.contains(&format!("FROM {cook_name} AS {build_name}")),
            "{build_name} must follow its matching cook"
        );
        assert!(build.contains("COPY --from=root-source /build /build"));
        let selected = selected_packages(build);
        assert!(
            !selected.is_empty() && selected.iter().all(|selected| *selected == package),
            "{build_name} may compile only {package}, got {selected:?}"
        );
        assert_shared_locked_caches(build, &build_name);

        let image = stage(image_stage);
        let native_copies: Vec<_> = image
            .lines()
            .filter(|line| line.contains("/native-output/"))
            .collect();
        assert_eq!(
            native_copies.len(),
            outputs.len(),
            "{image_stage} native output inventory drifted: {native_copies:?}"
        );
        for output in outputs {
            assert!(
                image.contains(&format!(
                    "COPY --from={build_name} /native-output/{output} /usr/local/bin/{output}"
                )),
                "{image_stage} must copy {output} only from {build_name}"
            );
        }
    }
}

#[test]
fn build_graph_has_no_shared_or_retired_cook_leg() {
    assert!(DOCKERFILE.contains("cargo install cargo-chef --version 0.1.77 --locked"));
    assert!(DOCKERFILE.contains("COPY Cargo.toml Cargo.lock ./"));
    assert!(
        DOCKERFILE.contains("COPY --from=root-planner /build/root-recipe.json ./root-recipe.json")
    );
    assert!(!DOCKERFILE.contains("component-recipe.json"));
    assert!(!DOCKERFILE.contains("AS root-cook"));
    assert!(!DOCKERFILE.contains(" AS builder\n"));
    assert!(!DOCKERFILE.contains("--from=builder"));
    assert!(DOCKERFILE.contains("id=wamn-root-target,target=/build/target"));
    assert!(DOCKERFILE.contains("id=wamn-component-target,target=/build/components/target"));

    let gates = stage("build-gates");
    assert_eq!(selected_packages(gates), ["wamn-gates"]);
    assert_shared_locked_caches(gates, "build-gates");

    for retired in [
        "builder-svc",
        "jco",
        "wac",
        "custom-node",
        "services/builder",
    ] {
        assert!(
            !DOCKERFILE.to_ascii_lowercase().contains(retired),
            "retired Docker leg returned: {retired}"
        );
    }
}
