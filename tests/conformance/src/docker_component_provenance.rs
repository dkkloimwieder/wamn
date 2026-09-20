//! Static provenance guard for Wasm components embedded by the Docker graph.

const DOCKERFILE: &str = include_str!("../../../Dockerfile");
const DOCKERIGNORE: &str = include_str!("../../../.dockerignore");

fn stage<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!(" AS {name}\n");
    let (_, contents) = source
        .split_once(&marker)
        .unwrap_or_else(|| panic!("Dockerfile must define stage {name}"));
    if contents.starts_with("FROM ") {
        return "";
    }
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

/// One target cache per build stage, and the download caches shared.
///
/// Seventeen stages used to lock one target cache, so they compiled one at a
/// time and deleted each other's workspace output. A stage that owns its target
/// waits for nobody; the registry and Git caches are downloads, which Cargo
/// already locks per package (wamn-szr0).
fn assert_stage_caches(contents: &str, owner: &str, stage_name: &str) {
    let mounts: Vec<_> = contents
        .lines()
        .filter(|line| line.contains("--mount=type=cache"))
        .collect();
    assert_eq!(mounts.len(), 3, "{owner} must mount exactly three caches");
    for id in ["id=wamn-root-cargo-registry", "id=wamn-root-cargo-git"] {
        assert!(
            mounts
                .iter()
                .any(|line| line.contains(id) && line.contains("sharing=shared")),
            "{owner} lost shared download cache {id}"
        );
    }
    let target = format!("id=wamn-root-target-{stage_name},target=/build/target");
    assert!(
        mounts
            .iter()
            .any(|line| line.contains(&target) && line.contains("sharing=locked")),
        "{owner} must own the locked target cache {target}"
    );
}

