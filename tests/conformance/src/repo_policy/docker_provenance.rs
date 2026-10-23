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
    native_images_share_one_build_stage(&dockerfile, problems);
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

/// One locked target cache per checkout, and the download caches shared.
///
/// The native stages share one target cache within a checkout (wamn-i6h97).
/// The checkout key keeps other checkouts from overwriting it (wamn-gc3sl.1).
/// Edge and cases keep their own per-checkout target caches.
fn stage_caches(contents: &str, owner: &str, stage_name: &str, problems: &mut Problems) {
    let mounts: Vec<_> = contents
        .lines()
        .filter(|line| line.contains("--mount=type=cache"))
        .collect();
    problems.require(mounts.len() == 3, || {
        format!("{owner} must mount exactly three caches")
    });
    for id in [
        "id=wamn-root-cargo-registry,target=/usr/local/cargo/registry",
        "id=wamn-root-cargo-git,target=/usr/local/cargo/git",
    ] {
        problems.require(
            mounts
                .iter()
                .any(|line| line.contains(id) && line.contains("sharing=locked")),
            || format!("{owner} lost shared download cache {id}"),
        );
    }
    let target =
        format!("id=wamn-root-target-{stage_name}-${{WAMN_CHECKOUT_KEY}},target=/build/target");
    problems.require(
        mounts
            .iter()
            .any(|line| line.contains(&target) && line.contains("sharing=locked")),
        || format!("{owner} must mount the locked target cache {target}"),
    );
}

