//! Provenance of the Wasm components and native binaries the Docker graph builds.

use std::path::Path;

use super::{Problems, read};

pub(super) fn check(root: &Path, problems: &mut Problems) {
    let (Some(dockerfile), Some(dockerignore)) = (
        read(root, "Dockerfile", problems),
        read(root, ".dockerignore", problems),
    ) else {
        return;
    };
    embedded_components_come_from_the_locked_builder(&dockerfile, &dockerignore, problems);
    native_images_have_package_scoped_build_stages(&dockerfile, problems);
    build_graph_has_one_source_stage_and_no_retired_leg(&dockerfile, problems);
    if let (Some(root_manifest), Some(apps_manifest)) = (
        read(root, "Cargo.toml", problems),
        read(root, "apps/Cargo.toml", problems),
    ) {
        workspace_paths_are_inside_the_copy_set(
            &dockerfile,
            &root_manifest,
            &apps_manifest,
            problems,
        );
    }
}

/// The contents of one Dockerfile stage, or a violation when it is missing.
fn stage<'a>(source: &'a str, name: &str, problems: &mut Problems) -> Option<&'a str> {
    let marker = format!(" AS {name}\n");
    let Some((_, contents)) = source.split_once(&marker) else {
        problems.push(format!("Dockerfile must define stage {name}"));
        return None;
    };
    if contents.starts_with("FROM ") {
        return Some("");
    }
    Some(
        contents
            .split_once("\nFROM ")
            .map_or(contents, |(stage, _)| stage),
    )
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
fn stage_caches(contents: &str, owner: &str, stage_name: &str, problems: &mut Problems) {
    let mounts: Vec<_> = contents
        .lines()
        .filter(|line| line.contains("--mount=type=cache"))
        .collect();
    problems.require(mounts.len() == 3, || {
        format!("{owner} must mount exactly three caches")
    });
    for id in ["id=wamn-root-cargo-registry", "id=wamn-root-cargo-git"] {
        problems.require(
            mounts
                .iter()
                .any(|line| line.contains(id) && line.contains("sharing=shared")),
            || format!("{owner} lost shared download cache {id}"),
        );
    }
    let target = format!("id=wamn-root-target-{stage_name},target=/build/target");
    problems.require(
        mounts
            .iter()
            .any(|line| line.contains(&target) && line.contains("sharing=locked")),
        || format!("{owner} must own the locked target cache {target}"),
    );
}