#[test]
fn every_embedded_component_comes_from_the_locked_builder() {
    assert_eq!(
        stage(
            "FROM base AS gates\nFROM base AS other\nCOPY component.wasm /bench/component.wasm\n",
            "gates",
        ),
        "",
        "an empty stage must not inherit the next stage's component bytes"
    );
    assert_eq!(
        stage(
            "FROM base AS gates\nRUN true\nFROM base AS other\nCOPY component.wasm /bench/component.wasm\n",
            "gates",
        ),
        "RUN true",
        "the next stage must also bound a nonempty stage"
    );
    let expected = [
        ("/component-output/busyloop.wasm", "/bench/busyloop.wasm"),
        (
            "/component-output/connection_http_standard.wasm",
            "/bench/connection-http-standard.wasm",
        ),
        (
            "/component-output/http_route.wasm",
            "/bench/http-route.wasm",
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
    assert_eq!(actual, expected, "embedded component list drifted");

    // ea71c1c4 (wamn-0h0g.26.7.2) deleted the last embedded execution guest, so
    // every remaining component ships to the gates image alone. The service
    // images carry native binaries only; a component COPY appearing in one is a
    // regression, and `stage` fails closed when a stage name disappears.
    for image_stage in [
        "host",
        "identity",
        "executor",
        "ctl",
        "dispatcher",
        "scenario-worker",
        "cdc-reader",
        "waker",
    ] {
        assert!(
            !stage(DOCKERFILE, image_stage).contains(".wasm"),
            "{image_stage} service image must not carry component bytes"
        );
    }
    let gates_stage = stage(DOCKERFILE, "gates");
    for (source, destination) in expected {
        assert!(
            gates_stage.contains(&format!(
                "COPY --from=component-builder {source} {destination}"
            )),
            "gates image lost embedded component {destination}"
        );
    }

    assert!(DOCKERFILE.contains("FROM component-toolchain AS component-builder"));
    assert!(DOCKERFILE.contains("COPY apps /build/apps"));
    assert!(DOCKERFILE.contains("rustup target add --toolchain 1.98.0 wasm32-wasip2"));
    assert!(DOCKERFILE.contains("cargo +1.98.0 build --locked --release --target wasm32-wasip2"));
    assert!(DOCKERIGNORE.lines().any(|line| line == "/apps/target"));
}

#[test]
fn retained_native_images_have_package_scoped_build_stages() {
    let packages = [
        ("host", "wamn-host", "host", &["wamn-host"][..]),
        (
            "identity",
            "wamn-identity",
            "identity",
            &["wamn-identity"][..],
        ),
        (
            "scenario-worker",
            "wamn-scenario-worker",
            "scenario-worker",
            &["wamn-scenario-worker"][..],
        ),
        ("ctl", "wamn-ctl", "ctl", &["wamn-ctl", "wamn-ctl-ops"][..]),
        (
            "cdc-reader",
            "wamn-cdc-reader",
            "cdc-reader",
            &["wamn-cdc-reader"][..],
        ),
    ];

    assert!(
        !DOCKERFILE.contains("cargo chef"),
        "a cook stage writes its output into the cache mount its build stage \
         already reads, so it cached nothing and only took the shared lock"
    );

    for (stage_name, package, image_stage, outputs) in packages {
        let build_name = format!("build-{stage_name}");
        let build = stage(DOCKERFILE, &build_name);
        assert!(
            DOCKERFILE.contains(&format!("FROM root-source AS {build_name}")),
            "{build_name} must build from the one source stage"
        );
        let selected = selected_packages(build);
        assert!(
            !selected.is_empty() && selected.iter().all(|selected| *selected == package),
            "{build_name} may compile only {package}, got {selected:?}"
        );
        assert_stage_caches(build, &build_name, stage_name);

        let image = stage(DOCKERFILE, image_stage);
        let native_copies: Vec<_> = image
            .lines()
            .filter(|line| line.contains("/native-output/"))
            .collect();
        assert_eq!(
            native_copies.len(),
            outputs.len(),
            "{image_stage} native output list drifted: {native_copies:?}"
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
fn build_graph_has_one_source_stage_and_no_retired_leg() {
    let source = stage(DOCKERFILE, "root-source");
    assert!(source.contains("COPY Cargo.toml Cargo.lock ./"));
    assert!(source.contains("COPY apps ./apps"));
    assert!(DOCKERFILE.contains("FROM toolchain AS root-source"));
    assert!(!DOCKERFILE.contains("recipe"));
    assert!(!DOCKERFILE.contains("AS root-cook"));
    assert!(!DOCKERFILE.contains(" AS builder\n"));
    assert!(!DOCKERFILE.contains("--from=builder"));
    assert!(DOCKERFILE.contains("id=wamn-component-target,target=/build/apps/target"));

    let gates = stage(DOCKERFILE, "build-gates");
    assert_eq!(selected_packages(gates), ["wamn-gates"]);
    assert_stage_caches(gates, "build-gates", "gates");

    // One target cache each, and no stage left holding the old shared one.
    assert!(!DOCKERFILE.contains("id=wamn-root-target,"));
    let targets: std::collections::BTreeSet<_> = DOCKERFILE
        .lines()
        .filter_map(|line| line.split_once("id=wamn-root-target-"))
        .map(|(_, rest)| rest.split(',').next().expect("a cache id ends"))
        .collect();
    assert_eq!(
        targets.len(),
        9,
        "every native build stage owns one target cache, got {targets:?}"
    );

    // wamn-0h0g.15.139 audited these against the bare-ordinary-English rule and
    // KEPT THEM BARE. `jco` and `wac` are three characters matched over the whole
    // lowercased Dockerfile, which is the fragile shape on its face — but they are
    // tool names, not words: neither is a substring of any English word, and a
    // Dockerfile spells a tool at a command position, so the only way either
    // appears is the retired leg returning. That is the `InlineExecutionDriver`
    // side of the wamn-0h0g.15.131 split, not the `flowrunner` side. If one ever
    // does collide, pin it to its command shape (`jco `, `wac `) rather than
    // deleting the row.
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

// wamn-at27. A stage copies named directories, so a workspace member or path
// dependency outside them breaks the image build with no source-level signal.
// 7e53e7f3b declared `wamn-test-postgres` in `apps/Cargo.toml` by the path
// `../test-support/postgres`; the component stage copied no `test-support`, and
// `docker build --target gates` then failed reading
// `/build/test-support/postgres/Cargo.toml` while every `tools/repo-lint` leg
// stayed green. Cargo reads every workspace manifest before it builds one guest,
// so a directory that no guest links still has to be present. This case compares
// the two workspace manifests against the two stages that carry their sources.
// It reads text only: an image build is far too slow for a gate run.

const ROOT_MANIFEST: &str = include_str!("../../../Cargo.toml");
const APPS_MANIFEST: &str = include_str!("../../../apps/Cargo.toml");

/// Whether the character before a key is part of a longer word.
///
/// `default-members` and `jmespath` both end in a key this reader looks for.
fn inside_word(manifest: &str, start: usize) -> bool {
    manifest[..start]
        .chars()
        .next_back()
        .is_some_and(|last| last.is_alphanumeric() || last == '-' || last == '_')
}

/// The text between the brackets of the `members` array of a workspace manifest.
fn members_array(manifest: &str) -> &str {
    let mut offset = 0;
    while let Some(found) = manifest[offset..].find("members") {
        let start = offset + found;
        offset = start + "members".len();
        if inside_word(manifest, start) {
            continue;
        }
        let Some(tail) = manifest[offset..].trim_start().strip_prefix('=') else {
            continue;
        };
        let Some(tail) = tail.trim_start().strip_prefix('[') else {
            continue;
        };
        return tail
            .split_once(']')
            .expect("the members array must close")
            .0;
    }
    panic!("a workspace manifest must declare its members");
}

/// Every `path = "..."` value of a manifest, in declaration order.
fn declared_paths(manifest: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut offset = 0;
    while let Some(found) = manifest[offset..].find("path") {
        let start = offset + found;
        offset = start + "path".len();
        if inside_word(manifest, start) {
            continue;
        }
        let Some(tail) = manifest[offset..].trim_start().strip_prefix('=') else {
            continue;
        };
        let Some(tail) = tail.trim_start().strip_prefix('"') else {
            continue;
        };
        paths.push(tail.split_once('"').expect("a path value must close").0);
    }
    paths
}

/// The repository-relative path that `relative` names when read from `base`.
fn resolve(base: &str, relative: &str) -> String {
    let mut segments: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            named => segments.push(named),
        }
    }
    segments.join("/")
}

/// The build-context sources of the plain `COPY` lines of one stage.
fn copied_sources(contents: &str) -> Vec<&str> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("COPY ") && !line.contains("--from="))
        .flat_map(|line| {
            let fields: Vec<_> = line.split_ascii_whitespace().skip(1).collect();
            let sources = fields.len() - 1;
            fields.into_iter().take(sources)
        })
        .map(|source| source.trim_start_matches("./"))
        .collect()
}

#[test]
fn every_workspace_path_a_build_stage_reads_is_inside_its_copy_set() {
    assert_eq!(
        resolve("apps", "../test-support/postgres"),
        "test-support/postgres",
        "a path dependency that leaves its workspace must resolve against the repository"
    );

    for (manifest, base, stage_name) in [
        (ROOT_MANIFEST, "", "root-source"),
        (APPS_MANIFEST, "apps", "component-toolchain"),
    ] {
        let copied = copied_sources(stage(DOCKERFILE, stage_name));
        assert!(!copied.is_empty(), "{stage_name} copies no source");
        let declared = members_array(manifest)
            .split('"')
            .skip(1)
            .step_by(2)
            .chain(declared_paths(manifest));
        for entry in declared {
            let path = resolve(base, entry);
            assert!(
                copied
                    .iter()
                    .any(|source| path == *source || path.starts_with(&format!("{source}/"))),
                "{stage_name} reads {path}, which none of its COPY sources {copied:?} carries"
            );
        }
    }
}