fn embedded_components_come_from_the_locked_builder(
    dockerfile: &str,
    dockerignore: &str,
    problems: &mut Problems,
) {
    let gates = [
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
    // The roles image exports the control-only entry that control-entry
    // composes from the pinned route guest (wamn-l5sus.3).
    let roles = [
        (
            "control-entry",
            "/component-output/control_http.wasm",
            "/http_route.wasm",
        ),
        (
            "component-builder",
            "/component-output/materializer.wasm",
            "/materializer.wasm",
        ),
    ];
    // Publish composes the route guest with each application, so the ctl
    // image carries the pinned route guest and nothing else (wamn-l5sus.2).
    let ctl_route = "COPY --from=component-builder /component-output/http_route.wasm \
                     /usr/local/share/wamn/http_route.wasm";
    let mut expected: Vec<_> = gates
        .iter()
        .map(|(source, destination)| ("component-builder", *source, *destination))
        .chain(roles)
        .chain([(
            "component-builder",
            "/component-output/http_route.wasm",
            "/usr/local/share/wamn/http_route.wasm",
        )])
        .collect();

    let mut actual = Vec::new();
    for line in dockerfile.lines().map(str::trim) {
        if !line.starts_with("COPY ") || !line.contains(".wasm") {
            continue;
        }
        let builder = ["component-builder", "control-entry"]
            .into_iter()
            .find(|builder| line.starts_with(&format!("COPY --from={builder} ")));
        let Some(builder) = builder else {
            problems.push(format!("embedded Wasm bypasses component-builder: {line}"));
            continue;
        };
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.len() != 4 {
            problems.push(format!("component COPY must have one source: {line}"));
            continue;
        }
        actual.push((builder, fields[2], fields[3]));
    }

    actual.sort_unstable();
    expected.sort_unstable();
    problems.require(actual == expected, || {
        format!("embedded component list drifted: {actual:?}")
    });

    // ea71c1c4 (wamn-0h0g.26.7.2) deleted the last embedded execution guest, so
    // service images carry native binaries only. Gates embed test fixtures,
    // and the roles stage exports the platform role components (53e4214c2).
    // `stage` reports a stage name that disappears.
    for image_stage in ["host", "identity", "ctl", "scenario-worker", "cdc-reader"] {
        if let Some(contents) = stage(dockerfile, image_stage, problems) {
            let contents = if image_stage == "ctl" {
                problems.require(contents.contains(ctl_route), || {
                    "ctl image lost the pinned route guest".to_owned()
                });
                contents.replacen(ctl_route, "", 1)
            } else {
                contents.to_owned()
            };
            problems.require(!contents.contains(".wasm"), || {
                format!("{image_stage} service image must not carry component bytes")
            });
        }
    }
    if let Some(contents) = stage(dockerfile, "gates", problems) {
        for (source, destination) in gates {
            problems.require(
                contents.contains(&format!(
                    "COPY --from=component-builder {source} {destination}"
                )),
                || format!("gates image lost embedded component {destination}"),
            );
        }
    }
    if let Some(contents) = stage(dockerfile, "roles", problems) {
        for (builder, source, destination) in roles {
            problems.require(
                contents.contains(&format!("COPY --from={builder} {source} {destination}")),
                || format!("roles image lost embedded component {destination}"),
            );
        }
    }

    for required in [
        "FROM component-toolchain AS component-builder",
        "FROM component-builder AS control-entry",
        "RUN wamn-component-composer --control-route /component-output/http_route.wasm \
         --output /component-output/control_http.wasm",
        "COPY apps /build/apps",
        "rustup target add --toolchain 1.99.0 wasm32-wasip2",
        "cargo +1.99.0 build --locked --release --target wasm32-wasip2",
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

fn native_images_share_one_build_stage(dockerfile: &str, problems: &mut Problems) {
    let packages = [
        "wamn-host",
        "wamn-runtime",
        "wamn-ctl",
        "wamn-scenario-worker",
        "wamn-cdc-reader",
        "wamn-identity",
        "wamn-gates",
        "wamn-component-composer",
    ];
    let outputs = [
        "wamn-host",
        "docker-credential-wamn",
        "wamn-ctl",
        "wamn-ctl-ops",
        "wamn",
        "wamn-scenario-worker",
        "wamn-cdc-reader",
        "wamn-identity",
        "wamn-component-composer",
        "wamn-gates",
    ];
    problems.require(!dockerfile.contains("cargo chef"), || {
        "a cook stage writes its output into the cache mount its build stage \
         already reads, so it cached nothing and only took the shared lock"
            .to_owned()
    });
    problems.require(
        dockerfile.matches(" AS build-native\n").count() == 1
            && dockerfile.contains("FROM root-source AS build-native\n"),
        || "one build-native stage must build from the one source stage".to_owned(),
    );
    if let Some(build) = stage(dockerfile, "build-native", problems) {
        let invocation = format!(
            "cargo build --locked --release -p {}",
            packages.join(" -p ")
        );
        // The ops feature reaches shared code, so the shipped wamn-ctl builds
        // without it and only wamn-ctl-ops gets a second invocation
        // (services/ctl/src/main.rs).
        let ops = "cargo build --locked --release -p wamn-ctl --features ops --bin wamn-ctl-ops";
        let commands = build.replace("\\\n", " ");
        let cargo_commands: Vec<_> = commands
            .split("&&")
            .map(str::trim)
            .filter(|command| command.starts_with("cargo "))
            .collect();
        problems.require(cargo_commands == [invocation.as_str(), ops], || {
            format!("build-native must run `{invocation}`, then `{ops}`")
        });
        for required in ["ARG WAMN_CHECKOUT_KEY", "test -n \"$WAMN_CHECKOUT_KEY\""] {
            problems.require(build.contains(required), || {
                format!("build-native lost `{required}`")
            });
        }
        stage_caches(build, "build-native", "native", problems);
        for output in outputs {
            problems.require(
                build.contains(&format!(
                    "install -D -m 0755 target/release/{output} /native-output/{output}"
                )),
                || format!("build-native must install {output} into /native-output"),
            );
        }
    }

    for (image_stage, outputs) in [
        ("host", &["wamn-host", "docker-credential-wamn"][..]),
        ("identity", &["wamn-identity"][..]),
        ("scenario-worker", &["wamn-scenario-worker"][..]),
        (
            "ctl",
            &["wamn-ctl", "wamn-ctl-ops", "wamn-scenario-worker"][..],
        ),
        ("cdc-reader", &["wamn-cdc-reader"][..]),
        (
            "gates",
            &["wamn-gates", "wamn-ctl", "wamn-ctl-ops", "wamn-cdc-reader"][..],
        ),
    ] {
        problems.require(
            !dockerfile.contains(&format!(" AS build-{image_stage}\n")),
            || format!("retired build-{image_stage} stage must use build-native"),
        );
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
                    "COPY --from=build-native /native-output/{output} /usr/local/bin/{output}"
                )),
                || format!("{image_stage} must copy {output} only from build-native"),
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
        dockerfile
            .contains("id=wamn-component-target-${WAMN_CHECKOUT_KEY},target=/build/apps/target"),
        || "the component build lost its own target cache".to_owned(),
    );

    // Native builds share one stage; edge and cases stay per checkout.
    // No stage keeps the retired shared root target cache.
    problems.require(!dockerfile.contains("id=wamn-root-target,"), || {
        "a stage still mounts the shared root target cache".to_owned()
    });
    for stage_name in ["edge", "cases"] {
        let build_name = format!("build-{stage_name}");
        if let Some(build) = stage(dockerfile, &build_name, problems) {
            stage_caches(build, &build_name, stage_name, problems);
        }
    }

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