fn embedded_components_come_from_the_locked_builder(
    dockerfile: &str,
    dockerignore: &str,
    problems: &mut Problems,
) {
    let mut expected = vec![
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
    for line in dockerfile.lines().map(str::trim) {
        if !line.starts_with("COPY ") || !line.contains(".wasm") {
            continue;
        }
        if !line.starts_with("COPY --from=component-builder ") {
            problems.push(format!("embedded Wasm bypasses component-builder: {line}"));
            continue;
        }
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.len() != 4 {
            problems.push(format!("component COPY must have one source: {line}"));
            continue;
        }
        actual.push((fields[2], fields[3]));
    }

    actual.sort_unstable();
    expected.sort_unstable();
    problems.require(actual == expected, || {
        format!("embedded component list drifted: {actual:?}")
    });

    // ea71c1c4 (wamn-0h0g.26.7.2) deleted the last embedded execution guest, so
    // every remaining component ships to the gates image alone. The service
    // images carry native binaries only; a component COPY appearing in one is a
    // regression, and `stage` reports a stage name that disappears.
    for image_stage in ["host", "identity", "ctl", "scenario-worker", "cdc-reader"] {
        if let Some(contents) = stage(dockerfile, image_stage, problems) {
            problems.require(!contents.contains(".wasm"), || {
                format!("{image_stage} service image must not carry component bytes")
            });
        }
    }
    if let Some(gates_stage) = stage(dockerfile, "gates", problems) {
        for (source, destination) in &expected {
            problems.require(
                gates_stage.contains(&format!(
                    "COPY --from=component-builder {source} {destination}"
                )),
                || format!("gates image lost embedded component {destination}"),
            );
        }
    }

    for required in [
        "FROM component-toolchain AS component-builder",
        "COPY apps /build/apps",
        "rustup target add --toolchain 1.98.0 wasm32-wasip2",
        "cargo +1.98.0 build --locked --release --target wasm32-wasip2",
    ] {
        problems.require(dockerfile.contains(required), || {
            format!("Dockerfile lost the locked component build line `{required}`")
        });
    }
    problems.require(
        dockerignore.lines().any(|line| line == "/apps/target"),
        || ".dockerignore must exclude /apps/target".to_owned(),
    );
}

fn native_images_have_package_scoped_build_stages(dockerfile: &str, problems: &mut Problems) {
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

    problems.require(!dockerfile.contains("cargo chef"), || {
        "a cook stage writes its output into the cache mount its build stage \
         already reads, so it cached nothing and only took the shared lock"
            .to_owned()
    });

    for (stage_name, package, image_stage, outputs) in packages {
        let build_name = format!("build-{stage_name}");
        problems.require(
            dockerfile.contains(&format!("FROM root-source AS {build_name}")),
            || format!("{build_name} must build from the one source stage"),
        );
        if let Some(build) = stage(dockerfile, &build_name, problems) {
            let selected = selected_packages(build);
            problems.require(
                !selected.is_empty() && selected.iter().all(|selected| *selected == package),
                || format!("{build_name} may compile only {package}, got {selected:?}"),
            );
            stage_caches(build, &build_name, stage_name, problems);
        }

        let Some(image) = stage(dockerfile, image_stage, problems) else {
            continue;
        };
        let native_copies: Vec<_> = image
            .lines()
            .filter(|line| line.contains("/native-output/"))
            .collect();
        problems.require(native_copies.len() == outputs.len(), || {
            format!("{image_stage} native output list drifted: {native_copies:?}")
        });
        for output in outputs {
            problems.require(
                image.contains(&format!(
                    "COPY --from={build_name} /native-output/{output} /usr/local/bin/{output}"
                )),
                || format!("{image_stage} must copy {output} only from {build_name}"),
            );
        }
    }
}

fn build_graph_has_one_source_stage_and_no_retired_leg(dockerfile: &str, problems: &mut Problems) {
    if let Some(source) = stage(dockerfile, "root-source", problems) {
        for required in ["COPY Cargo.toml Cargo.lock ./", "COPY apps ./apps"] {
            problems.require(source.contains(required), || {
                format!("root-source lost `{required}`")
            });
        }
    }
    problems.require(dockerfile.contains("FROM toolchain AS root-source"), || {
        "root-source must build from the toolchain stage".to_owned()
    });
    for retired in ["recipe", "AS root-cook", " AS builder\n", "--from=builder"] {
        problems.require(!dockerfile.contains(retired), || {
            format!("retired Docker stage returned: {retired:?}")
        });
    }
    problems.require(
        dockerfile.contains("id=wamn-component-target,target=/build/apps/target"),
        || "the component build lost its own target cache".to_owned(),
    );

    if let Some(gates) = stage(dockerfile, "build-gates", problems) {
        let selected = selected_packages(gates);
        problems.require(selected == ["wamn-gates"], || {
            format!("build-gates may compile only wamn-gates, got {selected:?}")
        });
        stage_caches(gates, "build-gates", "gates", problems);
    }

    // One target cache each, and no stage left holding the old shared one.
    problems.require(!dockerfile.contains("id=wamn-root-target,"), || {
        "a stage still mounts the shared root target cache".to_owned()
    });
    let targets: std::collections::BTreeSet<_> = dockerfile
        .lines()
        .filter_map(|line| line.split_once("id=wamn-root-target-"))
        .filter_map(|(_, rest)| rest.split(',').next())
        .collect();
    problems.require(targets.len() == 6, || {
        format!("every native build stage owns one target cache, got {targets:?}")
    });

    // wamn-0h0g.15.139 audited these against the bare-ordinary-English rule and
    // KEPT THEM BARE. `jco` and `wac` are three characters matched over the whole
    // lowercased Dockerfile, which is the fragile shape on its face — but they are
    // tool names, not words: neither is a substring of any English word, and a
    // Dockerfile spells a tool at a command position, so the only way either
    // appears is the retired leg returning. If one ever does collide, pin it to
    // its command shape (`jco `, `wac `) rather than deleting the row.
    let lowercase = dockerfile.to_ascii_lowercase();
    for retired in [
        "builder-svc",
        "jco",
        "wac",
        "custom-node",
        "services/builder",
    ] {
        problems.require(!lowercase.contains(retired), || {
            format!("retired Docker leg returned: {retired}")
        });
    }
}

// wamn-at27. A stage copies named directories, so a workspace member or path
// dependency outside them breaks the image build with no source-level signal.
// 7e53e7f3b declared `wamn-test-postgres` in `apps/Cargo.toml` by the path
// `../test-support/postgres`; the component stage copied no `test-support`, and
// `docker build --target gates` then failed reading
// `/build/test-support/postgres/Cargo.toml` while every `tools/repo-lint` leg
// stayed green. Cargo reads every workspace manifest before it builds one guest,
// so a directory that no guest links still has to be present. This lint compares
// the two workspace manifests against the two stages that carry their sources.

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
fn members_array(manifest: &str) -> Option<&str> {
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
        return tail.split_once(']').map(|(members, _)| members);
    }
    None
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
        if let Some((path, _)) = tail.split_once('"') {
            paths.push(path);
        }
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
            let sources = fields.len().saturating_sub(1);
            fields.into_iter().take(sources)
        })
        .map(|source| source.trim_start_matches("./"))
        .collect()
}

fn workspace_paths_are_inside_the_copy_set(
    dockerfile: &str,
    root_manifest: &str,
    apps_manifest: &str,
    problems: &mut Problems,
) {
    for (manifest, base, stage_name) in [
        (root_manifest, "", "root-source"),
        (apps_manifest, "apps", "component-toolchain"),
    ] {
        let Some(contents) = stage(dockerfile, stage_name, problems) else {
            continue;
        };
        let copied = copied_sources(contents);
        if copied.is_empty() {
            problems.push(format!("{stage_name} copies no source"));
            continue;
        }
        let Some(members) = members_array(manifest) else {
            problems.push(format!(
                "the workspace manifest read by {stage_name} declares no members"
            ));
            continue;
        };
        let declared = members
            .split('"')
            .skip(1)
            .step_by(2)
            .chain(declared_paths(manifest));
        for entry in declared {
            let path = resolve(base, entry);
            problems.require(
                copied
                    .iter()
                    .any(|source| path == *source || path.starts_with(&format!("{source}/"))),
                || {
                    format!(
                        "{stage_name} reads {path}, which none of its COPY sources {copied:?} carries"
                    )
                },
            );
        }
    }
}
